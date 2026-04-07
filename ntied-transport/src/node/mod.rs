mod handle;
pub mod inner;
mod state;
mod transport;

pub use handle::{Connection, ConnectionRef, DatagramChannel, StreamChannel};

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::task::JoinHandle;

use tracing::info;

use crate::crypto::{KemPrivateKey, PeerId, PrivateKey};
use crate::relay::protocol::{PURPOSE_RELAY, RelayMessage};
use crate::wire::Handshake;

use state::*;
use transport::*;

pub(crate) const RECV_BUF_SIZE: usize = 4096;
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const PING_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DIRECT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Node {
    shared: Arc<Shared>,
    recv_task: JoinHandle<()>,
}

impl Node {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let shared = Arc::new(Shared {
            socket: socket.clone(),
            identity,
            state: std::sync::Mutex::new(TransportState {
                connections: HashMap::new(),
                pending_connects: HashMap::new(),
                accept_queue: VecDeque::new(),
                next_connection_id: {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    socket.local_addr().ok().hash(&mut h);
                    Instant::now().hash(&mut h);
                    (h.finish() >> 32) | 1
                },
            }),
            relay: TokioMutex::new(None),
            shutdown: AtomicBool::new(false),
            pending_close: std::sync::Mutex::new(Vec::new()),
            ping_counter: AtomicU32::new(1),
            accept_notify: Notify::new(),
            established_notify: Notify::new(),
            data_notify: Notify::new(),
            stream_notify: Notify::new(),
        });

        let weak = Arc::downgrade(&shared);
        let recv_task = tokio::spawn(recv_loop(weak));

        Ok(Self { shared, recv_task })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared.socket.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.shared.identity.public_key().peer_id()
    }

    /// Gracefully shut down the node. Stops recv loop and relay listener.
    /// All pending operations will return errors.
    pub async fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.accept_notify.notify_waiters();
        self.shared.data_notify.notify_waiters();
        self.shared.stream_notify.notify_waiters();
        self.shared.established_notify.notify_waiters();
        let mut relay = self.shared.relay.lock().await;
        *relay = None;
    }

    pub async fn is_relay_attached(&self) -> bool {
        self.shared.relay.lock().await.is_some()
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<Connection> {
        let (connection_id, init_bytes) = {
            let mut state = self.shared.state.lock().unwrap();
            let sid = state.next_connection_id;
            state.next_connection_id += 1;

            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let init_bytes = Handshake {
                initiator_connection_id: sid,
                kem_public_key: *eph_pk,
            }
            .encode();

            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    send_path: SendPath::Direct { addr },
                    relay_peer_id: None,
                },
            );

            (sid, init_bytes)
        };

        self.shared.socket.send_to(&init_bytes, addr).await?;
        wait_for_established(&self.shared, connection_id).await
    }

    /// Attach to a relay server. Connects to the relay, opens a PURPOSE_RELAY
    /// datagram channel, receives the Welcome message, and spawns a background
    /// task that listens for tunneled packets from other peers.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        let conn = self.connect(relay_addr).await?;
        let datagram = conn.open_datagram(PURPOSE_RELAY).await?;

        let welcome_data = datagram.recv().await?;
        match RelayMessage::decode(&welcome_data) {
            Some(RelayMessage::Welcome { external_addr }) => {
                info!(%external_addr, "relay: attached, external addr from welcome");
            }
            _ => {
                tracing::warn!("relay: expected Welcome message, got something else");
            }
        }

        let weak = Arc::downgrade(&self.shared);
        let dg = datagram.clone();
        tokio::spawn(async move {
            relay_listener_loop(weak, dg, relay_addr).await;
        });

        {
            let mut relay = self.shared.relay.lock().await;
            *relay = Some(RelayState {
                _connection: conn,
                datagram,
                relay_addr,
            });
        }

        Ok(())
    }

    /// Connect to a remote peer through the attached relay. The peer must also
    /// be attached to the same relay.
    pub async fn connect_peer(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let (sid, init_bytes) = {
            let mut state = self.shared.state.lock().unwrap();
            let sid = state.next_connection_id;
            state.next_connection_id += 1;
            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());
            let init_bytes = Handshake {
                initiator_connection_id: sid,
                kem_public_key: *eph_pk,
            }
            .encode();
            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    send_path: SendPath::Relayed { peer_id: *peer_id },
                    relay_peer_id: Some(*peer_id),
                },
            );
            (sid, init_bytes)
        };
        send_via_relay(&self.shared, peer_id, &init_bytes).await?;

        wait_for_established(&self.shared, sid).await
    }

    pub async fn accept(&self) -> io::Result<Connection> {
        loop {
            if self.shared.shutdown.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "node shutdown",
                ));
            }
            {
                let mut state = self.shared.state.lock().unwrap();
                if let Some(connection_id) = state.accept_queue.pop_front() {
                    return Ok(Connection {
                        shared: self.shared.clone(),
                        connection_id,
                        closed: AtomicBool::new(false),
                    });
                }
            }
            self.shared.accept_notify.notified().await;
        }
    }
}
