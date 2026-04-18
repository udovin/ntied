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

use crate::connection_v2::{Connection as Inner, ConnectionId, RecvInfo};
use crate::crypto::{KemPublicKey, PeerId, PrivateKey, PublicKey};

use super::channel::Channel;
use super::stream::Stream;

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
    connection_map: ConnectionMap,
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
    pub(crate) connection_id: OwnedConnectionId,
    pub(crate) inner: Arc<Mutex<Inner>>,
    next_stream_id: AtomicU64,
    next_channel_id: AtomicU64,
    pub(crate) stream_notifies: NotifyMap,
    pub(crate) channel_notifies: NotifyMap,
    pub(crate) conn_notify: Arc<Notify>,
    /// Wakes main_loop when channels write data (including FIN-on-drop).
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) accept_stream_rx: TokioMutex<mpsc::Receiver<Stream>>,
    pub(crate) accept_channel_rx: TokioMutex<mpsc::Receiver<Channel>>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
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
        let mut conn = Inner::accept(
            ConnectionId(local_id),
            ConnectionId(peer_id),
            peer_kem_pk,
            identity,
        );

        let mut buf = [0u8; 1280];
        match conn.send(&mut buf, Instant::now()) {
            Ok((n, _)) => {
                if let Err(err) = socket.send_to(&buf[..n], addr).await {
                    warn!(?err, "failed to send InitAck");
                    return;
                }
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
            socket.clone(),
            addr,
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
            conn_notify,
            send_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_channel_rx: TokioMutex::new(accept_channel_rx),
            socket,
            addr,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        };
        if accept_tx.send(connection).await.is_err() {
            warn!("failed to send connection to accept queue");
        }
    }

    pub(crate) async fn connect(
        connection_id: OwnedConnectionId,
        mut rx: mpsc::Receiver<RawPacket>,
        socket: Arc<UdpSocket>,
        identity: PrivateKey,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) -> io::Result<Connection> {
        let mut conn = Inner::open(ConnectionId(connection_id.id()), identity);

        let mut buf = [0u8; 1280];
        let (n, _) = conn
            .send(&mut buf, Instant::now())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        socket.send_to(&buf[..n], addr).await?;

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

        conn.recv(
            &init_ack.data,
            RecvInfo {
                now: Instant::now(),
            },
        )
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;

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
            socket.clone(),
            addr,
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
            conn_notify,
            send_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_channel_rx: TokioMutex::new(accept_channel_rx),
            socket,
            addr,
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

    pub fn remote_addr(&self) -> SocketAddr {
        self.addr
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
            socket: self.socket.clone(),
            addr: self.addr,
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
            socket: self.socket.clone(),
            addr: self.addr,
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
        socket: Arc<UdpSocket>,
        addr: SocketAddr,
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
            socket: &socket,
            send_notify: &send_notify,
            addr,
            cancel_token: &cancel_token,
            pending_accept_streams: Vec::new(),
            pending_accept_channels: Vec::new(),
            scratch_streams: Vec::new(),
            scratch_channels: Vec::new(),
            scratch_writable: Vec::new(),
        };

        // Initial drain: send any pending frames (e.g. auth data after handshake).
        let sent = Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
        trace!(conn_id, packets_sent = sent, "drain_send initial");

        // Check if connection established during initial drain (auth may have completed).
        {
            let conn = inner.lock().unwrap();
            if conn.is_established() {
                if let Some(tx) = established_tx.take() {
                    trace!(conn_id, "connection established after initial drain");
                    let _ = tx.send(());
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
                    let Some(packet) = packet else {
                        trace!(conn_id, "rx channel closed, exiting main_loop");
                        break;
                    };
                    let now = Instant::now();
                    let pkt_len = packet.data.len();
                    let (is_closed, is_established) = {
                        let mut conn = inner.lock().unwrap();
                        if let Err(e) = conn.recv(&packet.data, RecvInfo { now }) {
                            trace!(?e, pkt_len, "recv error, dropping packet");
                        }
                        (conn.is_closed(), conn.is_established())
                    };

                    trace!(conn_id, pkt_len, is_closed, is_established, "processed rx packet");

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
                        }
                    }

                    let sent = Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
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
                    let sent = Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
                    trace!(conn_id, packets_sent = sent, "drain_send after timeout");
                }
                _ = send_notify.notified() => {
                    let sent = Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
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
            let sent = Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
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
        socket: &UdpSocket,
        addr: SocketAddr,
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
                    if let Err(err) = socket.send_to(&buf[..n], addr).await {
                        warn!(?err, "failed to send packet");
                    }
                }
                Err(_) => break,
            }
        }
        count
    }
}

/// Context for accept + notify operations inside main_loop.
struct AcceptCtx<'a> {
    inner: &'a Arc<Mutex<Inner>>,
    stream_notifies: &'a NotifyMap,
    channel_notifies: &'a NotifyMap,
    accept_stream_tx: &'a mpsc::Sender<Stream>,
    accept_channel_tx: &'a mpsc::Sender<Channel>,
    socket: &'a Arc<UdpSocket>,
    send_notify: &'a Arc<Notify>,
    addr: SocketAddr,
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
        } else {
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
                socket: ctx.socket.clone(),
                addr: ctx.addr,
                cancel_token: ctx.cancel_token.child_token(),
            });
        }
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
        } else {
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
                socket: ctx.socket.clone(),
                addr: ctx.addr,
                cancel_token: ctx.cancel_token.child_token(),
            });
        }
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
