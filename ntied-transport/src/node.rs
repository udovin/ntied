use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};
use tokio::task::JoinHandle;

use tracing::{debug, info, warn};

use crate::channel::ChannelError;
use crate::connection::Connection as InnerConnection;
use crate::crypto::PeerId;
use crate::crypto::{
    EncryptionKeys, KemPrivateKey, PrivateKey, PublicKey, compute_transcript_hash,
};
use crate::relay::protocol::{RelayMessage, PURPOSE_RELAY};
use crate::session::{Role, Session};
use crate::wire::packet::{Data, Packet};
use crate::wire::{Frame, KeyExchangeInit, KeyExchangeResponse};

const RECV_BUF_SIZE: usize = 4096;
const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

// ── SendPath ──

/// How to reach a peer: directly via UDP or through a relay.
#[derive(Debug, Clone)]
pub(crate) enum SendPath {
    Direct { addr: SocketAddr },
    Relayed { peer_id: PeerId },
}

// ── RelayState ──

/// State for an attached relay connection.
pub(crate) struct RelayState {
    _connection_id: u64,
    datagram: DatagramChannel,
}

// ── Node (Endpoint) ──

pub struct Node {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
}

pub(crate) struct Shared {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) identity: PrivateKey,
    pub(crate) state: TokioMutex<TransportState>,
    pub(crate) relay: TokioMutex<Option<RelayState>>,
    pub(crate) pending_close: std::sync::Mutex<Vec<u64>>,
    pub(crate) ping_counter: AtomicU32,
    pub(crate) accept_notify: Notify,
    pub(crate) established_notify: Notify,
    pub(crate) data_notify: Notify,
    pub(crate) stream_notify: Notify,
}

pub(crate) struct TransportState {
    pub(crate) connections: HashMap<u64, ConnEntry>,
    pub(crate) pending_connects: HashMap<u64, PendingConnect>,
    pub(crate) accept_queue: VecDeque<u64>,
    pub(crate) next_connection_id: u64,
}

// ── ConnEntry ──

pub(crate) struct ConnEntry {
    pub(crate) send_path: SendPath,
    pub(crate) conn: Box<InnerConnection>,
    pub(crate) last_recv: Instant,
    pub(crate) last_ping_sent: Instant,
    pub(crate) closed: bool,
    pub(crate) is_local_initiator: bool,
}

impl ConnEntry {
    /// Returns the direct address if this is a direct connection, None if relayed.
    pub(crate) fn addr(&self) -> Option<SocketAddr> {
        match &self.send_path {
            SendPath::Direct { addr } => Some(*addr),
            SendPath::Relayed { .. } => None,
        }
    }

    pub(crate) fn is_established(&self) -> bool {
        self.conn.is_established()
    }

    pub(crate) fn got_connection_close(&self) -> bool {
        self.conn.got_connection_close()
    }

    pub(crate) fn queue_connection_close(&mut self, error_code: u32) {
        self.conn.queue_connection_close(error_code);
    }

    pub(crate) fn queue_ping(&mut self, ping_id: u32) {
        self.conn.queue_ping(ping_id);
    }

    pub(crate) fn queue_frame(&mut self, frame: Frame) {
        self.conn.queue_frame(frame);
    }

    pub(crate) fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        self.conn.poll_packets(now)
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.conn.has_pending()
    }

    pub(crate) fn peer_public_key(&self) -> Option<&PublicKey> {
        self.conn.peer_public_key()
    }

    pub(crate) fn local_connection_id(&self) -> u64 {
        self.conn.local_connection_id()
    }

    pub(crate) fn remote_connection_id(&self) -> u64 {
        self.conn.remote_connection_id()
    }

    pub(crate) fn has_pending_accept(&self) -> bool {
        self.conn.has_pending_accept()
    }
}

// ── PendingConnect ──

pub(crate) struct PendingConnect {
    pub(crate) ephemeral_key: Box<KemPrivateKey>,
    pub(crate) send_path: SendPath,
}

// ── Node impl ──

impl Node {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
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
                    Instant::now().hash(&mut h);
                    (h.finish() >> 32) | 1 // random-ish, never 0
                },
            }),
            relay: TokioMutex::new(None),
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

    pub fn peer_id(&self) -> PeerId {
        self.shared.identity.public_key().peer_id()
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<Connection> {
        let connection_id = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_connection_id;
            state.next_connection_id += 1;

            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());

            let init_bytes = KeyExchangeInit {
                initiator_connection_id: sid,
                kem_public_key: *eph_pk,
            }
            .encode();
            self.shared.socket.send_to(&init_bytes, addr).await?;

            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    send_path: SendPath::Direct { addr },
                },
            );

            sid
        };

        wait_for_established(&self.shared, connection_id).await
    }

    /// Attach to a relay server. Connects to the relay, opens a PURPOSE_RELAY
    /// datagram channel, receives the Welcome message, and spawns a background
    /// task that listens for tunneled packets from other peers.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        let conn = self.connect(relay_addr).await?;
        let datagram = conn.open_datagram(PURPOSE_RELAY).await?;

        // Receive and consume the Welcome message
        let welcome_data = datagram.recv().await?;
        match RelayMessage::decode(&welcome_data) {
            Some(RelayMessage::Welcome { external_addr }) => {
                info!(%external_addr, "relay: attached, external addr from welcome");
            }
            _ => {
                warn!("relay: expected Welcome message, got something else");
            }
        }

        // Store relay state
        {
            let mut relay = self.shared.relay.lock().await;
            *relay = Some(RelayState {
                _connection_id: conn.connection_id(),
                datagram: datagram.clone(),
            });
        }

        // Spawn relay listener task
        let shared = self.shared.clone();
        let dg = datagram;
        tokio::spawn(async move {
            relay_listener_loop(shared, dg).await;
        });

        // Keep the Connection alive by leaking it (the relay listener task owns it implicitly
        // via the datagram channel's shared reference). We forget the Connection handle so
        // Drop doesn't fire and close it.
        std::mem::forget(conn);

        Ok(())
    }

    /// Connect to a remote peer through the attached relay. The peer must also
    /// be attached to the same relay.
    pub async fn connect_peer(&self, peer_id: &PeerId) -> io::Result<Connection> {
        let (sid, init_bytes) = {
            let mut state = self.shared.state.lock().await;
            let sid = state.next_connection_id;
            state.next_connection_id += 1;
            let eph = Box::new(KemPrivateKey::generate());
            let eph_pk = Box::new(eph.public_key());
            let init_bytes = KeyExchangeInit {
                initiator_connection_id: sid,
                kem_public_key: *eph_pk,
            }
            .encode();
            state.pending_connects.insert(
                sid,
                PendingConnect {
                    ephemeral_key: eph,
                    send_path: SendPath::Relayed {
                        peer_id: *peer_id,
                    },
                },
            );
            (sid, init_bytes)
        };
        // Lock released — safe to call send_via_relay
        send_via_relay(&self.shared, peer_id, &init_bytes).await?;

        wait_for_established(&self.shared, sid).await
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

// ── Connection ──

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
            .and_then(|e| e.peer_public_key().cloned())
    }

    pub async fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().await.map(|pk| pk.peer_id())
    }

    pub async fn remote_addr(&self) -> Option<SocketAddr> {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.connection_id)
            .and_then(|e| e.addr())
    }

    pub async fn is_established(&self) -> bool {
        let state = self.shared.state.lock().await;
        state
            .connections
            .get(&self.connection_id)
            .map_or(false, |e| e.is_established() && !e.closed)
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut state = self.shared.state.lock().await;
        if let Some(entry) = state.connections.get_mut(&self.connection_id) {
            if !entry.closed {
                entry.closed = true;
                entry.queue_connection_close(0);
                let packets = entry.poll_packets(Instant::now());
                let send_path = entry.send_path.clone();
                drop(state);
                send_packets(&self.shared, &send_path, &packets).await;
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

// ── StreamChannel ──

pub struct StreamChannel {
    shared: Arc<Shared>,
    connection_id: u64,
    channel_id: u32,
}

#[derive(Clone)]
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

// ── recv_loop ──

async fn recv_loop(shared: Arc<Shared>) {
    let mut buf = vec![0u8; RECV_BUF_SIZE].into_boxed_slice();
    let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            result = shared.socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, addr)) => {
                        let path = SendPath::Direct { addr };
                        process_packet(&shared, &buf[..len], path).await;
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

// ── Relay listener ──

/// Background task that reads from the relay datagram channel and processes
/// tunneled packets as if they arrived from the network.
async fn relay_listener_loop(shared: Arc<Shared>, datagram: DatagramChannel) {
    loop {
        let data = match datagram.recv().await {
            Ok(d) => d,
            Err(e) => {
                debug!("relay listener: recv error: {e}");
                break;
            }
        };

        let msg = match RelayMessage::decode(&data) {
            Some(m) => m,
            None => continue,
        };

        match msg {
            RelayMessage::Tunnel {
                peer_id,
                data: inner,
            } => {
                let path = SendPath::Relayed { peer_id };
                process_packet(&shared, &inner, path).await;
            }
            _ => {
                debug!("relay listener: ignoring non-tunnel message");
            }
        }
    }

    // Relay disconnected — clear relay state
    let mut relay = shared.relay.lock().await;
    *relay = None;
    warn!("relay listener: disconnected");
}

// ── Packet processing ──

async fn process_packet(shared: &Shared, buf: &[u8], send_path: SendPath) {
    let packet = match Packet::decode(buf) {
        Ok(p) => Box::new(p),
        Err(_) => return,
    };

    match *packet {
        Packet::KeyExchangeInit(init) => {
            handle_key_exchange_init(shared, Box::new(init), send_path).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            handle_key_exchange_response(shared, Box::new(resp)).await;
        }
        Packet::Data(data) => {
            handle_data(shared, data).await;
        }
        Packet::HolePunch(_) => {
            // Received a hole punch; nothing to do for direct-only transport.
        }
    }
}

// ── Key exchange ──

async fn handle_key_exchange_init(
    shared: &Shared,
    init: Box<KeyExchangeInit>,
    send_path: SendPath,
) {
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

    let response_bytes = response.encode();

    // Send the response back via the same path the init arrived on
    match &send_path {
        SendPath::Direct { addr } => {
            let _ = shared.socket.send_to(&response_bytes, *addr).await;
        }
        SendPath::Relayed { peer_id } => {
            drop(state); // release lock before async relay send
            let _ = send_via_relay(shared, peer_id, &response_bytes).await;
            // re-acquire
            let mut state2 = shared.state.lock().await;
            // Re-check next_connection_id wasn't reused (it was already incremented, so safe)
            let session = Session::new(Role::Responder, 1, keys, th);
            let auth_payload = build_auth_payload(&shared.identity, &th);

            let conn = Box::new(InnerConnection::new(
                session,
                local_sid,
                init.initiator_connection_id,
                false,
                auth_payload,
            ));

            let entry = ConnEntry {
                send_path: send_path.clone(),
                conn,
                last_recv: Instant::now(),
                last_ping_sent: Instant::now(),
                closed: false,
                is_local_initiator: false,
            };
            state2.connections.insert(local_sid, entry);

            let packets = state2
                .connections
                .get_mut(&local_sid)
                .unwrap()
                .poll_packets(Instant::now());
            drop(state2);

            send_packets(shared, &send_path, &packets).await;
            return;
        }
    }

    // Direct path: state lock is still held
    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = Box::new(InnerConnection::new(
        session,
        local_sid,
        init.initiator_connection_id,
        false,
        auth_payload,
    ));

    let entry = ConnEntry {
        send_path: send_path.clone(),
        conn,
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
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &send_path, &packets).await;
}

pub(crate) async fn handle_key_exchange_response(
    shared: &Shared,
    resp: Box<KeyExchangeResponse>,
) {
    let mut state = shared.state.lock().await;

    let pending = match state.pending_connects.remove(&resp.initiator_connection_id) {
        Some(p) => p,
        None => return,
    };

    let send_path = pending.send_path;

    let init_pk = Box::new(pending.ephemeral_key.public_key());
    let init_ss = match pending.ephemeral_key.decapsulate(&resp.kem_ciphertext) {
        Some(ss) => ss,
        None => return,
    };

    let keys = EncryptionKeys::new(&init_ss, &init_pk, &resp.kem_ciphertext);
    let th = compute_transcript_hash(&init_pk, &resp.kem_ciphertext);
    let session = Session::new(Role::Initiator, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = Box::new(InnerConnection::new(
        session,
        resp.initiator_connection_id,
        resp.responder_connection_id,
        true,
        auth_payload,
    ));

    let entry = ConnEntry {
        send_path: send_path.clone(),
        conn,
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: true,
    };
    state
        .connections
        .insert(resp.initiator_connection_id, entry);

    let packets = state
        .connections
        .get_mut(&resp.initiator_connection_id)
        .unwrap()
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &send_path, &packets).await;
}

// ── Data handling ──

async fn handle_data(shared: &Shared, data: Data) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let receiver_sid = data.receiver_connection_id;
    let connection_id = match find_session_by_receiver(&state, receiver_sid) {
        Some(id) => id,
        None => {
            debug!(me = %short_pid(shared), receiver_sid, "handle_data: unknown session");
            return;
        }
    };

    let (was_established, is_established, has_new_stream, got_close, packets, entry_send_path, is_local_initiator) = {
        let entry = match state.connections.get_mut(&connection_id) {
            Some(e) => e,
            None => return,
        };

        let was_established = entry.is_established();
        let had_close = entry.got_connection_close();
        // Process the data packet; drop any unhandled frames (no relay/gateway).
        entry.conn.on_data_packet(data, now);
        entry.last_recv = now;
        let is_established = entry.is_established();
        let has_new_stream = entry.has_pending_accept();
        let got_close = !had_close && entry.got_connection_close();
        let is_local_initiator = entry.is_local_initiator;
        if got_close {
            entry.closed = true;
        }
        let packets = entry.poll_packets(now);
        let entry_send_path = entry.send_path.clone();

        (
            was_established,
            is_established,
            has_new_stream,
            got_close,
            packets,
            entry_send_path,
            is_local_initiator,
        )
    };

    if got_close {
        state.connections.remove(&connection_id);
    }

    if !was_established && is_established && !is_local_initiator {
        debug!(connection_id, "accept_queue: push (handle_data)");
        state.accept_queue.push_back(connection_id);
    }
    drop(state);

    send_packets(shared, &entry_send_path, &packets).await;

    if !was_established && is_established {
        info!(me = %short_pid(shared), connection_id, is_local_initiator, "ESTABLISHED");
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

// ── Flush ──

async fn flush_all(shared: &Shared) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    // Process pending close requests from dropped Connection handles.
    let closes: Vec<u64> = shared.pending_close.lock().unwrap().drain(..).collect();
    for connection_id in closes {
        if let Some(entry) = state.connections.get_mut(&connection_id) {
            if !entry.closed {
                entry.closed = true;
                entry.queue_connection_close(0);
            }
        }
    }

    // Detect timed-out connections.
    let mut timed_out: Vec<u64> = Vec::new();
    for (&sid, entry) in state.connections.iter_mut() {
        if entry.closed {
            continue;
        }
        if entry.is_established() && now.duration_since(entry.last_recv) > CONNECTION_TIMEOUT {
            timed_out.push(sid);
            continue;
        }
        if entry.is_established() && now.duration_since(entry.last_ping_sent) > PING_INTERVAL {
            let ping_id = shared.ping_counter.fetch_add(1, Ordering::Relaxed);
            entry.queue_ping(ping_id);
            entry.last_ping_sent = now;
        }
    }
    for sid in &timed_out {
        state.connections.remove(sid);
    }

    // Poll all connections for outbound packets.
    let mut to_send: Vec<(SendPath, Vec<Data>)> = Vec::new();
    let mut to_remove: Vec<u64> = Vec::new();

    for (&sid, entry) in state.connections.iter_mut() {
        let packets = entry.poll_packets(now);
        if !packets.is_empty() {
            to_send.push((entry.send_path.clone(), packets));
        }
        if entry.closed && !entry.has_pending() {
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

    for (path, packets) in &to_send {
        send_packets(shared, path, packets).await;
    }
}

// ── Helpers ──

pub(crate) fn short_pid(shared: &Shared) -> String {
    let full = format!("{:?}", shared.identity.public_key().peer_id());
    full.chars()
        .skip(full.len().saturating_sub(6))
        .take(4)
        .collect()
}

pub(crate) async fn flush_connection(shared: &Shared, connection_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (send_path, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let packets = entry.poll_packets(now);
        (entry.send_path.clone(), packets)
    };

    send_packets(shared, &send_path, &packets).await;
    Ok(())
}

pub(crate) fn find_session_by_receiver(
    state: &TransportState,
    receiver_connection_id: u64,
) -> Option<u64> {
    state
        .connections
        .iter()
        .find(|(_, e)| e.local_connection_id() == receiver_connection_id)
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

/// Send encoded data packets via the appropriate path (direct UDP or relay tunnel).
pub(crate) async fn send_packets(shared: &Shared, send_path: &SendPath, packets: &[Data]) {
    match send_path {
        SendPath::Direct { addr } => {
            for data in packets {
                let _ = shared.socket.send_to(&data.encode(), *addr).await;
            }
        }
        SendPath::Relayed { peer_id } => {
            for data in packets {
                let encoded = data.encode();
                if let Err(e) = send_via_relay(shared, peer_id, &encoded).await {
                    debug!(%peer_id, "send_packets relay: {e}");
                }
            }
        }
    }
}

/// Send raw bytes to a peer through the attached relay's tunnel.
///
/// This function writes to the relay datagram channel and flushes the relay
/// connection using direct UDP sends only (never recursing through the
/// generic `send_packets` path).
async fn send_via_relay(shared: &Shared, peer_id: &PeerId, data: &[u8]) -> io::Result<()> {
    let relay = shared.relay.lock().await;
    let relay_state = relay.as_ref().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "no relay attached")
    })?;

    let msg = RelayMessage::Tunnel {
        peer_id: *peer_id,
        data: data.to_vec(),
    };
    let encoded_msg = msg.encode();
    let relay_conn_id = relay_state.datagram.connection_id;
    let relay_ch_id = relay_state.datagram.channel_id;
    drop(relay);

    // Write to the relay connection's datagram channel
    {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&relay_conn_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "relay connection gone"))?;
        entry
            .conn
            .write_datagram(relay_ch_id, &encoded_msg)
            .map_err(channel_err_to_io)?;
    }

    // Flush the relay connection directly via UDP (relay is always Direct)
    flush_connection_direct(shared, relay_conn_id).await
}

/// Flush a connection using direct UDP only. Used for the relay connection
/// itself to avoid recursion in send_packets.
async fn flush_connection_direct(shared: &Shared, connection_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (addr, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let addr = match &entry.send_path {
            SendPath::Direct { addr } => *addr,
            SendPath::Relayed { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "flush_connection_direct called on relayed connection",
                ));
            }
        };
        let packets = entry.poll_packets(now);
        (addr, packets)
    };

    for data in &packets {
        let _ = shared.socket.send_to(&data.encode(), addr).await;
    }
    Ok(())
}

/// Wait for a connection to become established, with timeout.
async fn wait_for_established(shared: &Arc<Shared>, connection_id: u64) -> io::Result<Connection> {
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        tokio::select! {
            _ = shared.established_notify.notified() => {
                let state = shared.state.lock().await;
                if let Some(entry) = state.connections.get(&connection_id) {
                    if entry.is_established() {
                        return Ok(Connection {
                            shared: shared.clone(),
                            connection_id,
                            closed: AtomicBool::new(false),
                        });
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let mut state = shared.state.lock().await;
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

fn channel_err_to_io(e: ChannelError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
