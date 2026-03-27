use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::task::JoinHandle;

use crate::crypto::PeerId as PeerIdType;
use crate::crypto::{
    EncryptionKeys, EphemeralPrivateKey, PeerId, PrivateKey, PublicKey, compute_transcript_hash,
};
use crate::discovery::{ConnectionRequest, Discovery, DiscoveryFactory, RouteInfo};
use crate::net::PeerConnection;
use crate::raw::{RouteMap, TransportSocket};
use crate::session::{Role, Session};
use crate::stream::StreamError;
use crate::wire::packet::{Data, HolePunch, Packet};
use crate::wire::{Frame, GatewayRelay, KeyExchangeInit, KeyExchangeResponse};

const RECV_BUF_SIZE: usize = 2048;
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const HOLE_PUNCH_COUNT: u8 = 4;
const HOLE_PUNCH_INTERVAL: Duration = Duration::from_millis(150);

pub struct Node {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
}

struct Shared {
    socket: Arc<UdpSocket>,
    identity: PrivateKey,
    discovery: Arc<dyn Discovery>,
    routes: RouteMap,
    state: TokioMutex<TransportState>,
    pending_close: std::sync::Mutex<Vec<u64>>,
    ping_counter: AtomicU32,
    accept_notify: Notify,
    established_notify: Notify,
    data_notify: Notify,
    stream_notify: Notify,
    gateway_notify: Notify,
    gateway_mode: AtomicBool,
}

struct TransportState {
    connections: HashMap<u64, ConnEntry>,
    pending_connects: HashMap<u64, PendingConnect>,
    accept_queue: VecDeque<u64>,
    next_session_id: u64,
    hole_punches: Vec<HolePunchEntry>,
    gateway: Option<GatewayState>,
    gateway_clients: HashMap<PeerId, RegisteredClient>,
}

#[derive(Clone)]
struct RegisteredClient {
    session_id: u64,
    external_addr: SocketAddr,
}

struct GatewayState {
    session_id: u64,
    registered: bool,
    relay_mtu: u16,
}

struct HolePunchEntry {
    peer_addr: SocketAddr,
    next_send: Instant,
    remaining: u8,
}

#[derive(Debug, Clone)]
pub enum TransportPath {
    Direct {
        addr: SocketAddr,
    },
    Relayed {
        gateway_session_id: u64,
        dest_peer_id: PeerIdType,
    },
}

struct ConnEntry {
    path: TransportPath,
    conn: Box<PeerConnection>,
    last_recv: Instant,
    last_ping_sent: Instant,
    closed: bool,
    is_local_initiator: bool,
}

struct PendingConnect {
    ephemeral_key: Box<EphemeralPrivateKey>,
    peer_addr: SocketAddr,
    relayed: bool,
    target_peer_id: Option<PeerId>,
}

impl Node {
    pub async fn bind(
        addr: SocketAddr,
        identity: PrivateKey,
        factory: &dyn DiscoveryFactory,
    ) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let routes = RouteMap::default();
        let transport_socket = TransportSocket::new(socket.clone(), routes.clone());
        let discovery = factory.create(&transport_socket).await?;
        Self::init(socket, routes, identity, discovery).await
    }

    async fn init(
        socket: Arc<UdpSocket>,
        routes: RouteMap,
        identity: PrivateKey,
        discovery: Arc<dyn Discovery>,
    ) -> io::Result<Self> {
        let local_addr = socket.local_addr()?;
        let peer_id = identity.public_key().peer_id();

        let shared = Arc::new(Shared {
            socket: socket.clone(),
            identity,
            discovery,
            routes,
            state: TokioMutex::new(TransportState {
                connections: HashMap::new(),
                pending_connects: HashMap::new(),
                accept_queue: VecDeque::new(),
                next_session_id: 1,
                hole_punches: Vec::new(),
                gateway: None,
                gateway_clients: HashMap::new(),
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

        shared.discovery.register(peer_id, local_addr).await;

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
        let route = self
            .shared
            .discovery
            .resolve(peer_id)
            .await
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "peer not found in discovery")
            })?;

        match route {
            RouteInfo::Direct(addr) => self.connect_to_addr(addr, Some(peer_id.clone())).await,
            RouteInfo::Relayed { gateway_addr: _ } => self.connect_via_relay(peer_id).await,
        }
    }

    async fn connect_to_addr(
        &self,
        peer_addr: SocketAddr,
        target_peer_id: Option<PeerId>,
    ) -> io::Result<Connection> {
        let session_id = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_session_id;
            state.next_session_id += 1;

            let eph = Box::new(EphemeralPrivateKey::generate());
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
                initiator_session_id: sid,
                target_peer_id: target_peer_id.unwrap_or(PeerId::zero()),
                ephemeral_public_key: *eph_pk,
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
                },
            );

            sid
        };

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.established_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(entry) = state.connections.get(&session_id) {
                        if entry.conn.is_established() {
                            return Ok(Connection {
                                shared: self.shared.clone(),
                                session_id,
                                closed: AtomicBool::new(false),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.shared.state.lock().await;
                    state.pending_connects.remove(&session_id);
                    state.connections.remove(&session_id);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "handshake timed out",
                    ));
                }
            }
        }
    }

    pub async fn connect_addr(&self, addr: SocketAddr) -> io::Result<Connection> {
        self.connect_to_addr(addr, None).await
    }

    pub fn enable_gateway(&self) {
        self.shared.gateway_mode.store(true, Ordering::SeqCst);
    }

    pub fn is_gateway(&self) -> bool {
        self.shared.gateway_mode.load(Ordering::SeqCst)
    }

    async fn connect_via_relay(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let session_id = {
            let mut state = self.shared.state.lock().await;
            let gw = state.gateway.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "not connected to gateway")
            })?;
            let gw_session_id = gw.session_id;

            let sid = state.next_session_id;
            state.next_session_id += 1;

            let eph = Box::new(EphemeralPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let init = KeyExchangeInit {
                initiator_session_id: sid,
                target_peer_id: peer_id.clone(),
                ephemeral_public_key: *eph_pk,
            };
            let init_bytes = init.encode();

            if let Some(gw_entry) = state.connections.get_mut(&gw_session_id) {
                gw_entry.conn.queue_frame(Frame::GatewayRelay(GatewayRelay {
                    dest_peer_id: peer_id.clone(),
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
                },
            );

            flush_connection_locked(&mut state, &self.shared, gw_session_id).await;

            sid
        };

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.established_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(entry) = state.connections.get(&session_id) {
                        if entry.conn.is_established() {
                            return Ok(Connection {
                                shared: self.shared.clone(),
                                session_id,
                                closed: AtomicBool::new(false),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = self.shared.state.lock().await;
                    state.pending_connects.remove(&session_id);
                    state.connections.remove(&session_id);
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

        let gw_conn = self.connect_addr(bootstrap_addr).await?;
        let gw_session_id = gw_conn.session_id();
        std::mem::forget(gw_conn);

        let local_peer_id = self.shared.identity.public_key().peer_id();
        {
            let mut state = self.shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&gw_session_id) {
                entry
                    .conn
                    .queue_frame(Frame::GatewayRegister(crate::wire::GatewayRegister {
                        peer_id: local_peer_id,
                        flags: 0,
                        auth_data: Vec::new(),
                    }));
            }
            state.gateway = Some(GatewayState {
                session_id: gw_session_id,
                registered: false,
                relay_mtu: 0,
            });
        }
        flush_connection(&self.shared, gw_session_id).await?;

        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.gateway_notify.notified() => {
                    let state = self.shared.state.lock().await;
                    if let Some(gw) = &state.gateway {
                        if gw.registered {
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
                if let Some(session_id) = state.accept_queue.pop_front() {
                    return Ok(Connection {
                        shared: self.shared.clone(),
                        session_id,
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

pub struct Connection {
    shared: Arc<Shared>,
    session_id: u64,
    closed: AtomicBool,
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.shared
                .pending_close
                .lock()
                .unwrap()
                .push(self.session_id);
        }
    }
}

impl Connection {
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub async fn peer_public_key(&self) -> Option<PublicKey> {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.session_id)
            .and_then(|e| e.conn.peer_public_key().cloned())
    }

    pub async fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().await.map(|pk| pk.peer_id())
    }

    pub async fn transport_path(&self) -> Option<TransportPath> {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.session_id)
            .map(|e| e.path.clone())
    }

    pub async fn is_established(&self) -> bool {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.session_id)
            .map_or(false, |e| e.conn.is_established() && !e.closed)
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut state = self.shared.state.lock().await;
        if let Some(entry) = state.connections.get_mut(&self.session_id) {
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

    pub async fn open_stream(&self, purpose: u16) -> io::Result<ReliableStream> {
        let stream_id = {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_stream(purpose)
        };
        flush_connection(&self.shared, self.session_id).await?;
        Ok(ReliableStream {
            shared: self.shared.clone(),
            session_id: self.session_id,
            stream_id,
        })
    }

    pub async fn accept_stream(&self) -> io::Result<(ReliableStream, u16)> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                if let Some(entry) = state.connections.get_mut(&self.session_id) {
                    if let Some((stream_id, purpose)) = entry.conn.accept_stream() {
                        return Ok((
                            ReliableStream {
                                shared: self.shared.clone(),
                                session_id: self.session_id,
                                stream_id,
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

    pub async fn open_datagram_stream(&self, purpose: u16) -> io::Result<DatagramStream> {
        let stream_id = {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_datagram(purpose)
        };
        flush_connection(&self.shared, self.session_id).await?;
        Ok(DatagramStream {
            shared: self.shared.clone(),
            session_id: self.session_id,
            stream_id,
        })
    }

    pub async fn accept_datagram_stream(&self) -> io::Result<(DatagramStream, u16)> {
        let (stream, purpose) = self.accept_stream().await?;
        Ok((
            DatagramStream {
                shared: stream.shared,
                session_id: stream.session_id,
                stream_id: stream.stream_id,
            },
            purpose,
        ))
    }
}

pub struct ReliableStream {
    shared: Arc<Shared>,
    session_id: u64,
    stream_id: u32,
}

pub struct DatagramStream {
    shared: Arc<Shared>,
    session_id: u64,
    stream_id: u32,
}

impl ReliableStream {
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write(self.stream_id, data)
                .map_err(stream_err_to_io)?;
        }
        flush_connection(&self.shared, self.session_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                let entry = state.connections.get_mut(&self.session_id).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                })?;
                match entry.conn.read(self.stream_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(stream_err_to_io(e)),
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
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_stream(self.stream_id)
                .map_err(stream_err_to_io)?;
        }
        flush_connection(&self.shared, self.session_id).await?;
        Ok(())
    }
}

impl DatagramStream {
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().await;
            let entry = state
                .connections
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write_datagram(self.stream_id, data)
                .map_err(stream_err_to_io)?;
        }
        flush_connection(&self.shared, self.session_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().await;
                let entry = state.connections.get_mut(&self.session_id).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                })?;
                match entry.conn.read_datagram(self.stream_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(stream_err_to_io(e)),
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
                .get_mut(&self.session_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_stream(self.stream_id)
                .map_err(stream_err_to_io)?;
        }
        flush_connection(&self.shared, self.session_id).await?;
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
                        if route_to_raw(&shared.routes, &buf[..len], addr) {
                            continue;
                        }
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
            request = shared.discovery.recv_connection_request() => {
                handle_connection_request(&shared, request).await;
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

async fn handle_connection_request(shared: &Shared, request: ConnectionRequest) {
    let hole_punch_bytes = Packet::HolePunch(HolePunch {
        sender_peer_id: shared.identity.public_key().peer_id(),
    })
    .encode();
    let _ = shared
        .socket
        .send_to(&hole_punch_bytes, request.peer_addr)
        .await;

    let mut state = shared.state.lock().await;
    state.hole_punches.push(HolePunchEntry {
        peer_addr: request.peer_addr,
        next_send: Instant::now() + HOLE_PUNCH_INTERVAL,
        remaining: HOLE_PUNCH_COUNT - 1,
    });
}

async fn handle_key_exchange_init(shared: &Shared, init: Box<KeyExchangeInit>, addr: SocketAddr) {
    shared
        .state
        .lock()
        .await
        .hole_punches
        .retain(|e| e.peer_addr != addr);

    let resp_eph = Box::new(EphemeralPrivateKey::generate());
    let (ct, resp_ss) = match resp_eph.encapsulate(&init.ephemeral_public_key) {
        Some(pair) => pair,
        None => return,
    };
    let ct = Box::new(ct);

    let keys = EncryptionKeys::new(&resp_ss, &init.ephemeral_public_key, &ct);
    let th = compute_transcript_hash(&init.ephemeral_public_key, &ct);

    let mut state = shared.state.lock().await;
    let local_sid = state.next_session_id;
    state.next_session_id += 1;

    let response = Box::new(KeyExchangeResponse {
        responder_session_id: local_sid,
        initiator_session_id: init.initiator_session_id,
        kem_ciphertext: *ct,
    });

    let _ = shared.socket.send_to(&response.encode(), addr).await;

    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = PeerConnection::new(
        session,
        local_sid,
        init.initiator_session_id,
        false,
        auth_payload,
    );

    let entry = ConnEntry {
        path: TransportPath::Direct { addr },
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: false,
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

async fn handle_key_exchange_response(
    shared: &Shared,
    resp: Box<KeyExchangeResponse>,
    addr: SocketAddr,
) {
    let mut state = shared.state.lock().await;
    state.hole_punches.retain(|e| e.peer_addr != addr);

    let pending = match state.pending_connects.remove(&resp.initiator_session_id) {
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
        resp.initiator_session_id,
        resp.responder_session_id,
        true,
        auth_payload,
    );

    let path = if pending.relayed {
        let gw_sid = state.gateway.as_ref().map(|g| g.session_id).unwrap_or(0);
        TransportPath::Relayed {
            gateway_session_id: gw_sid,
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
    };
    state.connections.insert(resp.initiator_session_id, entry);

    let packets = state
        .connections
        .get_mut(&resp.initiator_session_id)
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

    let session_id = match find_session_by_receiver(&state, data.receiver_session_id) {
        Some(id) => id,
        None => return,
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
    ) = {
        let entry = match state.connections.get_mut(&session_id) {
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
        )
    };

    if got_close {
        state.connections.remove(&session_id);
    }

    if !was_established && is_established && !is_local_initiator {
        state.accept_queue.push_back(session_id);
    }
    drop(state);

    send_packets(shared, &path, &packets).await;

    for frame in unhandled {
        process_unhandled_frame(shared, session_id, frame).await;
    }

    if !was_established && is_established {
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

async fn flush_all(shared: &Shared) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let closes: Vec<u64> = shared.pending_close.lock().unwrap().drain(..).collect();
    for session_id in closes {
        if let Some(entry) = state.connections.get_mut(&session_id) {
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

    let gw_session_id = state.gateway.as_ref().map(|g| g.session_id);

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
        if Some(sid) == gw_session_id {
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
                        relay_frames.push(Frame::GatewayRelay(GatewayRelay {
                            dest_peer_id: dest_peer_id.clone(),
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

    if let Some(gw_sid) = gw_session_id {
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

async fn flush_connection(shared: &Shared, session_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (path, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&session_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let packets = entry.conn.poll_packets(now);
        (entry.path.clone(), packets)
    };

    send_packets(shared, &path, &packets).await;
    Ok(())
}

fn find_session_by_receiver(state: &TransportState, receiver_session_id: u64) -> Option<u64> {
    state
        .connections
        .iter()
        .find(|(_, e)| e.conn.local_session_id() == receiver_session_id)
        .map(|(&id, _)| id)
}

fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();
    let sig = identity.sign(transcript_hash);
    let mut payload = Vec::new();
    payload.extend_from_slice(&pk.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());
    payload
}

fn route_to_raw(routes: &RouteMap, data: &[u8], addr: SocketAddr) -> bool {
    let map = routes.read().unwrap();
    if let Some(sender) = map.get(&addr) {
        let _ = sender.try_send(data.to_vec());
        return true;
    }
    false
}

async fn send_packets(shared: &Shared, path: &TransportPath, packets: &[Data]) {
    match path {
        TransportPath::Direct { addr } => {
            for data in packets {
                let _ = shared.socket.send_to(&data.encode(), *addr).await;
            }
        }
        TransportPath::Relayed {
            gateway_session_id,
            dest_peer_id,
        } => {
            let mut state = shared.state.lock().await;
            if let Some(gw_entry) = state.connections.get_mut(gateway_session_id) {
                for data in packets {
                    gw_entry.conn.queue_frame(Frame::GatewayRelay(GatewayRelay {
                        dest_peer_id: dest_peer_id.clone(),
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

async fn flush_connection_locked(state: &mut TransportState, shared: &Shared, session_id: u64) {
    if let Some(entry) = state.connections.get_mut(&session_id) {
        let packets = entry.conn.poll_packets(Instant::now());
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

async fn process_unhandled_frame(shared: &Shared, session_id: u64, frame: Frame) {
    if shared.gateway_mode.load(Ordering::SeqCst) {
        process_gateway_server_frame(shared, session_id, &frame).await;
    }

    match frame {
        Frame::GatewayDeliver(deliver) => {
            process_gateway_deliver(shared, deliver).await;
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

async fn process_gateway_server_frame(shared: &Shared, session_id: u64, frame: &Frame) {
    match frame {
        Frame::GatewayRegister(reg) => {
            let mut state = shared.state.lock().await;
            let external_addr = match state.connections.get(&session_id) {
                Some(entry) => match &entry.path {
                    TransportPath::Direct { addr } => *addr,
                    _ => return,
                },
                None => return,
            };
            state.gateway_clients.insert(
                reg.peer_id,
                RegisteredClient {
                    session_id,
                    external_addr,
                },
            );
            if let Some(entry) = state.connections.get_mut(&session_id) {
                entry.conn.queue_frame(Frame::GatewayRegisterAck(
                    crate::wire::GatewayRegisterAck {
                        status: 0,
                        relay_mtu: (crate::wire::packet::INITIAL_MTU
                            - crate::wire::packet::PACKET_OVERHEAD
                            - 36) as u16,
                    },
                ));
            }
            drop(state);
            flush_connection(shared, session_id).await.ok();
        }
        Frame::GatewayRelay(relay) => {
            let state = shared.state.lock().await;
            let sender_peer_id = state
                .connections
                .get(&session_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let dest_client = state.gateway_clients.get(&relay.dest_peer_id).cloned();
            drop(state);

            let src_peer_id = match sender_peer_id {
                Some(id) => id,
                None => return,
            };
            let dest = match dest_client {
                Some(c) => c,
                None => return,
            };

            let deliver = Frame::GatewayDeliver(crate::wire::GatewayDeliver {
                src_peer_id,
                inner: relay.inner.clone(),
            });

            let mut state = shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&dest.session_id) {
                entry.conn.queue_frame(deliver);
            }
            drop(state);
            flush_connection(shared, dest.session_id).await.ok();
        }
        Frame::HolePunchRequest(req) => {
            let state = shared.state.lock().await;
            let requester_addr = state
                .connections
                .get(&session_id)
                .and_then(|e| match &e.path {
                    TransportPath::Direct { addr } => Some(*addr),
                    _ => None,
                });
            let requester_peer_id = state
                .connections
                .get(&session_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let target_client = state.gateway_clients.get(&req.target_peer_id).cloned();
            drop(state);

            let requester_id = match requester_peer_id {
                Some(id) => id,
                None => return,
            };
            let req_addr = match requester_addr {
                Some(a) => a,
                None => return,
            };
            let target = match target_client {
                Some(c) => c,
                None => return,
            };

            let notify = Frame::HolePunchNotify(crate::wire::HolePunchNotify {
                requester_peer_id: requester_id,
                addrs: vec![req_addr],
            });

            let mut state = shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&target.session_id) {
                entry.conn.queue_frame(notify);
            }
            drop(state);
            flush_connection(shared, target.session_id).await.ok();
        }
        _ => {}
    }
}

async fn process_gateway_deliver(shared: &Shared, deliver: crate::wire::GatewayDeliver) {
    let packet = match Packet::decode(&deliver.inner) {
        Ok(p) => p,
        Err(_) => return,
    };

    match packet {
        Packet::KeyExchangeInit(init) => {
            handle_key_exchange_init_relayed(shared, Box::new(init), deliver.src_peer_id).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            handle_key_exchange_response(
                shared,
                Box::new(resp),
                SocketAddr::from(([0, 0, 0, 0], 0)),
            )
            .await;
        }
        Packet::Data(data) => {
            process_relayed_data(shared, data).await;
        }
        _ => {}
    }
}

async fn handle_key_exchange_init_relayed(
    shared: &Shared,
    init: Box<KeyExchangeInit>,
    src_peer_id: PeerId,
) {
    let resp_eph = Box::new(EphemeralPrivateKey::generate());
    let (ct, resp_ss) = match resp_eph.encapsulate(&init.ephemeral_public_key) {
        Some(pair) => pair,
        None => return,
    };
    let ct = Box::new(ct);

    let keys = EncryptionKeys::new(&resp_ss, &init.ephemeral_public_key, &ct);
    let th = compute_transcript_hash(&init.ephemeral_public_key, &ct);

    let mut state = shared.state.lock().await;
    let gw = match &state.gateway {
        Some(g) => g.session_id,
        None => return,
    };

    let local_sid = state.next_session_id;
    state.next_session_id += 1;

    let response = KeyExchangeResponse {
        responder_session_id: local_sid,
        initiator_session_id: init.initiator_session_id,
        kem_ciphertext: *ct,
    };
    let resp_bytes = response.encode();

    if let Some(gw_entry) = state.connections.get_mut(&gw) {
        gw_entry.conn.queue_frame(Frame::GatewayRelay(GatewayRelay {
            dest_peer_id: src_peer_id.clone(),
            inner: resp_bytes,
        }));
    }

    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = PeerConnection::new(
        session,
        local_sid,
        init.initiator_session_id,
        false,
        auth_payload,
    );

    let path = TransportPath::Relayed {
        gateway_session_id: gw,
        dest_peer_id: src_peer_id,
    };

    let entry = ConnEntry {
        path,
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: false,
    };
    state.connections.insert(local_sid, entry);

    flush_connection_locked(&mut state, shared, gw).await;

    let packets = state
        .connections
        .get_mut(&local_sid)
        .unwrap()
        .conn
        .poll_packets(Instant::now());

    let relay_path = state.connections.get(&local_sid).unwrap().path.clone();
    drop(state);

    send_packets(shared, &relay_path, &packets).await;
}

async fn process_relayed_data(shared: &Shared, data: Data) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let session_id = match find_session_by_receiver(&state, data.receiver_session_id) {
        Some(id) => id,
        None => return,
    };

    let entry = match state.connections.get_mut(&session_id) {
        Some(e) => e,
        None => return,
    };

    let was_established = entry.conn.is_established();
    let had_close = entry.conn.got_connection_close();
    let _unhandled = entry.conn.on_data_packet(data, now);
    entry.last_recv = now;
    let is_established = entry.conn.is_established();
    let has_new_stream = entry.conn.has_pending_accept();
    let got_close = !had_close && entry.conn.got_connection_close();
    let is_local_initiator = entry.is_local_initiator;
    if got_close {
        entry.closed = true;
    }

    if got_close {
        state.connections.remove(&session_id);
    }
    if !was_established && is_established && !is_local_initiator {
        state.accept_queue.push_back(session_id);
    }
    drop(state);

    if !was_established && is_established {
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

fn stream_err_to_io(e: StreamError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
