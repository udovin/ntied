use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::v2::crypto::{
    EncryptionKeys, EphemeralPrivateKey, PeerId, PrivateKey, compute_transcript_hash,
};
use crate::v2::discovery::Discovery;
use crate::v2::net::PeerConnection;
use crate::v2::session::{Role, Session};
use crate::v2::wire::packet::{Data, Packet};

const RECV_BUF_SIZE: usize = 2048;
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Transport {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
}

struct Shared {
    socket: Arc<UdpSocket>,
    identity: PrivateKey,
    discovery: Arc<dyn Discovery>,
    state: Mutex<TransportState>,
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
}

struct ConnEntry {
    peer_addr: SocketAddr,
    conn: Box<PeerConnection>,
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
            state: Mutex::new(TransportState {
                connections: HashMap::new(),
                pending_connects: HashMap::new(),
                accept_queue: VecDeque::new(),
                next_session_id: 1,
            }),
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

            let init = crate::v2::wire::packet::KeyExchangeInit {
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
}

impl Connection {
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub async fn is_established(&self) -> bool {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.session_id)
            .map_or(false, |e| e.conn.is_established())
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
}

pub struct ReliableStream {
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
        _ => {}
    }
}

async fn handle_key_exchange_init(
    shared: &Shared,
    init: crate::v2::wire::packet::KeyExchangeInit,
    addr: SocketAddr,
) {
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

    let response = crate::v2::wire::packet::KeyExchangeResponse {
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
    resp: crate::v2::wire::packet::KeyExchangeResponse,
    addr: SocketAddr,
) {
    let mut state = shared.state.lock().await;

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

    let session_id = match find_session_by_receiver(&state, data.receiver_session_id) {
        Some(id) => id,
        None => return,
    };

    let (was_established, is_established, has_new_stream, packets, peer_addr) = {
        let entry = match state.connections.get_mut(&session_id) {
            Some(e) => e,
            None => return,
        };

        let was_established = entry.conn.is_established();
        entry.conn.on_data_packet(data, now);
        let is_established = entry.conn.is_established();
        let has_new_stream = entry.conn.has_pending_accept();
        let packets = entry.conn.poll_packets(now);
        let peer_addr = entry.peer_addr;

        (
            was_established,
            is_established,
            has_new_stream,
            packets,
            peer_addr,
        )
    };

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

    let mut to_send: Vec<(SocketAddr, Vec<Data>)> = Vec::new();
    for entry in state.connections.values_mut() {
        let packets = entry.conn.poll_packets(now);
        if !packets.is_empty() {
            to_send.push((entry.peer_addr, packets));
        }
    }
    drop(state);

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

fn stream_err_to_io(e: crate::v2::stream::StreamError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
