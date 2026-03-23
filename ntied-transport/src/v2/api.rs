use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::task::JoinHandle;

use super::crypto::compute_transcript_hash;
use super::crypto::{EncryptionKeys, EphemeralPrivateKey, PeerId, PrivateKey, PublicKey};
use super::discovery::Discovery;
use super::net::PeerConnection;
use super::session::{Role, Session};
use super::stream::StreamError;
use super::wire::packet::{Data, HolePunch, Packet};
use super::wire::{KeyExchangeInit, KeyExchangeResponse};

const RECV_BUF_SIZE: usize = 2048;
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const HOLE_PUNCH_COUNT: u8 = 4;
const HOLE_PUNCH_INTERVAL: Duration = Duration::from_millis(150);

pub struct Transport {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
}

struct Shared {
    socket: Arc<UdpSocket>,
    identity: PrivateKey,
    discovery: Arc<dyn Discovery>,
    state: TokioMutex<TransportState>,
    pending_close: std::sync::Mutex<Vec<u64>>,
    ping_counter: AtomicU32,
    accept_notify: Notify,
    established_notify: Notify,
    data_notify: Notify,
    stream_notify: Notify,
}

struct TransportState {
    connections: HashMap<u64, ConnEntry>,
    pending_connects: HashMap<u64, PendingConnect>,
    accept_queue: VecDeque<u64>,
    next_session_id: u64,
    hole_punches: Vec<HolePunchEntry>,
}

struct HolePunchEntry {
    peer_addr: SocketAddr,
    next_send: Instant,
    remaining: u8,
}

struct ConnEntry {
    peer_addr: SocketAddr,
    conn: Box<PeerConnection>,
    last_recv: Instant,
    last_ping_sent: Instant,
    closed: bool,
}

struct PendingConnect {
    ephemeral_key: Box<EphemeralPrivateKey>,
}

impl Transport {
    pub async fn bind(
        addr: SocketAddr,
        identity: PrivateKey,
        discovery: Arc<dyn Discovery>,
    ) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let local_addr = socket.local_addr()?;
        let peer_id = identity.public_key().peer_id();
        discovery.register(peer_id, local_addr).await;

        let shared = Arc::new(Shared {
            socket: socket.clone(),
            identity,
            discovery,
            state: TokioMutex::new(TransportState {
                connections: HashMap::new(),
                pending_connects: HashMap::new(),
                accept_queue: VecDeque::new(),
                next_session_id: 1,
                hole_punches: Vec::new(),
            }),
            pending_close: std::sync::Mutex::new(Vec::new()),
            ping_counter: AtomicU32::new(1),
            accept_notify: Notify::new(),
            established_notify: Notify::new(),
            data_notify: Notify::new(),
            stream_notify: Notify::new(),
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

    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let peer_addr = self
            .shared
            .discovery
            .resolve(peer_id)
            .await
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "peer not found in discovery")
            })?;

        let session_id = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_session_id;
            state.next_session_id += 1;

            let eph = Box::new(EphemeralPrivateKey::generate());
            let eph_pk = eph.public_key();

            let hole_punch = HolePunch {
                sender_peer_id: self.shared.identity.public_key().peer_id(),
            };
            let _ = self
                .shared
                .socket
                .send_to(&Packet::HolePunch(hole_punch).encode(), peer_addr)
                .await;

            state.hole_punches.push(HolePunchEntry {
                peer_addr,
                next_send: Instant::now() + HOLE_PUNCH_INTERVAL,
                remaining: HOLE_PUNCH_COUNT - 1,
            });

            let init = KeyExchangeInit {
                initiator_session_id: sid,
                target_peer_id: peer_id.clone(),
                ephemeral_public_key: eph_pk,
            };
            self.shared
                .socket
                .send_to(&init.encode(), peer_addr)
                .await?;

            state
                .pending_connects
                .insert(sid, PendingConnect { ephemeral_key: eph });

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
                let addr = entry.peer_addr;
                drop(state);
                for pkt in packets {
                    let _ = self.shared.socket.send_to(&pkt.encode(), addr).await;
                }
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
    let mut buf = [0u8; RECV_BUF_SIZE];
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
            request = shared.discovery.recv_connection_request() => {
                handle_connection_request(&shared, request).await;
            }
        }
    }
}

async fn process_packet(shared: &Shared, buf: &[u8], addr: SocketAddr) {
    let packet = match Packet::decode(buf) {
        Ok(p) => p,
        Err(_) => return,
    };

    match packet {
        Packet::KeyExchangeInit(init) => {
            handle_key_exchange_init(shared, init, addr).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            handle_key_exchange_response(shared, resp, addr).await;
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
        Packet::Relay(_) => {}
    }
}

async fn handle_connection_request(
    shared: &Shared,
    request: crate::v2::discovery::ConnectionRequest,
) {
    let hole_punch = HolePunch {
        sender_peer_id: shared.identity.public_key().peer_id(),
    };
    let _ = shared
        .socket
        .send_to(&Packet::HolePunch(hole_punch).encode(), request.peer_addr)
        .await;

    let mut state = shared.state.lock().await;
    state.hole_punches.push(HolePunchEntry {
        peer_addr: request.peer_addr,
        next_send: Instant::now() + HOLE_PUNCH_INTERVAL,
        remaining: HOLE_PUNCH_COUNT - 1,
    });
}

async fn handle_key_exchange_init(shared: &Shared, init: KeyExchangeInit, addr: SocketAddr) {
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

    let keys = EncryptionKeys::new(&resp_ss, &init.ephemeral_public_key, &ct);
    let th = compute_transcript_hash(&init.ephemeral_public_key, &ct);

    let mut state = shared.state.lock().await;
    let local_sid = state.next_session_id;
    state.next_session_id += 1;

    let response = KeyExchangeResponse {
        responder_session_id: local_sid,
        initiator_session_id: init.initiator_session_id,
        kem_ciphertext: ct,
    };

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
        peer_addr: addr,
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
    };
    state.connections.insert(local_sid, entry);

    let packets = state
        .connections
        .get_mut(&local_sid)
        .unwrap()
        .conn
        .poll_packets(Instant::now());
    drop(state);

    for data in packets {
        let _ = shared.socket.send_to(&data.encode(), addr).await;
    }
}

async fn handle_key_exchange_response(
    shared: &Shared,
    resp: KeyExchangeResponse,
    addr: SocketAddr,
) {
    let mut state = shared.state.lock().await;
    state.hole_punches.retain(|e| e.peer_addr != addr);

    let pending = match state.pending_connects.remove(&resp.initiator_session_id) {
        Some(p) => p,
        None => return,
    };

    let init_pk = pending.ephemeral_key.public_key();
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

    let entry = ConnEntry {
        peer_addr: addr,
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
    };
    state.connections.insert(resp.initiator_session_id, entry);

    let packets = state
        .connections
        .get_mut(&resp.initiator_session_id)
        .unwrap()
        .conn
        .poll_packets(Instant::now());
    drop(state);

    for data in packets {
        let _ = shared.socket.send_to(&data.encode(), addr).await;
    }
}

async fn handle_data(shared: &Shared, data: Data, _addr: SocketAddr) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    state.hole_punches.retain(|e| e.peer_addr != _addr);

    let session_id = match find_session_by_receiver(&state, data.receiver_session_id) {
        Some(id) => id,
        None => return,
    };

    let (was_established, is_established, has_new_stream, got_close, packets, peer_addr) = {
        let entry = match state.connections.get_mut(&session_id) {
            Some(e) => e,
            None => return,
        };

        let was_established = entry.conn.is_established();
        let had_close = entry.conn.got_connection_close();
        entry.conn.on_data_packet(data, now);
        entry.last_recv = now;
        let is_established = entry.conn.is_established();
        let has_new_stream = entry.conn.has_pending_accept();
        let got_close = !had_close && entry.conn.got_connection_close();
        if got_close {
            entry.closed = true;
        }
        let packets = entry.conn.poll_packets(now);
        let peer_addr = entry.peer_addr;

        (
            was_established,
            is_established,
            has_new_stream,
            got_close,
            packets,
            peer_addr,
        )
    };

    if got_close {
        state.connections.remove(&session_id);
    }

    if !was_established && is_established {
        state.accept_queue.push_back(session_id);
    }
    drop(state);

    for pkt in packets {
        let _ = shared.socket.send_to(&pkt.encode(), peer_addr).await;
    }

    if !was_established && is_established {
        shared.accept_notify.notify_waiters();
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

    let mut to_send: Vec<(SocketAddr, Vec<Data>)> = Vec::new();
    let mut to_remove: Vec<u64> = Vec::new();
    for (&sid, entry) in state.connections.iter_mut() {
        let packets = entry.conn.poll_packets(now);
        if !packets.is_empty() {
            to_send.push((entry.peer_addr, packets));
        }
        if entry.closed && !entry.conn.has_pending() {
            to_remove.push(sid);
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

    for (addr, packets) in to_send {
        for data in packets {
            let _ = shared.socket.send_to(&data.encode(), addr).await;
        }
    }
}

async fn flush_connection(shared: &Shared, session_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (addr, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&session_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let packets = entry.conn.poll_packets(now);
        (entry.peer_addr, packets)
    };

    for data in packets {
        shared.socket.send_to(&data.encode(), addr).await?;
    }
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

fn stream_err_to_io(e: StreamError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
