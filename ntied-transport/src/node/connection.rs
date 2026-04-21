use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::connection::{Connection as Inner, ConnectionId, RecvInfo};
use crate::crypto::{KemPublicKey, PeerId, PrivateKey, PublicKey};

use super::channel::Channel;
use super::path::{
    Path, PathState, Paths, check_state_timers, record_recv_and_promote, send_via_paths,
};
use super::relay::RelayConnection;
use super::stream::Stream;
use super::transport::Transport;

/// Raw packet routed from Node recv_loop to Connection main_loop.
pub(crate) struct RawPacket {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

pub(crate) type ConnectionMap = Arc<RwLock<HashMap<u64, mpsc::Sender<RawPacket>>>>;

/// Per-stream and per-channel Notify maps.
pub(crate) type NotifyMap = Arc<Mutex<HashMap<u64, Arc<Notify>>>>;

pub(crate) struct OwnedConnectionId {
    id: u64,
    cleanup: ConnectionCleanup,
}

enum ConnectionCleanup {
    /// Direct connection: registered in Node's connection_map for UDP dispatch.
    Direct(ConnectionMap),
    /// Tunneled connection: registered in Node's connection_map (the same
    /// table the relay-pump uses for dispatch by connection_id).
    /// `_relay` is kept to anchor the relay-conn's lifetime to the tunneled
    /// connection (so the relay isn't dropped while we still need it).
    Tunneled {
        _relay: Arc<RelayConnection>,
        connection_map: ConnectionMap,
    },
}

impl OwnedConnectionId {
    pub(crate) fn new(
        id: u64,
        connection_map: &ConnectionMap,
        tx: mpsc::Sender<RawPacket>,
    ) -> Self {
        connection_map.write().unwrap().insert(id, tx);
        Self {
            id,
            cleanup: ConnectionCleanup::Direct(connection_map.clone()),
        }
    }

    pub(crate) fn tunneled(
        id: u64,
        relay: Arc<RelayConnection>,
        connection_map: ConnectionMap,
        tx: mpsc::Sender<RawPacket>,
    ) -> Self {
        connection_map.write().unwrap().insert(id, tx);
        Self {
            id,
            cleanup: ConnectionCleanup::Tunneled {
                _relay: relay,
                connection_map,
            },
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for OwnedConnectionId {
    fn drop(&mut self) {
        match &self.cleanup {
            ConnectionCleanup::Direct(map) => {
                map.write().unwrap().remove(&self.id);
            }
            ConnectionCleanup::Tunneled {
                connection_map, ..
            } => {
                connection_map.write().unwrap().remove(&self.id);
            }
        }
    }
}

pub struct Connection {
    pub(crate) connection_id: OwnedConnectionId,
    pub(crate) inner: Arc<Mutex<Inner>>,
    next_stream_id: AtomicU64,
    next_channel_id: AtomicU64,
    pub(crate) stream_notifies: NotifyMap,
    pub(crate) channel_notifies: NotifyMap,
    /// Wakes main_loop when channels write data (including FIN-on-drop).
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) accept_stream_rx: TokioMutex<mpsc::Receiver<Stream>>,
    pub(crate) accept_channel_rx: TokioMutex<mpsc::Receiver<Channel>>,
    pub(crate) paths: Paths,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) main_task: Mutex<Option<JoinHandle<()>>>,
}

impl Connection {
    pub(crate) async fn accept(
        local_id: u64,
        peer_id: u64,
        peer_kem_pk: KemPublicKey,
        connection_id: OwnedConnectionId,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        rx: mpsc::Receiver<RawPacket>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) {
        let transport = Transport::udp(socket.clone(), addr);
        let initial_path = Path::new(transport, addr, PathState::Active);
        let paths: Paths = Arc::new(RwLock::new(vec![initial_path]));
        Self::accept_inner(
            local_id,
            peer_id,
            peer_kem_pk,
            connection_id,
            paths,
            socket,
            identity,
            rx,
            accept_tx,
            cancel_token,
        )
        .await;
    }

    pub(crate) async fn accept_tunneled(
        local_id: u64,
        peer_id: u64,
        peer_kem_pk: KemPublicKey,
        connection_id: OwnedConnectionId,
        transport: Arc<Transport>,
        relay_addr: SocketAddr,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        rx: mpsc::Receiver<RawPacket>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
    ) {
        let initial_path = Path::new(transport, relay_addr, PathState::Active);
        let paths: Paths = Arc::new(RwLock::new(vec![initial_path]));
        Self::accept_inner(
            local_id,
            peer_id,
            peer_kem_pk,
            connection_id,
            paths,
            socket,
            identity,
            rx,
            accept_tx,
            cancel_token,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn accept_inner(
        local_id: u64,
        peer_id: u64,
        peer_kem_pk: KemPublicKey,
        connection_id: OwnedConnectionId,
        paths: Paths,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        rx: mpsc::Receiver<RawPacket>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
    ) {
        let mut conn = Inner::accept(
            ConnectionId(local_id),
            ConnectionId(peer_id),
            peer_kem_pk,
            identity,
        );

        let mut buf = [0u8; 1280];
        match conn.send(&mut buf, Instant::now()) {
            Ok((n, _)) => {
                send_via_paths(&paths, &buf[..n]).await;
            }
            Err(e) => {
                warn!(?e, "failed to generate InitAck");
                return;
            }
        }

        let inner = Arc::new(Mutex::new(conn));
        let stream_notifies: NotifyMap = Default::default();
        let channel_notifies: NotifyMap = Default::default();
        let conn_notify = Arc::new(Notify::new());
        let send_notify = Arc::new(Notify::new());
        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(16);
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(16);
        let (established_tx, established_rx) = oneshot::channel();

        let task = tokio::spawn(Self::main_loop(
            local_id,
            inner.clone(),
            rx,
            paths.clone(),
            socket.clone(),
            cancel_token.clone(),
            Some(established_tx),
            stream_notifies.clone(),
            channel_notifies.clone(),
            conn_notify.clone(),
            send_notify.clone(),
            accept_stream_tx,
            accept_channel_tx,
        ));

        match established_rx.await {
            Ok(()) => {}
            Err(_) => {
                cancel_token.cancel();
                warn!("connection closed during auth");
                return;
            }
        }

        let connection = Connection {
            connection_id,
            inner,
            next_stream_id: AtomicU64::new(1), // responder: odd
            next_channel_id: AtomicU64::new(1),
            stream_notifies,
            channel_notifies,
            send_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_channel_rx: TokioMutex::new(accept_channel_rx),
            paths,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        };
        if accept_tx.send(connection).await.is_err() {
            warn!("failed to send connection to accept queue");
        }
    }

    pub(crate) async fn connect(
        connection_id: OwnedConnectionId,
        rx: mpsc::Receiver<RawPacket>,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) -> io::Result<Connection> {
        let transport = Transport::udp(socket.clone(), addr);
        let initial_path = Path::new(transport, addr, PathState::Active);
        let paths: Paths = Arc::new(RwLock::new(vec![initial_path]));
        Self::finalize_connect(connection_id, paths, socket, rx, identity, cancel_token).await
    }

    pub(crate) async fn connect_tunneled(
        connection_id: OwnedConnectionId,
        rx: mpsc::Receiver<RawPacket>,
        transport: Arc<Transport>,
        relay_addr: SocketAddr,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        cancel_token: CancellationToken,
    ) -> io::Result<Connection> {
        let initial_path = Path::new(transport, relay_addr, PathState::Active);
        let paths: Paths = Arc::new(RwLock::new(vec![initial_path]));
        Self::finalize_connect(connection_id, paths, socket, rx, identity, cancel_token).await
    }

    async fn finalize_connect(
        connection_id: OwnedConnectionId,
        paths: Paths,
        socket: Arc<UdpSocket>,
        mut rx: mpsc::Receiver<RawPacket>,
        identity: PrivateKey,
        cancel_token: CancellationToken,
    ) -> io::Result<Connection> {
        let mut conn = Inner::open(ConnectionId(connection_id.id()), identity);

        let mut buf = [0u8; 1280];
        let (n, _) = conn
            .send(&mut buf, Instant::now())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        send_via_paths(&paths, &buf[..n]).await;

        // Wait for InitAck with handshake timeout.
        let handshake_timeout = conn.timeout().unwrap_or(std::time::Duration::from_secs(10));
        let init_ack = tokio::select! {
            packet = rx.recv() => {
                packet.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::ConnectionReset, "Channel closed")
                })?
            }
            _ = tokio::time::sleep(handshake_timeout) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Handshake timed out"));
            }
            _ = cancel_token.cancelled() => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "Cancelled"));
            }
        };

        {
            let mut data = init_ack.data;
            conn.recv(&mut data,
                RecvInfo {
                    now: Instant::now(),
                },
            )
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        }

        let inner = Arc::new(Mutex::new(conn));
        let stream_notifies: NotifyMap = Default::default();
        let channel_notifies: NotifyMap = Default::default();
        let conn_notify = Arc::new(Notify::new());
        let send_notify = Arc::new(Notify::new());
        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(16);
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(16);
        let (established_tx, established_rx) = oneshot::channel();

        let cid = connection_id.id();
        let task = tokio::spawn(Self::main_loop(
            cid,
            inner.clone(),
            rx,
            paths.clone(),
            socket.clone(),
            cancel_token.clone(),
            Some(established_tx),
            stream_notifies.clone(),
            channel_notifies.clone(),
            conn_notify.clone(),
            send_notify.clone(),
            accept_stream_tx,
            accept_channel_tx,
        ));

        match established_rx.await {
            Ok(()) => {}
            Err(_) => {
                cancel_token.cancel();
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "Connection closed during auth",
                ));
            }
        }

        Ok(Connection {
            connection_id,
            inner,
            next_stream_id: AtomicU64::new(0), // initiator: even
            next_channel_id: AtomicU64::new(0),
            stream_notifies,
            channel_notifies,
            send_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_channel_rx: TokioMutex::new(accept_channel_rx),
            paths,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        })
    }

    pub fn connection_id(&self) -> u64 {
        self.connection_id.id()
    }

    pub fn peer_public_key(&self) -> Option<PublicKey> {
        self.inner.lock().unwrap().peer_public_key().cloned()
    }

    pub fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().map(|pk| pk.peer_id())
    }

    pub fn remote_addr(&self) -> Option<SocketAddr> {
        let paths = self.paths.read().unwrap();
        paths
            .iter()
            .find(|p| p.state() == PathState::Active)
            .or_else(|| paths.first())
            .map(|p| p.addr_key)
    }

    /// True iff outbound traffic is currently using a direct UDP path
    /// (i.e. an `Active` Udp transport exists).
    pub fn is_using_direct_path(&self) -> bool {
        self.paths
            .read()
            .unwrap()
            .iter()
            .any(|p| p.state() == PathState::Active && matches!(&*p.transport, Transport::Udp { .. }))
    }

    pub fn open_stream(&self) -> io::Result<Stream> {
        let stream_id = self.next_stream_id.fetch_add(2, Ordering::Relaxed);
        {
            let mut conn = self.inner.lock().unwrap();
            conn.stream_write(stream_id, &[], false)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        self.send_notify.notify_one();
        let notify = Arc::new(Notify::new());
        self.stream_notifies
            .lock()
            .unwrap()
            .insert(stream_id, notify.clone());
        Ok(Stream {
            stream_id,
            inner: self.inner.clone(),
            notify,
            send_notify: self.send_notify.clone(),
            stream_notifies: self.stream_notifies.clone(),
            cancel_token: self.cancel_token.child_token(),
        })
    }

    pub fn open_channel(&self) -> io::Result<Channel> {
        let channel_id = self.next_channel_id.fetch_add(2, Ordering::Relaxed);
        {
            let mut conn = self.inner.lock().unwrap();
            conn.open_channel(channel_id)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        self.send_notify.notify_one();
        let notify = Arc::new(Notify::new());
        self.channel_notifies
            .lock()
            .unwrap()
            .insert(channel_id, notify.clone());
        Ok(Channel {
            channel_id,
            inner: self.inner.clone(),
            notify,
            send_notify: self.send_notify.clone(),
            channel_notifies: self.channel_notifies.clone(),
            cancel_token: self.cancel_token.child_token(),
        })
    }

    pub async fn accept_stream(&self) -> io::Result<Stream> {
        self.accept_stream_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Connection closed"))
    }

    pub async fn accept_channel(&self) -> io::Result<Channel> {
        self.accept_channel_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Connection closed"))
    }

    /// Ask the relay (if this connection is tunneled) to perform a hole-punch
    /// signal exchange with the peer: relay sends each side the other's
    /// external address. Once the addresses propagate the per-connection
    /// state machine begins probing a direct path; on success it switches
    /// outbound traffic from the tunnel to direct UDP.
    ///
    /// No-op (returns `Ok`) if the connection has no tunnel path.
    pub async fn try_direct(&self) -> io::Result<()> {
        let target = self
            .peer_id()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "peer_id not yet known"))?;
        let relay = {
            let paths = self.paths.read().unwrap();
            paths.iter().find_map(|p| match &*p.transport {
                Transport::Tunnel { relay, .. } => Some(relay.clone()),
                _ => None,
            })
        };
        let Some(relay) = relay else {
            return Ok(());
        };
        relay.send_holepunch_request(target).await
    }

    pub async fn close(&self) {
        let main_task = self.main_task.lock().unwrap().take();
        if let Some(task) = main_task {
            self.cancel_token.cancel();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn main_loop(
        conn_id: u64,
        inner: Arc<Mutex<Inner>>,
        mut rx: mpsc::Receiver<RawPacket>,
        paths: Paths,
        socket: Arc<UdpSocket>,
        cancel_token: CancellationToken,
        established_tx: Option<oneshot::Sender<()>>,
        stream_notifies: NotifyMap,
        channel_notifies: NotifyMap,
        conn_notify: Arc<Notify>,
        send_notify: Arc<Notify>,
        accept_stream_tx: mpsc::Sender<Stream>,
        accept_channel_tx: mpsc::Sender<Channel>,
    ) {
        let mut established_tx = established_tx;
        let mut send_buf = [0u8; 1280];
        let mut ctx = AcceptCtx {
            inner: &inner,
            stream_notifies: &stream_notifies,
            channel_notifies: &channel_notifies,
            accept_stream_tx: &accept_stream_tx,
            accept_channel_tx: &accept_channel_tx,
            send_notify: &send_notify,
            cancel_token: &cancel_token,
            pending_accept_streams: Vec::new(),
            pending_accept_channels: Vec::new(),
            scratch_streams: Vec::new(),
            scratch_channels: Vec::new(),
            scratch_writable: Vec::new(),
        };

        // Initial drain: send any pending frames (e.g. auth data after handshake).
        let sent = Self::drain_send(&inner, &mut send_buf, &paths).await;
        trace!(conn_id, packets_sent = sent, "drain_send initial");

        // Check if connection established during initial drain (auth may have completed).
        {
            let conn = inner.lock().unwrap();
            if conn.is_established() {
                if let Some(tx) = established_tx.take() {
                    trace!(conn_id, "connection established after initial drain");
                    let _ = tx.send(());
                    drop(conn);
                    Self::auto_request_direct(&inner, &paths);
                }
            }
        }

        loop {
            let timeout_dur = {
                let conn = inner.lock().unwrap();
                conn.timeout().unwrap_or(std::time::Duration::from_secs(60))
            };
            let sleep = tokio::time::sleep(timeout_dur);

            tokio::select! {
                packet = rx.recv() => {
                    let Some(mut packet) = packet else {
                        trace!(conn_id, "rx channel closed, exiting main_loop");
                        break;
                    };
                    let now = Instant::now();
                    let pkt_len = packet.data.len();
                    let pkt_addr = packet.addr;
                    let (recv_ok, is_closed, is_established) = {
                        let mut conn = inner.lock().unwrap();
                        let r = conn.recv(&mut packet.data, RecvInfo { now });
                        if let Err(e) = &r {
                            trace!(?e, pkt_len, "recv error, dropping packet");
                        }
                        (r.is_ok(), conn.is_closed(), conn.is_established())
                    };

                    trace!(conn_id, pkt_len, is_closed, is_established, "processed rx packet");

                    if recv_ok {
                        record_recv_and_promote(&paths, pkt_addr, now);
                    }

                    if is_closed {
                        trace!(conn_id, "connection closed by peer");
                        notify_and_accept(&mut ctx);
                        notify_all(&stream_notifies);
                        notify_all(&channel_notifies);
                        conn_notify.notify_waiters();
                        return;
                    }
                    if is_established {
                        if let Some(tx) = established_tx.take() {
                            trace!(conn_id, "connection established");
                            let _ = tx.send(());
                            Self::auto_request_direct(&inner, &paths);
                        }
                    }

                    Self::poll_pending_holepunch(&paths, &socket);

                    let sent = Self::drain_send(&inner, &mut send_buf, &paths).await;
                    trace!(conn_id, packets_sent = sent, "drain_send after rx");
                    notify_and_accept(&mut ctx);
                }
                _ = sleep => {
                    trace!(conn_id, timeout_ms = timeout_dur.as_millis(), "timeout fired");
                    let now = Instant::now();
                    {
                        let mut conn = inner.lock().unwrap();
                        conn.on_timeout(now);
                        if conn.is_closed() {
                            trace!(conn_id, "connection closed by timeout");
                            drop(conn);
                            notify_all(&stream_notifies);
                            notify_all(&channel_notifies);
                            conn_notify.notify_waiters();
                            return;
                        }
                    }
                    check_state_timers(&paths, now);
                    Self::poll_pending_holepunch(&paths, &socket);
                    let sent = Self::drain_send(&inner, &mut send_buf, &paths).await;
                    trace!(conn_id, packets_sent = sent, "drain_send after timeout");
                }
                _ = send_notify.notified() => {
                    Self::poll_pending_holepunch(&paths, &socket);
                    let sent = Self::drain_send(&inner, &mut send_buf, &paths).await;
                    trace!(conn_id, packets_sent = sent, "drain_send after send_notify");
                }
                _ = cancel_token.cancelled() => {
                    trace!(conn_id, "cancel token fired");
                    break;
                }
            }
        }

        {
            let mut conn = inner.lock().unwrap();
            let _ = conn.close(0, b"shutdown");
        }
        // Drain all remaining data and then the ConnectionClose.
        loop {
            let sent = Self::drain_send(&inner, &mut send_buf, &paths).await;
            trace!(
                conn_id,
                packets_sent = sent,
                "drain_send after shutdown close"
            );
            if sent == 0 {
                break;
            }
        }
        notify_all(&stream_notifies);
        notify_all(&channel_notifies);
        conn_notify.notify_waiters();
    }

    const MAX_SEND_BURST: u32 = 32;

    async fn drain_send(
        inner: &Mutex<Inner>,
        buf: &mut [u8],
        paths: &Paths,
    ) -> u32 {
        let mut count = 0u32;
        while count < Self::MAX_SEND_BURST {
            let result = {
                let mut conn = inner.lock().unwrap();
                conn.send(buf, Instant::now())
            };
            match result {
                Ok((n, _)) => {
                    count += 1;
                    send_via_paths(paths, &buf[..n]).await;
                }
                Err(_) => break,
            }
        }
        count
    }

    /// Fired once when the connection becomes established. If we're tunneled,
    /// kick off the hole-punch handshake automatically — without waiting for
    /// the user to call `try_direct()`. Subsequent path probing and promotion
    /// are driven by the per-iteration helpers in `path.rs`.
    fn auto_request_direct(inner: &Mutex<Inner>, paths: &Paths) {
        let peer_id = inner
            .lock()
            .unwrap()
            .peer_public_key()
            .map(|pk| pk.peer_id());
        let Some(peer_id) = peer_id else {
            return;
        };
        let relay = paths.read().unwrap().iter().find_map(|p| match &*p.transport {
            Transport::Tunnel { relay, .. } => Some(relay.clone()),
            _ => None,
        });
        let Some(relay) = relay else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = relay.send_holepunch_request(peer_id).await {
                trace!(?err, "auto holepunch request failed");
            }
        });
    }

    /// For each Tunnel path, ask its relay whether a `HolePunchNotify` for
    /// our peer has arrived. If yes, add a Probing Direct path so we begin
    /// dual-sending — first valid recv promotes it to Active.
    fn poll_pending_holepunch(paths: &Paths, socket: &Arc<UdpSocket>) {
        let pending: Vec<SocketAddr> = {
            let guard = paths.read().unwrap();
            guard
                .iter()
                .filter_map(|p| match &*p.transport {
                    Transport::Tunnel { relay, peer_id } => relay.take_pending_holepunch(peer_id),
                    _ => None,
                })
                .collect()
        };
        if pending.is_empty() {
            return;
        }
        let mut guard = paths.write().unwrap();
        for addr in pending {
            if guard.iter().any(|p| p.addr_key == addr) {
                continue;
            }
            let transport = Transport::udp(socket.clone(), addr);
            let path = Path::new(transport, addr, PathState::Probing);
            guard.push(path);
        }
    }
}

/// Context for accept + notify operations inside main_loop.
struct AcceptCtx<'a> {
    inner: &'a Arc<Mutex<Inner>>,
    stream_notifies: &'a NotifyMap,
    channel_notifies: &'a NotifyMap,
    accept_stream_tx: &'a mpsc::Sender<Stream>,
    accept_channel_tx: &'a mpsc::Sender<Channel>,
    send_notify: &'a Arc<Notify>,
    cancel_token: &'a CancellationToken,
    /// Stream IDs that couldn't be accepted last time (queue was full).
    pending_accept_streams: Vec<u64>,
    /// Channel IDs that couldn't be accepted last time (queue was full).
    pending_accept_channels: Vec<u64>,
    /// Reusable scratch buffers for drain_updated_* / writable_streams.
    scratch_streams: Vec<u64>,
    scratch_channels: Vec<u64>,
    scratch_writable: Vec<u64>,
}

/// Wake streams/channels with updated state.
/// Auto-accept new peer-initiated streams/channels.
/// If the accept queue is full, deferred IDs are saved in `ctx.pending_accept_*`
/// and retried on the next call.
fn notify_and_accept(ctx: &mut AcceptCtx<'_>) {
    // Reuse scratch buffers across calls — drain_* appends, we clear first.
    ctx.scratch_streams.clear();
    ctx.scratch_channels.clear();
    ctx.scratch_writable.clear();
    {
        let mut conn = ctx.inner.lock().unwrap();
        conn.drain_updated_streams(&mut ctx.scratch_streams);
        conn.drain_updated_channels(&mut ctx.scratch_channels);
        ctx.scratch_writable.extend(conn.writable_streams());
    }

    // Prepend previously deferred IDs before new ones.
    ctx.pending_accept_streams.append(&mut ctx.scratch_streams);
    let streams_to_process = std::mem::take(&mut ctx.pending_accept_streams);

    let mut sn = ctx.stream_notifies.lock().unwrap();
    for id in streams_to_process {
        if let Some(notify) = sn.get(&id) {
            notify.notify_one();
            continue;
        }
        // See the matching comment in the channel loop below: ids
        // surface both for newly opened streams AND for state changes on
        // our own locally-opened streams (close_send / cleanup). Only
        // accept when the stream is present and our send side is still
        // open — otherwise this is a surfacing of one of our own
        // streams, not a fresh peer-initiated one.
        let stream_is_fresh_peer = {
            let conn = ctx.inner.lock().unwrap();
            conn.is_stream_writable(id)
        };
        if !stream_is_fresh_peer {
            trace!(stream_id = id, "skipping accept: not a fresh peer stream");
            continue;
        }
        let Ok(permit) = ctx.accept_stream_tx.try_reserve() else {
            trace!(stream_id = id, "accept queue full, deferring stream");
            ctx.pending_accept_streams.push(id);
            continue;
        };
        trace!(stream_id = id, "auto-accepting new stream");
        let notify = Arc::new(Notify::new());
        notify.notify_one();
        sn.insert(id, notify.clone());
        permit.send(Stream {
            stream_id: id,
            inner: ctx.inner.clone(),
            notify,
            send_notify: ctx.send_notify.clone(),
            stream_notifies: ctx.stream_notifies.clone(),
            cancel_token: ctx.cancel_token.child_token(),
        });
    }

    // Wake writers blocked on full buffer (ACK freed space).
    for &id in &ctx.scratch_writable {
        if let Some(notify) = sn.get(&id) {
            notify.notify_one();
        }
    }
    drop(sn);

    ctx.pending_accept_channels.append(&mut ctx.scratch_channels);
    let channels_to_process = std::mem::take(&mut ctx.pending_accept_channels);

    let mut cn = ctx.channel_notifies.lock().unwrap();
    for id in channels_to_process {
        if let Some(notify) = cn.get(&id) {
            notify.notify_one();
            continue;
        }
        // `drain_updated_channels` surfaces ids for many state changes,
        // including `close_send` + `try_cleanup` on our own locally
        // opened channels after `Channel::drop`. Without this guard a
        // stale handle for our just-closed id lands in the accept queue
        // and a later `channel_send` on it fails with `IdReused` once
        // the manager's `local_next_id` has advanced past it. Only
        // surface ids that actually represent an open peer-initiated
        // channel: present in the map with our send side still open.
        let channel_is_fresh_peer = {
            let conn = ctx.inner.lock().unwrap();
            conn.is_channel_writable(id)
        };
        if !channel_is_fresh_peer {
            trace!(channel_id = id, "skipping accept: not a fresh peer channel");
            continue;
        }
        let Ok(permit) = ctx.accept_channel_tx.try_reserve() else {
            trace!(channel_id = id, "accept queue full, deferring channel");
            ctx.pending_accept_channels.push(id);
            continue;
        };
        trace!(channel_id = id, "auto-accepting new channel");
        let notify = Arc::new(Notify::new());
        notify.notify_one();
        cn.insert(id, notify.clone());
        permit.send(Channel {
            channel_id: id,
            inner: ctx.inner.clone(),
            notify,
            send_notify: ctx.send_notify.clone(),
            channel_notifies: ctx.channel_notifies.clone(),
            cancel_token: ctx.cancel_token.child_token(),
        });
    }
}

fn notify_all(map: &NotifyMap) {
    let m = map.lock().unwrap();
    for notify in m.values() {
        notify.notify_one();
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
