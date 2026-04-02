use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::task::JoinHandle;

use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::channel::ChannelError;
use crate::connection::PeerConnection;
use crate::crypto::PeerId as PeerIdType;
use crate::crypto::{
    EncryptionKeys, KemPrivateKey, PeerId, PrivateKey, PublicKey, compute_transcript_hash,
};
use crate::dht::{DhtHandler, DhtRecord};
use crate::session::{Role, Session};
use crate::wire::packet::{Data, HolePunch, Packet};
use crate::wire::{Frame, GatewayPacket, KeyExchangeInit, KeyExchangeResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteInfo {
    Direct(SocketAddr),
    Relayed {
        gateway_peer_id: PeerId,
        gateway_addr: SocketAddr,
    },
}

const RECV_BUF_SIZE: usize = 4096;
const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const HOLE_PUNCH_COUNT: u8 = 4;
const HOLE_PUNCH_INTERVAL: Duration = Duration::from_millis(150);

pub struct NodeConfig {
    pub identity: PrivateKey,
    pub bind_addr: SocketAddr,
    pub bootstrap: Vec<SocketAddr>,
    pub relay: bool,
    pub registry: bool,
}

pub struct Node {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
}

pub(crate) struct Shared {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) identity: PrivateKey,
    pub(crate) state: TokioMutex<TransportState>,
    pub(crate) pending_close: std::sync::Mutex<Vec<u64>>,
    pub(crate) ping_counter: AtomicU32,
    pub(crate) accept_notify: Notify,
    pub(crate) established_notify: Notify,
    pub(crate) data_notify: Notify,
    pub(crate) stream_notify: Notify,
    pub(crate) gateway_notify: Notify,
    pub(crate) gateway_mode: AtomicBool,
}

pub(crate) struct TransportState {
    pub(crate) connections: HashMap<u64, ConnEntry>,
    pub(crate) pending_connects: HashMap<u64, PendingConnect>,
    pub(crate) accept_queue: VecDeque<u64>,
    pub(crate) next_connection_id: u64,
    pub(crate) hole_punches: Vec<HolePunchEntry>,
    pub(crate) gateway: Option<GatewayState>,
    pub(crate) gateway_clients: HashMap<PeerId, RegisteredClient>,
    pub(crate) dht_handler: Option<DhtHandler>,
    pub(crate) pending_dht_queries: HashMap<u32, oneshot::Sender<Option<DhtRecord>>>,
    pub(crate) next_dht_request_id: u32,
    pub(crate) dht_publish_fragments: HashMap<u64, DhtPublishCollector>,
    pub(crate) gateway_peers: HashMap<PeerId, GatewayPeer>,
    pub(crate) pending_gw_queries: HashMap<u32, PendingGwQuery>,
    pub(crate) next_gw_query_id: u32,
}

#[derive(Clone)]
pub(crate) struct PendingGwQuery {
    pub(crate) client_connection_id: u64,
    pub(crate) client_request_id: u32,
    pub(crate) remaining_peers: usize,
}

pub(crate) const GATEWAY_PEER_FLAG: u16 = 0x01;

#[derive(Clone)]
pub(crate) struct GatewayPeer {
    pub(crate) connection_id: u64,
    pub(crate) addr: SocketAddr,
}

pub(crate) struct DhtPublishCollector {
    pub(crate) fragments: Vec<Option<Vec<u8>>>,
    pub(crate) received: u8,
    pub(crate) total: u8,
}

#[derive(Clone)]
pub(crate) struct RegisteredClient {
    pub(crate) connection_id: u64,
    pub(crate) external_addr: SocketAddr,
}

pub(crate) struct GatewayState {
    pub(crate) connection_id: u64,
    pub(crate) registered: bool,
    pub(crate) relay_mtu: u16,
}

pub(crate) struct HolePunchEntry {
    pub(crate) peer_addr: SocketAddr,
    pub(crate) next_send: Instant,
    pub(crate) remaining: u8,
}

#[derive(Debug, Clone)]
pub enum TransportPath {
    Direct {
        addr: SocketAddr,
    },
    Relayed {
        gateway_connection_id: u64,
        dest_peer_id: PeerIdType,
    },
}

pub(crate) struct ConnEntry {
    pub(crate) path: TransportPath,
    pub(crate) conn: Box<PeerConnection>,
    pub(crate) last_recv: Instant,
    pub(crate) last_ping_sent: Instant,
    pub(crate) closed: bool,
    pub(crate) is_local_initiator: bool,
    pub(crate) service: u16,
}

pub(crate) struct PendingConnect {
    pub(crate) ephemeral_key: Box<KemPrivateKey>,
    pub(crate) peer_addr: SocketAddr,
    pub(crate) relayed: bool,
    pub(crate) target_peer_id: Option<PeerId>,
    pub(crate) relay_connection_id: Option<u64>,
    pub(crate) service: u16,
}

impl Node {
    pub async fn start(config: NodeConfig) -> io::Result<Self> {
        let node = Self::bind(config.bind_addr, config.identity).await?;

        if config.registry {
            node.enable_gateway().await;
            if !config.bootstrap.is_empty() {
                node.bootstrap(config.bootstrap.clone()).await?;
            }
        }

        if config.relay {
            if !config.registry {
                node.enable_gateway().await;
            }
            if !config.bootstrap.is_empty() && !config.registry {
                node.bootstrap(config.bootstrap.clone()).await?;
            }
        }

        if !config.relay && !config.registry && !config.bootstrap.is_empty() {
            // Peer mode: join network via relay
            node.join_network(NetworkConfig {
                bootstrap: config.bootstrap,
                preferred_gateway: None,
            })
            .await?;
        }

        Ok(node)
    }

    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        Self::init(socket, identity).await
    }

    async fn init(socket: Arc<UdpSocket>, identity: PrivateKey) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            socket: socket.clone(),
            identity,
            state: TokioMutex::new(TransportState {
                connections: HashMap::new(),
                pending_connects: HashMap::new(),
                accept_queue: VecDeque::new(),
                next_connection_id: {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    socket.local_addr().ok().hash(&mut h);
                    std::time::Instant::now().hash(&mut h);
                    (h.finish() >> 32) | 1 // random-ish, never 0
                },
                hole_punches: Vec::new(),
                gateway: None,
                gateway_clients: HashMap::new(),
                dht_handler: None,
                pending_dht_queries: HashMap::new(),
                next_dht_request_id: 1,
                dht_publish_fragments: HashMap::new(),
                gateway_peers: HashMap::new(),
                pending_gw_queries: HashMap::new(),
                next_gw_query_id: 0x8000_0000,
            }),
            pending_close: std::sync::Mutex::new(Vec::new()),
            ping_counter: AtomicU32::new(1),
            accept_notify: Notify::new(),
            established_notify: Notify::new(),
            data_notify: Notify::new(),
            stream_notify: Notify::new(),
            gateway_notify: Notify::new(),
            gateway_mode: AtomicBool::new(false),
        });

        let task_shared = shared.clone();
        let recv_task = tokio::spawn(recv_loop(task_shared));

        Ok(Self {
            shared,
            _recv_task: recv_task,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared.socket.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.shared.identity.public_key().peer_id()
    }

    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let state = self.shared.state.lock().await;
        if state.gateway.is_some() {
            drop(state);
            let dht = DhtDiscovery {
                shared: self.shared.clone(),
            };
            debug!(?peer_id, "connect: trying DHT");
            if let Some(route) = dht.resolve(peer_id).await {
                debug!(?peer_id, ?route, "connect: DHT resolved");
                return match route {
                    RouteInfo::Direct(addr) => {
                        self.connect_to_addr(
                            addr,
                            Some(peer_id.clone()),
                            crate::wire::packet::SERVICE_APPLICATION,
                        )
                        .await
                    }
                    RouteInfo::Relayed { gateway_addr, .. } => {
                        // Connect to the peer's relay, then relay through it
                        self.connect_via_peer_relay(peer_id, gateway_addr).await
                    }
                };
            }
            warn!(?peer_id, "connect: DHT resolve failed");
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "peer not found via DHT",
        ))
    }

    async fn connect_to_addr(
        &self,
        peer_addr: SocketAddr,
        target_peer_id: Option<PeerId>,
        service: u16,
    ) -> io::Result<Connection> {
        let connection_id = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_connection_id;
            state.next_connection_id += 1;

            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let hole_punch_bytes = Packet::HolePunch(HolePunch {
                sender_peer_id: self.shared.identity.public_key().peer_id(),
            })
            .encode();
            let _ = self
                .shared
                .socket
                .send_to(&hole_punch_bytes, peer_addr)
                .await;

            state.hole_punches.push(HolePunchEntry {
                peer_addr,
                next_send: Instant::now() + HOLE_PUNCH_INTERVAL,
                remaining: HOLE_PUNCH_COUNT - 1,
            });

            let init_bytes = KeyExchangeInit {
                initiator_connection_id: sid,
                service,
                kem_public_key: *eph_pk,
            }
            .encode();
            self.shared.socket.send_to(&init_bytes, peer_addr).await?;

            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    peer_addr,
                    relayed: false,
                    target_peer_id: target_peer_id.clone(),
                    relay_connection_id: None,
                    service,
                },
            );

            sid
        };

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.established_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(entry) = state.connections.get(&connection_id) {
                        if entry.conn.is_established() {
                            return Ok(Connection {
                                shared: self.shared.clone(),
                                connection_id,
                                closed: AtomicBool::new(false),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.shared.state.lock().await;
                    state.pending_connects.remove(&connection_id);
                    state.connections.remove(&connection_id);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "handshake timed out",
                    ));
                }
            }
        }
    }

    pub async fn connect_addr(&self, addr: SocketAddr) -> io::Result<Connection> {
        self.connect_to_addr(addr, None, crate::wire::packet::SERVICE_APPLICATION)
            .await
    }

    pub async fn enable_gateway(&self) {
        let local_id = self.shared.identity.public_key().peer_id();
        let mut state = self.shared.state.lock().await;
        state.dht_handler = Some(DhtHandler::new(local_id));
        drop(state);
        self.shared.gateway_mode.store(true, Ordering::SeqCst);
    }

    pub fn is_gateway(&self) -> bool {
        self.shared.gateway_mode.load(Ordering::SeqCst)
    }

    async fn publish_dht_record(&self, gw_connection_id: u64, gw_addr: SocketAddr) {
        let pk = self.shared.identity.public_key();
        let peer_id = pk.peer_id();
        let gw_peer_id = {
            let state = self.shared.state.lock().await;
            state
                .connections
                .get(&gw_connection_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()))
        };

        let gw_id = match gw_peer_id {
            Some(id) => id,
            None => return,
        };

        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let record = DhtRecord::sign(
            peer_id,
            pk,
            vec![crate::dht::GatewayInfo {
                gateway_peer_id: gw_id,
                addrs: vec![gw_addr],
                latency_hint: 0,
            }],
            crate::dht::RoutingPolicy::Open,
            1,
            now_unix + 3600,
            &self.shared.identity,
        );

        let encoded = record.encode();
        let max_fragment = 1000; // fits within a single Data packet
        let fragments: Vec<Vec<u8>> = encoded.chunks(max_fragment).map(|c| c.to_vec()).collect();
        let total = fragments.len() as u8;

        let mut state = self.shared.state.lock().await;
        if let Some(entry) = state.connections.get_mut(&gw_connection_id) {
            for (i, data) in fragments.into_iter().enumerate() {
                entry
                    .conn
                    .queue_frame(Frame::DhtPublish(crate::wire::DhtPublish {
                        fragment_index: i as u8,
                        fragment_total: total,
                        data,
                    }));
            }
        }
        drop(state);
        flush_connection(&self.shared, gw_connection_id).await.ok();
    }

    pub async fn add_gateway_peer(&self, addr: SocketAddr) -> io::Result<()> {
        let conn = self
            .connect_to_addr(addr, None, crate::wire::packet::SERVICE_SYSTEM)
            .await?;
        let peer_connection_id = conn.connection_id();
        let peer_pk = conn.peer_public_key().await;
        std::mem::forget(conn);

        let local_peer_id = self.shared.identity.public_key().peer_id();
        {
            let mut state = self.shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&peer_connection_id) {
                entry
                    .conn
                    .queue_frame(Frame::GatewayRegister(crate::wire::GatewayRegister {
                        peer_id: local_peer_id,
                        flags: GATEWAY_PEER_FLAG,
                        auth_data: Vec::new(),
                    }));
            }

            if let Some(pk) = peer_pk {
                let pid = pk.peer_id();
                state.gateway_peers.insert(
                    pid,
                    GatewayPeer {
                        connection_id: peer_connection_id,
                        addr,
                    },
                );
                if let Some(dht) = &mut state.dht_handler {
                    dht.table_mut().insert(
                        crate::dht::DhtNode {
                            peer_id: pid,
                            addrs: vec![addr],
                        },
                        Instant::now(),
                    );
                }
            }
        }
        flush_connection(&self.shared, peer_connection_id).await?;

        // Wait for ack
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.gateway_notify.notified() => {
                    return Ok(());
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "gateway peer registration timed out",
                    ));
                }
            }
        }
    }

    /// Bootstrap into the gateway network.
    /// Connects to seed nodes, then iteratively discovers and connects
    /// to peers that should be in our k-buckets.
    pub async fn bootstrap(&self, seed_addrs: Vec<SocketAddr>) -> io::Result<()> {
        // Connect to seed nodes
        for addr in &seed_addrs {
            if let Err(e) = self.add_gateway_peer(*addr).await {
                warn!(?addr, ?e, "bootstrap: failed to connect to seed");
            }
        }

        let my_id = self.shared.identity.public_key().peer_id();

        // Iterative bootstrap: discover and connect to new peers
        for _round in 0..3 {
            // Start a refresh lookup for our own ID
            let actions = {
                let mut state = self.shared.state.lock().await;
                let dht = match &mut state.dht_handler {
                    Some(d) => d,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "gateway mode not enabled",
                        ));
                    }
                };
                let (_req_id, actions) = dht.start_refresh(my_id, Instant::now());
                actions
            };

            // Process the FIND_NODE sends
            process_dht_actions(&self.shared, actions).await;

            // Wait for replies to arrive and be processed
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Check k-buckets for nodes we're not yet connected to
            let new_peers: Vec<(PeerId, SocketAddr)> = {
                let state = self.shared.state.lock().await;
                let dht = match &state.dht_handler {
                    Some(d) => d,
                    None => continue,
                };
                let all_nodes = dht.table().closest(&my_id, 100);
                all_nodes
                    .into_iter()
                    .filter(|node| !state.gateway_peers.contains_key(&node.peer_id))
                    .filter_map(|node| node.addrs.first().map(|addr| (node.peer_id, *addr)))
                    .collect()
            };

            if new_peers.is_empty() {
                break;
            }

            for (peer_id, addr) in new_peers {
                debug!(?peer_id, ?addr, "bootstrap: connecting to discovered peer");
                if let Err(e) = self.add_gateway_peer(addr).await {
                    debug!(?addr, ?e, "bootstrap: failed to connect to peer");
                }
            }
        }

        info!(
            peers = {
                let state = self.shared.state.lock().await;
                state.gateway_peers.len()
            },
            "bootstrap: complete"
        );
        Ok(())
    }

    pub fn dht_discovery(&self) -> Arc<DhtDiscovery> {
        Arc::new(DhtDiscovery {
            shared: self.shared.clone(),
        })
    }

    async fn connect_via_peer_relay(
        &self,
        peer_id: &PeerId,
        relay_addr: SocketAddr,
    ) -> io::Result<Connection> {
        // Connect to the peer's relay as a gateway client
        let relay_conn = self
            .connect_to_addr(relay_addr, None, crate::wire::packet::SERVICE_SYSTEM)
            .await?;
        let relay_connection_id = relay_conn.connection_id();
        std::mem::forget(relay_conn);

        // Register with the relay
        let local_peer_id = self.shared.identity.public_key().peer_id();
        {
            let mut state = self.shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&relay_connection_id) {
                entry
                    .conn
                    .queue_frame(Frame::GatewayRegister(crate::wire::GatewayRegister {
                        peer_id: local_peer_id,
                        flags: 0, // client, not peer
                        auth_data: Vec::new(),
                    }));
            }
        }
        flush_connection(&self.shared, relay_connection_id).await?;

        // Wait for ack
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::select! {
                _ = self.shared.gateway_notify.notified() => break,
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "relay registration timed out",
                    ));
                }
            }
        }

        // Now send KeyExchangeInit via this relay
        let connection_id = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_connection_id;
            state.next_connection_id += 1;
            info!(me = %short_pid(&self.shared), ?peer_id, sid, relay_connection_id, "connect_via_peer_relay: sending init");

            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let init = KeyExchangeInit {
                initiator_connection_id: sid,
                service: crate::wire::packet::SERVICE_APPLICATION,
                kem_public_key: *eph_pk,
            };
            let init_bytes = init.encode();

            if let Some(entry) = state.connections.get_mut(&relay_connection_id) {
                entry.conn.queue_frame(Frame::GatewayPacket(GatewayPacket {
                    dest_peer_id: peer_id.clone(),
                    src_peer_id: local_peer_id,
                    inner: init_bytes,
                }));
            }

            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    peer_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
                    relayed: true,
                    target_peer_id: Some(peer_id.clone()),
                    relay_connection_id: Some(relay_connection_id),
                    service: crate::wire::packet::SERVICE_APPLICATION,
                },
            );

            flush_connection_locked(&mut state, &self.shared, relay_connection_id).await;
            sid
        };

        // Wait for handshake
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.established_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(entry) = state.connections.get(&connection_id) {
                        if entry.conn.is_established() {
                            return Ok(Connection {
                                shared: self.shared.clone(),
                                connection_id,
                                closed: AtomicBool::new(false),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.shared.state.lock().await;
                    state.pending_connects.remove(&connection_id);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "handshake timed out",
                    ));
                }
            }
        }
    }

    async fn connect_via_relay(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let connection_id = {
            let mut state = self.shared.state.lock().await;
            let gw = state.gateway.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "not connected to gateway")
            })?;
            let gw_connection_id = gw.connection_id;

            let sid = state.next_connection_id;
            state.next_connection_id += 1;
            info!(me = %short_pid(&self.shared), ?peer_id, sid, gw_connection_id, "connect_via_relay: sending init");

            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let init = KeyExchangeInit {
                initiator_connection_id: sid,
                service: crate::wire::packet::SERVICE_APPLICATION,
                kem_public_key: *eph_pk,
            };
            let init_bytes = init.encode();

            if let Some(gw_entry) = state.connections.get_mut(&gw_connection_id) {
                gw_entry
                    .conn
                    .queue_frame(Frame::GatewayPacket(GatewayPacket {
                        dest_peer_id: peer_id.clone(),
                        src_peer_id: self.shared.identity.public_key().peer_id(),
                        inner: init_bytes,
                    }));
            }

            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    peer_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
                    relayed: true,
                    target_peer_id: Some(peer_id.clone()),
                    relay_connection_id: None,
                    service: crate::wire::packet::SERVICE_APPLICATION,
                },
            );

            flush_connection_locked(&mut state, &self.shared, gw_connection_id).await;

            sid
        };

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.established_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(entry) = state.connections.get(&connection_id) {
                        if entry.conn.is_established() {
                            return Ok(Connection {
                                shared: self.shared.clone(),
                                connection_id,
                                closed: AtomicBool::new(false),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.shared.state.lock().await;
                    state.pending_connects.remove(&connection_id);
                    state.connections.remove(&connection_id);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "handshake timed out",
                    ));
                }
            }
        }
    }

    pub async fn join_network(&self, config: NetworkConfig) -> io::Result<()> {
        let bootstrap_addr =
            config.bootstrap.first().copied().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "no bootstrap address")
            })?;

        let gw_conn = self
            .connect_to_addr(
                bootstrap_addr,
                None,
                crate::wire::packet::SERVICE_SYSTEM,
            )
            .await?;
        let gw_connection_id = gw_conn.connection_id();
        std::mem::forget(gw_conn);

        let local_peer_id = self.shared.identity.public_key().peer_id();
        {
            let mut state = self.shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&gw_connection_id) {
                entry
                    .conn
                    .queue_frame(Frame::GatewayRegister(crate::wire::GatewayRegister {
                        peer_id: local_peer_id,
                        flags: 0,
                        auth_data: Vec::new(),
                    }));
            }
            state.gateway = Some(GatewayState {
                connection_id: gw_connection_id,
                registered: false,
                relay_mtu: 0,
            });
        }
        flush_connection(&self.shared, gw_connection_id).await?;

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.gateway_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(gw) = &state.gateway {
                        if gw.registered {
                            drop(state);
                            self.publish_dht_record(gw_connection_id, bootstrap_addr).await;
                            return Ok(());
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "gateway registration timed out",
                    ));
                }
            }
        }
    }

    pub async fn accept(&self) -> io::Result<Connection> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
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

pub struct NetworkConfig {
    pub bootstrap: Vec<SocketAddr>,
    pub preferred_gateway: Option<PeerId>,
}

pub use crate::registry::client::DhtDiscovery;

pub struct Connection {
    shared: Arc<Shared>,
    connection_id: u64,
    closed: AtomicBool,
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.shared
                .pending_close
                .lock()
                .unwrap()
                .push(self.connection_id);
        }
    }
}

impl Connection {
    pub fn connection_id(&self) -> u64 {
        self.connection_id
    }

    pub async fn peer_public_key(&self) -> Option<PublicKey> {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.connection_id)
            .and_then(|e| e.conn.peer_public_key().cloned())
    }

    pub async fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().await.map(|pk| pk.peer_id())
    }

    pub async fn transport_path(&self) -> Option<TransportPath> {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.connection_id)
            .map(|e| e.path.clone())
    }

    pub async fn is_established(&self) -> bool {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.connection_id)
            .map_or(false, |e| e.conn.is_established() && !e.closed)
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut state = self.shared.state.lock().await;
        if let Some(entry) = state.connections.get_mut(&self.connection_id) {
            if !entry.closed {
                entry.closed = true;
                entry.conn.queue_connection_close(0);
                let packets = entry.conn.poll_packets(Instant::now());
                let path = entry.path.clone();
                drop(state);
                send_packets(&self.shared, &path, &packets).await;
            }
        }
        Ok(())
    }

    pub async fn open_stream(&self, purpose: u16) -> io::Result<StreamChannel> {
        let channel_id = {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_stream(purpose)
        };
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(StreamChannel {
            shared: self.shared.clone(),
            connection_id: self.connection_id,
            channel_id,
        })
    }

    pub async fn accept_stream(&self) -> io::Result<(StreamChannel, u16)> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                if let Some(entry) = state.connections.get_mut(&self.connection_id) {
                    if let Some((channel_id, purpose)) = entry.conn.accept_stream() {
                        return Ok((
                            StreamChannel {
                                shared: self.shared.clone(),
                                connection_id: self.connection_id,
                                channel_id,
                            },
                            purpose,
                        ));
                    }
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection gone",
                    ));
                }
            }
            self.shared.stream_notify.notified().await;
        }
    }

    pub async fn open_datagram(&self, purpose: u16) -> io::Result<DatagramChannel> {
        let channel_id = {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_datagram(purpose)
        };
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(DatagramChannel {
            shared: self.shared.clone(),
            connection_id: self.connection_id,
            channel_id,
        })
    }

    pub async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)> {
        let (stream, purpose) = self.accept_stream().await?;
        Ok((
            DatagramChannel {
                shared: stream.shared,
                connection_id: stream.connection_id,
                channel_id: stream.channel_id,
            },
            purpose,
        ))
    }
}

pub struct StreamChannel {
    shared: Arc<Shared>,
    connection_id: u64,
    channel_id: u32,
}

pub struct DatagramChannel {
    shared: Arc<Shared>,
    connection_id: u64,
    channel_id: u32,
}

impl StreamChannel {
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write(self.channel_id, data)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                let entry = state
                    .connections
                    .get_mut(&self.connection_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                    })?;
                match entry.conn.read(self.channel_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(channel_err_to_io(e)),
                }
            }
            self.shared.data_notify.notified().await;
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_channel(self.channel_id)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }
}

impl DatagramChannel {
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write_datagram(self.channel_id, data)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                let entry = state
                    .connections
                    .get_mut(&self.connection_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                    })?;
                match entry.conn.read_datagram(self.channel_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(channel_err_to_io(e)),
                }
            }
            self.shared.data_notify.notified().await;
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_channel(self.channel_id)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }
}

async fn recv_loop(shared: Arc<Shared>) {
    let mut buf = vec![0u8; RECV_BUF_SIZE].into_boxed_slice();
    let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            result = shared.socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, addr)) => {
                        process_packet(&shared, &buf[..len], addr).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            }
            _ = flush_interval.tick() => {
                flush_all(&shared).await;
            }
        }
    }
}

async fn process_packet(shared: &Shared, buf: &[u8], addr: SocketAddr) {
    let packet = match Packet::decode(buf) {
        Ok(p) => Box::new(p),
        Err(_) => return,
    };

    match *packet {
        Packet::KeyExchangeInit(init) => {
            handle_key_exchange_init(shared, Box::new(init), addr).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            handle_key_exchange_response(shared, Box::new(resp), addr).await;
        }
        Packet::Data(data) => {
            handle_data(shared, data, addr).await;
        }
        Packet::HolePunch(_) => {
            shared
                .state
                .lock()
                .await
                .hole_punches
                .retain(|e| e.peer_addr != addr);
        }
    }
}

async fn handle_key_exchange_init(shared: &Shared, init: Box<KeyExchangeInit>, addr: SocketAddr) {
    shared
        .state
        .lock()
        .await
        .hole_punches
        .retain(|e| e.peer_addr != addr);

    let resp_eph = Box::new(KemPrivateKey::generate());
    let (ct, resp_ss) = match resp_eph.encapsulate(&init.kem_public_key) {
        Some(pair) => pair,
        None => return,
    };
    let ct = Box::new(ct);

    let keys = EncryptionKeys::new(&resp_ss, &init.kem_public_key, &ct);
    let th = compute_transcript_hash(&init.kem_public_key, &ct);

    let mut state = shared.state.lock().await;
    let local_sid = state.next_connection_id;
    state.next_connection_id += 1;

    let response = Box::new(KeyExchangeResponse {
        responder_connection_id: local_sid,
        initiator_connection_id: init.initiator_connection_id,
        kem_ciphertext: *ct,
    });

    let _ = shared.socket.send_to(&response.encode(), addr).await;

    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = PeerConnection::new(
        session,
        local_sid,
        init.initiator_connection_id,
        false,
        auth_payload,
    );

    let service = init.service;
    let entry = ConnEntry {
        path: TransportPath::Direct { addr },
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: false,
        service,
    };
    state.connections.insert(local_sid, entry);

    let packets = state
        .connections
        .get_mut(&local_sid)
        .unwrap()
        .conn
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &TransportPath::Direct { addr }, &packets).await;
}

pub(crate) async fn handle_key_exchange_response(
    shared: &Shared,
    resp: Box<KeyExchangeResponse>,
    addr: SocketAddr,
) {
    let mut state = shared.state.lock().await;
    state.hole_punches.retain(|e| e.peer_addr != addr);

    let pending = match state.pending_connects.remove(&resp.initiator_connection_id) {
        Some(p) => p,
        None => return,
    };

    let init_pk = Box::new(pending.ephemeral_key.public_key());
    let init_ss = match pending.ephemeral_key.decapsulate(&resp.kem_ciphertext) {
        Some(ss) => ss,
        None => return,
    };

    let keys = EncryptionKeys::new(&init_ss, &init_pk, &resp.kem_ciphertext);
    let th = compute_transcript_hash(&init_pk, &resp.kem_ciphertext);
    let session = Session::new(Role::Initiator, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = PeerConnection::new(
        session,
        resp.initiator_connection_id,
        resp.responder_connection_id,
        true,
        auth_payload,
    );

    let path = if pending.relayed {
        let gw_sid = pending
            .relay_connection_id
            .unwrap_or_else(|| state.gateway.as_ref().map(|g| g.connection_id).unwrap_or(0));
        TransportPath::Relayed {
            gateway_connection_id: gw_sid,
            dest_peer_id: pending.target_peer_id.unwrap_or(PeerId::zero()),
        }
    } else {
        TransportPath::Direct { addr }
    };

    let entry = ConnEntry {
        path: path.clone(),
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: true,
        service: pending.service,
    };
    state
        .connections
        .insert(resp.initiator_connection_id, entry);

    let packets = state
        .connections
        .get_mut(&resp.initiator_connection_id)
        .unwrap()
        .conn
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &path, &packets).await;
}

async fn handle_data(shared: &Shared, data: Data, _addr: SocketAddr) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    state.hole_punches.retain(|e| e.peer_addr != _addr);

    let receiver_sid = data.receiver_connection_id;
    let connection_id = match find_session_by_receiver(&state, receiver_sid) {
        Some(id) => id,
        None => {
            debug!(me = %short_pid(shared), receiver_sid, "handle_data: unknown session");
            return;
        }
    };

    let (
        was_established,
        is_established,
        has_new_stream,
        got_close,
        packets,
        path,
        unhandled,
        is_local_initiator,
        is_peer_session,
    ) = {
        let entry = match state.connections.get_mut(&connection_id) {
            Some(e) => e,
            None => return,
        };

        let was_established = entry.conn.is_established();
        let had_close = entry.conn.got_connection_close();
        let unhandled = entry.conn.on_data_packet(data, now);
        entry.last_recv = now;
        let is_established = entry.conn.is_established();
        let has_new_stream = entry.conn.has_pending_accept();
        let got_close = !had_close && entry.conn.got_connection_close();
        let is_local_initiator = entry.is_local_initiator;
        let is_peer_session = entry.service == crate::wire::packet::SERVICE_APPLICATION;
        if got_close {
            entry.closed = true;
        }
        let packets = entry.conn.poll_packets(now);
        let path = entry.path.clone();

        (
            was_established,
            is_established,
            has_new_stream,
            got_close,
            packets,
            path,
            unhandled,
            is_local_initiator,
            is_peer_session,
        )
    };

    if got_close {
        state.connections.remove(&connection_id);
    }

    if !was_established && is_established && !is_local_initiator && is_peer_session {
        debug!(connection_id, "accept_queue: push (handle_data)");
        state.accept_queue.push_back(connection_id);
    }
    drop(state);

    send_packets(shared, &path, &packets).await;

    for frame in unhandled {
        process_unhandled_frame(shared, connection_id, frame).await;
    }

    if !was_established && is_established {
        info!(me = %short_pid(shared), connection_id, is_local_initiator, "ESTABLISHED (direct)");
        if !is_local_initiator {
            shared.accept_notify.notify_waiters();
        }
        shared.established_notify.notify_waiters();
    }
    shared.data_notify.notify_waiters();
    if has_new_stream {
        shared.stream_notify.notify_waiters();
    }
}

// end of handle_data

async fn flush_all(shared: &Shared) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let closes: Vec<u64> = shared.pending_close.lock().unwrap().drain(..).collect();
    for connection_id in closes {
        if let Some(entry) = state.connections.get_mut(&connection_id) {
            if !entry.closed {
                entry.closed = true;
                entry.conn.queue_connection_close(0);
            }
        }
    }

    let local_peer_id = shared.identity.public_key().peer_id();
    let mut hole_punch_addrs: Vec<SocketAddr> = Vec::new();
    state.hole_punches.retain_mut(|entry| {
        if now >= entry.next_send {
            hole_punch_addrs.push(entry.peer_addr);
            entry.remaining -= 1;
            if entry.remaining == 0 {
                return false;
            }
            entry.next_send = now + HOLE_PUNCH_INTERVAL;
        }
        true
    });

    let gw_connection_id = state.gateway.as_ref().map(|g| g.connection_id);

    let mut timed_out: Vec<u64> = Vec::new();
    for (&sid, entry) in state.connections.iter_mut() {
        if entry.closed {
            continue;
        }
        if entry.conn.is_established() && now.duration_since(entry.last_recv) > CONNECTION_TIMEOUT {
            timed_out.push(sid);
            continue;
        }
        if entry.conn.is_established() && now.duration_since(entry.last_ping_sent) > PING_INTERVAL {
            let ping_id = shared.ping_counter.fetch_add(1, Ordering::Relaxed);
            entry.conn.queue_ping(ping_id);
            entry.last_ping_sent = now;
        }
    }
    for sid in &timed_out {
        state.connections.remove(sid);
    }

    let mut to_send_direct: Vec<(SocketAddr, Vec<Data>)> = Vec::new();
    let mut relay_frames: Vec<Frame> = Vec::new();
    let mut to_remove: Vec<u64> = Vec::new();

    for (&sid, entry) in state.connections.iter_mut() {
        if Some(sid) == gw_connection_id {
            continue;
        }
        let packets = entry.conn.poll_packets(now);
        if !packets.is_empty() {
            match &entry.path {
                TransportPath::Direct { addr } => {
                    to_send_direct.push((*addr, packets));
                }
                TransportPath::Relayed { dest_peer_id, .. } => {
                    for data in packets {
                        relay_frames.push(Frame::GatewayPacket(GatewayPacket {
                            dest_peer_id: dest_peer_id.clone(),
                            src_peer_id: shared.identity.public_key().peer_id(),
                            inner: data.encode(),
                        }));
                    }
                }
            }
        }
        if entry.closed && !entry.conn.has_pending() {
            to_remove.push(sid);
        }
    }

    if let Some(gw_sid) = gw_connection_id {
        if let Some(gw_entry) = state.connections.get_mut(&gw_sid) {
            for frame in relay_frames {
                gw_entry.conn.queue_frame(frame);
            }
            let gw_packets = gw_entry.conn.poll_packets(now);
            if !gw_packets.is_empty() {
                if let TransportPath::Direct { addr } = gw_entry.path {
                    to_send_direct.push((addr, gw_packets));
                }
            }
            if gw_entry.closed && !gw_entry.conn.has_pending() {
                to_remove.push(gw_sid);
            }
        }
    }

    for sid in &to_remove {
        state.connections.remove(sid);
    }
    drop(state);

    if !timed_out.is_empty() || !to_remove.is_empty() {
        shared.data_notify.notify_waiters();
        shared.stream_notify.notify_waiters();
    }

    if !hole_punch_addrs.is_empty() {
        let hp = Packet::HolePunch(HolePunch {
            sender_peer_id: local_peer_id,
        })
        .encode();
        for addr in hole_punch_addrs {
            let _ = shared.socket.send_to(&hp, addr).await;
        }
    }

    for (addr, packets) in &to_send_direct {
        for data in packets {
            let _ = shared.socket.send_to(&data.encode(), *addr).await;
        }
    }
}

pub(crate) fn short_pid(shared: &Shared) -> String {
    let full = format!("{:?}", shared.identity.public_key().peer_id());
    full.chars()
        .skip(full.len().saturating_sub(6))
        .take(4)
        .collect()
}

pub(crate) async fn flush_connection(shared: &Shared, connection_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (path, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let packets = entry.conn.poll_packets(now);
        (entry.path.clone(), packets)
    };

    send_packets(shared, &path, &packets).await;
    Ok(())
}

pub(crate) fn find_session_by_receiver(
    state: &TransportState,
    receiver_connection_id: u64,
) -> Option<u64> {
    state
        .connections
        .iter()
        .find(|(_, e)| e.conn.local_connection_id() == receiver_connection_id)
        .map(|(&id, _)| id)
}

pub(crate) fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();
    let sig = identity.sign(transcript_hash);
    let mut payload = Vec::new();
    payload.extend_from_slice(&pk.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());
    payload
}

pub(crate) async fn send_packets(shared: &Shared, path: &TransportPath, packets: &[Data]) {
    match path {
        TransportPath::Direct { addr } => {
            for data in packets {
                let _ = shared.socket.send_to(&data.encode(), *addr).await;
            }
        }
        TransportPath::Relayed {
            gateway_connection_id,
            dest_peer_id,
        } => {
            let mut state = shared.state.lock().await;
            if let Some(gw_entry) = state.connections.get_mut(gateway_connection_id) {
                for data in packets {
                    gw_entry
                        .conn
                        .queue_frame(Frame::GatewayPacket(GatewayPacket {
                            dest_peer_id: dest_peer_id.clone(),
                            src_peer_id: shared.identity.public_key().peer_id(),
                            inner: data.encode(),
                        }));
                }
                let gw_path = gw_entry.path.clone();
                let gw_packets = gw_entry.conn.poll_packets(Instant::now());
                drop(state);
                if let TransportPath::Direct { addr } = gw_path {
                    for pkt in gw_packets {
                        let _ = shared.socket.send_to(&pkt.encode(), addr).await;
                    }
                }
            }
        }
    }
}

pub(crate) async fn flush_connection_locked(
    state: &mut TransportState,
    shared: &Shared,
    connection_id: u64,
) {
    if let Some(entry) = state.connections.get_mut(&connection_id) {
        let packets = entry.conn.poll_packets(Instant::now());
        if !packets.is_empty() {
            debug!(me = %short_pid(shared), connection_id, count = packets.len(), "flush_locked: sending");
        }
        let path = entry.path.clone();
        match path {
            TransportPath::Direct { addr } => {
                for data in packets {
                    let _ = shared.socket.send_to(&data.encode(), addr).await;
                }
            }
            _ => {}
        }
    }
}

pub(crate) async fn process_unhandled_frame(shared: &Shared, connection_id: u64, frame: Frame) {
    let is_gateway = shared.gateway_mode.load(Ordering::SeqCst);
    if is_gateway {
        process_gateway_server_frame(shared, connection_id, &frame).await;
    }

    match frame {
        Frame::GatewayPacket(pkt) if !is_gateway => {
            process_gateway_packet_client(shared, pkt).await;
        }
        Frame::GatewayRegisterAck(ack) => {
            let mut state = shared.state.lock().await;
            if let Some(gw) = &mut state.gateway {
                gw.registered = ack.status == 0;
                gw.relay_mtu = ack.relay_mtu;
            }
            drop(state);
            shared.gateway_notify.notify_waiters();
        }
        Frame::DhtQueryReply(reply) => {
            debug!(
                request_id = reply.request_id,
                status = reply.status,
                frag = reply.fragment_index,
                total = reply.fragment_total,
                data_len = reply.data.len(),
                "client: received DhtQueryReply fragment"
            );
            let mut state = shared.state.lock().await;

            // Reassemble fragmented DhtQueryReply
            let assembled =
                if reply.fragment_total <= 1 {
                    Some(reply)
                } else {
                    let key = reply.request_id as u64 | 0x8000_0000_0000_0000;
                    let collector = state.dht_publish_fragments.entry(key).or_insert_with(|| {
                        DhtPublishCollector {
                            fragments: vec![None; reply.fragment_total as usize],
                            received: 0,
                            total: reply.fragment_total,
                        }
                    });
                    let idx = reply.fragment_index as usize;
                    if idx < collector.fragments.len() && collector.fragments[idx].is_none() {
                        collector.fragments[idx] = Some(reply.data.clone());
                        collector.received += 1;
                    }
                    if collector.received == collector.total {
                        let data: Vec<u8> = collector
                            .fragments
                            .iter()
                            .filter_map(|f| f.as_ref())
                            .flat_map(|f| f.iter().copied())
                            .collect();
                        state.dht_publish_fragments.remove(&key);
                        Some(crate::wire::DhtQueryReply {
                            request_id: reply.request_id,
                            status: reply.status,
                            fragment_index: 0,
                            fragment_total: 1,
                            data,
                        })
                    } else {
                        None
                    }
                };

            if let Some(reply) = assembled {
                if let Some(sender) = state.pending_dht_queries.remove(&reply.request_id) {
                    let record = if reply.status == 0 {
                        DhtRecord::decode(&reply.data).ok().filter(|r| r.verify())
                    } else {
                        None
                    };
                    let _ = sender.send(record);
                }
            }
        }
        Frame::HolePunchNotify(notify) => {
            let hole_punch_bytes = Packet::HolePunch(HolePunch {
                sender_peer_id: shared.identity.public_key().peer_id(),
            })
            .encode();
            for addr in &notify.addrs {
                let _ = shared.socket.send_to(&hole_punch_bytes, *addr).await;
            }
            let mut state = shared.state.lock().await;
            for addr in notify.addrs {
                state.hole_punches.push(HolePunchEntry {
                    peer_addr: addr,
                    next_send: Instant::now() + HOLE_PUNCH_INTERVAL,
                    remaining: HOLE_PUNCH_COUNT - 1,
                });
            }
        }
        _ => {}
    }
}

async fn process_gateway_server_frame(shared: &Shared, connection_id: u64, frame: &Frame) {
    crate::relay::server::process_relay_frame(shared, connection_id, frame).await;
    crate::registry::server::process_registry_frame(shared, connection_id, frame).await;
}

pub(crate) use crate::registry::client::process_dht_actions;
pub(crate) use crate::registry::client::queue_fragmented_query_reply;

async fn process_gateway_packet_client(shared: &Shared, pkt: GatewayPacket) {
    crate::relay::client::process_gateway_packet_client(shared, pkt).await;
}

fn channel_err_to_io(e: ChannelError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
