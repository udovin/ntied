use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::connection::Connection as InnerConnection;
use crate::crypto::{EncryptionKeys, KemPrivateKey, PrivateKey, compute_transcript_hash};
use crate::session::{Role, Session};
use crate::wire::{Data, Handshake, HandshakeAck, Packet};

use super::channel::{DatagramChannel, StreamChannel};
use super::util::build_auth_payload;

const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type ConnectionMap = Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>>;
pub(crate) type ChannelMap = Arc<RwLock<HashMap<u32, mpsc::Sender<Vec<u8>>>>>;

pub(crate) struct OwnedConnectionId {
    id: u64,
    connection_map: ConnectionMap,
}

impl OwnedConnectionId {
    pub(crate) fn new(id: u64, connection_map: &ConnectionMap, tx: mpsc::Sender<Packet>) -> Self {
        connection_map.write().unwrap().insert(id, tx);
        Self {
            id,
            connection_map: connection_map.clone(),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for OwnedConnectionId {
    fn drop(&mut self) {
        self.connection_map.write().unwrap().remove(&self.id);
    }
}

pub struct Connection {
    pub(crate) _owned_connection_id: OwnedConnectionId,
    pub(crate) inner: Arc<Mutex<InnerConnection>>,
    pub(crate) channel_map: ChannelMap,
    pub(crate) accept_channel_rx: TokioMutex<mpsc::Receiver<(u32, u16, mpsc::Receiver<Vec<u8>>)>>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) main_task: Mutex<Option<JoinHandle<()>>>,
}

impl Connection {
    pub(crate) async fn accept(
        init: Handshake,
        owned_connection_id: OwnedConnectionId,
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        rx: mpsc::Receiver<Packet>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) {
        let responder_connection_id = owned_connection_id.id();

        let eph = KemPrivateKey::generate();
        let Some((ct, shared_secret)) = eph.encapsulate(&init.kem_public_key) else {
            return;
        };

        let keys = EncryptionKeys::new(&shared_secret, &init.kem_public_key, &ct);
        let transcript_hash = compute_transcript_hash(&init.kem_public_key, &ct);

        let response = HandshakeAck {
            responder_connection_id,
            initiator_connection_id: init.initiator_connection_id,
            kem_ciphertext: ct,
        };
        if let Err(err) = socket.send_to(&response.encode(), addr).await {
            warn!(?err, "Failed to send handshake ack");
        }

        let session = Session::new(Role::Responder, 1, keys, transcript_hash);
        let auth_payload = build_auth_payload(&identity, &transcript_hash);
        let mut conn = InnerConnection::new(
            session,
            responder_connection_id,
            init.initiator_connection_id,
            false,
            auth_payload,
        );

        let packets = conn.poll_packets(Instant::now());
        Self::send_packets(&socket, addr, packets).await;

        let inner = Arc::new(Mutex::new(conn));

        let channel_map: ChannelMap = Default::default();
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Self::main_loop(
            inner.clone(),
            rx,
            socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            channel_map.clone(),
            accept_channel_tx,
        ));

        match tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                cancel_token.cancel();
                warn!("Connection unexpectedly closed");
                return;
            }
            Err(_) => {
                cancel_token.cancel();
                warn!("Auth timed out");
                return;
            }
        }

        let connection = Connection {
            _owned_connection_id: owned_connection_id,
            inner,
            channel_map,
            accept_channel_rx: TokioMutex::new(accept_channel_rx),
            socket,
            addr,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        };
        if let Err(_) = accept_tx.send(connection).await {
            warn!("Failed to send connection to accept queue");
        }
    }

    pub(crate) async fn connect(
        owned_connection_id: OwnedConnectionId,
        eph: KemPrivateKey,
        init: Handshake,
        mut rx: mpsc::Receiver<Packet>,
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) -> io::Result<Connection> {
        let connection_id = owned_connection_id.id();

        let handshake_ack = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                let packet = rx.recv().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::ConnectionReset, "channel closed")
                })?;
                match packet {
                    Packet::HandshakeAck(v) => break Ok::<_, io::Error>(v),
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;

        let shared_secret = eph
            .decapsulate(&handshake_ack.kem_ciphertext)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "KEM decapsulation failed")
            })?;

        let keys = EncryptionKeys::new(
            &shared_secret,
            &init.kem_public_key,
            &handshake_ack.kem_ciphertext,
        );
        let transcript_hash =
            compute_transcript_hash(&init.kem_public_key, &handshake_ack.kem_ciphertext);
        let session = Session::new(Role::Initiator, 1, keys, transcript_hash);
        let auth_payload = build_auth_payload(&identity, &transcript_hash);
        let mut conn = InnerConnection::new(
            session,
            connection_id,
            handshake_ack.responder_connection_id,
            true,
            auth_payload,
        );

        let packets = conn.poll_packets(Instant::now());
        Self::send_packets(&socket, addr, packets).await;

        let inner = Arc::new(Mutex::new(conn));

        let channel_map: ChannelMap = Default::default();
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Self::main_loop(
            inner.clone(),
            rx,
            socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            channel_map.clone(),
            accept_channel_tx,
        ));

        match tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                cancel_token.cancel();
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "Connection closed",
                ));
            }
            Err(_) => {
                cancel_token.cancel();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Auth timed out"));
            }
        }

        Ok(Connection {
            _owned_connection_id: owned_connection_id,
            inner,
            channel_map,
            accept_channel_rx: tokio::sync::Mutex::new(accept_channel_rx),
            socket,
            addr,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        })
    }

    pub async fn close(&self) {
        let main_task = self.main_task.lock().unwrap().take();
        if let Some(task) = main_task {
            self.cancel_token.cancel();
            let _ = task.await;
        }
    }

    pub(crate) async fn main_loop(
        inner: Arc<Mutex<InnerConnection>>,
        mut rx: mpsc::Receiver<Packet>,
        socket: Arc<UdpSocket>,
        addr: SocketAddr,
        cancel_token: CancellationToken,
        established_tx: Option<oneshot::Sender<()>>,
        channel_map: ChannelMap,
        accept_channel_tx: mpsc::Sender<(u32, u16, mpsc::Receiver<Vec<u8>>)>,
    ) {
        let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ping_interval = tokio::time::interval(PING_INTERVAL);
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut established_tx = established_tx;
        let mut last_recv = Instant::now();
        let mut ping_counter: u32 = 0;

        loop {
            tokio::select! {
                packet = rx.recv() => {
                    let Some(packet) = packet else { break };
                    match packet {
                        Packet::Data(data) => {
                            last_recv = Instant::now();
                            let packets = {
                                let mut conn = inner.lock().unwrap();
                                conn.on_data_packet(data, last_recv);
                                if conn.is_established() {
                                    if let Some(tx) = established_tx.take() {
                                        let _ = tx.send(());
                                    }
                                }
                                conn.send_packets()
                            };
                            Self::send_packets(&socket, addr, packets).await;
                            if Self::dispatch_channels(&inner, &channel_map, &accept_channel_tx) {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    let now = Instant::now();
                    if now.duration_since(last_recv) > CONNECTION_TIMEOUT {
                        warn!("Connection timed out");
                        break;
                    }
                    let packets = {
                        let mut conn = inner.lock().unwrap();
                        conn.poll_packets(now)
                    };
                    Self::send_packets(&socket, addr, packets).await;
                }
                _ = ping_interval.tick() => {
                    let packets = {
                        let mut conn = inner.lock().unwrap();
                        if conn.is_established() {
                            ping_counter = ping_counter.wrapping_add(1);
                            conn.queue_ping(ping_counter);
                            conn.send_packets()
                        } else {
                            Vec::new()
                        }
                    };
                    Self::send_packets(&socket, addr, packets).await;
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }

        let packets = {
            let mut conn = inner.lock().unwrap();
            if !conn.got_connection_close() {
                conn.queue_connection_close(0);
            }
            conn.poll_packets(Instant::now())
        };
        Self::send_packets(&socket, addr, packets).await;
    }

    async fn send_packets(socket: &UdpSocket, addr: SocketAddr, packets: Vec<Data>) {
        for packet in packets {
            if let Err(err) = socket.send_to(&packet.encode(), addr).await {
                warn!(?err, "Failed to send packet");
            }
        }
    }

    /// Returns true if the connection should be stopped.
    fn dispatch_channels(
        inner: &Arc<Mutex<InnerConnection>>,
        channel_map: &ChannelMap,
        accept_channel_tx: &mpsc::Sender<(u32, u16, mpsc::Receiver<Vec<u8>>)>,
    ) -> bool {
        let mut conn = inner.lock().unwrap();

        if conn.got_connection_close() {
            channel_map.write().unwrap().clear();
            return true;
        }

        while let Some((id, purpose)) = conn.accept_stream() {
            let (tx, rx) = mpsc::channel(16);
            if accept_channel_tx.try_send((id, purpose, rx)).is_ok() {
                channel_map.write().unwrap().insert(id, tx);
            } else {
                warn!(
                    channel_id = id,
                    "Dropping accepted stream: accept channel full"
                );
            }
        }
        while let Some((id, purpose)) = conn.accept_datagram() {
            let (tx, rx) = mpsc::channel(16);
            if accept_channel_tx.try_send((id, purpose, rx)).is_ok() {
                channel_map.write().unwrap().insert(id, tx);
            } else {
                warn!(
                    channel_id = id,
                    "Dropping accepted datagram: accept channel full"
                );
            }
        }

        let mut finished = Vec::new();
        {
            let map = channel_map.read().unwrap();
            for (&channel_id, tx) in map.iter() {
                while let Ok(Some(data)) = conn.read(channel_id) {
                    if tx.try_send(data).is_err() {
                        warn!(channel_id, "Channel buffer full, data dropped");
                        break;
                    }
                }
                while let Ok(Some(data)) = conn.read_datagram(channel_id) {
                    if tx.try_send(data).is_err() {
                        warn!(channel_id, "Channel buffer full, datagram dropped");
                        break;
                    }
                }
                if conn.is_channel_finished(channel_id) {
                    finished.push(channel_id);
                }
            }
        }
        if !finished.is_empty() {
            let mut map = channel_map.write().unwrap();
            for id in finished {
                map.remove(&id);
            }
        }
        false
    }

    pub fn open_stream(&self, purpose: u16) -> StreamChannel {
        let (tx, rx) = mpsc::channel(16);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_stream(purpose)
        };
        self.channel_map.write().unwrap().insert(channel_id, tx);
        StreamChannel {
            channel_id,
            inner: self.inner.clone(),
            channel_map: self.channel_map.clone(),
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    pub fn open_datagram(&self, purpose: u16) -> DatagramChannel {
        let (tx, rx) = mpsc::channel(16);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_datagram(purpose)
        };
        self.channel_map.write().unwrap().insert(channel_id, tx);
        DatagramChannel {
            channel_id,
            inner: self.inner.clone(),
            channel_map: self.channel_map.clone(),
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    pub async fn accept_stream(&self) -> io::Result<(StreamChannel, u16)> {
        let (channel_id, purpose, rx) = self
            .accept_channel_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))?;
        Ok((
            StreamChannel {
                channel_id,
                inner: self.inner.clone(),
                channel_map: self.channel_map.clone(),
                rx: tokio::sync::Mutex::new(rx),
            },
            purpose,
        ))
    }

    pub async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)> {
        let (channel_id, purpose, rx) = self
            .accept_channel_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))?;
        Ok((
            DatagramChannel {
                channel_id,
                inner: self.inner.clone(),
                channel_map: self.channel_map.clone(),
                rx: tokio::sync::Mutex::new(rx),
            },
            purpose,
        ))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let main_task = self.main_task.lock().unwrap().take();
        if let Some(task) = main_task {
            self.cancel_token.cancel();
            drop(task);
        }
    }
}
