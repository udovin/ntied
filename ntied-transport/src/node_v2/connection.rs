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

use super::channel::{DatagramChannel, StreamChannel};

const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// Next stream ID. Initiator: even (0,2,4...), responder: odd (1,3,5...).
    next_stream_id: AtomicU64,
    /// Next channel ID. Same even/odd convention.
    next_channel_id: AtomicU64,
    pub(crate) stream_notifies: NotifyMap,
    pub(crate) channel_notifies: NotifyMap,
    pub(crate) conn_notify: Arc<Notify>,
    /// Wakes main_loop when channels write data (including FIN-on-drop).
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) accept_stream_rx: TokioMutex<mpsc::Receiver<StreamChannel>>,
    pub(crate) accept_channel_rx: TokioMutex<mpsc::Receiver<DatagramChannel>>,
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

        let mut buf = [0u8; 2048];
        match conn.send(&mut buf, Instant::now()) {
            Ok((n, _)) => {
                if let Err(err) = socket.send_to(&buf[..n], addr).await {
                    warn!(?err, "Failed to send InitAck");
                    return;
                }
            }
            Err(e) => {
                warn!(?e, "Failed to generate InitAck");
                return;
            }
        }

        let inner = Arc::new(Mutex::new(conn));
        let stream_notifies: NotifyMap = Default::default();
        let channel_notifies: NotifyMap = Default::default();
        let conn_notify = Arc::new(Notify::new());
        let send_notify = Arc::new(Notify::new());
        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(1);
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(1);
        let (established_tx, established_rx) = oneshot::channel();

        let task = tokio::spawn(Self::main_loop(
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
                warn!("Connection closed during auth");
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
            warn!("Failed to send connection to accept queue");
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

        let mut buf = [0u8; 2048];
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
        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(1);
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(1);
        let (established_tx, established_rx) = oneshot::channel();

        let task = tokio::spawn(Self::main_loop(
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

    pub fn open_stream(&self) -> StreamChannel {
        let stream_id = self.next_stream_id.fetch_add(2, Ordering::Relaxed);
        let notify = Arc::new(Notify::new());
        self.stream_notifies
            .lock()
            .unwrap()
            .insert(stream_id, notify.clone());
        StreamChannel {
            stream_id,
            inner: self.inner.clone(),
            notify,
            send_notify: self.send_notify.clone(),
            stream_notifies: self.stream_notifies.clone(),
            socket: self.socket.clone(),
            addr: self.addr,
            cancel_token: self.cancel_token.child_token(),
        }
    }

    pub fn open_channel(&self) -> DatagramChannel {
        let channel_id = self.next_channel_id.fetch_add(2, Ordering::Relaxed);
        let notify = Arc::new(Notify::new());
        self.channel_notifies
            .lock()
            .unwrap()
            .insert(channel_id, notify.clone());
        DatagramChannel {
            channel_id,
            inner: self.inner.clone(),
            notify,
            send_notify: self.send_notify.clone(),
            channel_notifies: self.channel_notifies.clone(),
            socket: self.socket.clone(),
            addr: self.addr,
            cancel_token: self.cancel_token.child_token(),
        }
    }

    pub async fn accept_stream(&self) -> io::Result<StreamChannel> {
        self.accept_stream_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Connection closed"))
    }

    pub async fn accept_channel(&self) -> io::Result<DatagramChannel> {
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
            // Timeout to prevent hanging if main_loop is stuck.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn main_loop(
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
        accept_stream_tx: mpsc::Sender<StreamChannel>,
        accept_channel_tx: mpsc::Sender<DatagramChannel>,
    ) {
        let mut established_tx = established_tx;
        let mut send_buf = [0u8; 2048];
        let mut ping_interval = tokio::time::interval(PING_INTERVAL);
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let ctx = AcceptCtx {
            inner: &inner,
            stream_notifies: &stream_notifies,
            channel_notifies: &channel_notifies,
            accept_stream_tx: &accept_stream_tx,
            accept_channel_tx: &accept_channel_tx,
            socket: &socket,
            send_notify: &send_notify,
            addr,
            cancel_token: &cancel_token,
        };

        loop {
            let timeout_dur = {
                let conn = inner.lock().unwrap();
                conn.timeout().unwrap_or(std::time::Duration::from_secs(60))
            };
            let sleep = tokio::time::sleep(timeout_dur);

            tokio::select! {
                packet = rx.recv() => {
                    let Some(packet) = packet else { break };
                    let now = Instant::now();
                    let (is_closed, is_established) = {
                        let mut conn = inner.lock().unwrap();
                        if let Err(e) = conn.recv(&packet.data, RecvInfo { now }) {
                            trace!(?e, "recv error (dropped packet)");
                        }
                        (conn.is_closed(), conn.is_established())
                    };

                    if is_closed {
                        notify_all(&stream_notifies);
                        notify_all(&channel_notifies);
                        conn_notify.notify_waiters();
                        return;
                    }
                    if is_established {
                        if let Some(tx) = established_tx.take() {
                            let _ = tx.send(());
                        }
                    }

                    Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
                    notify_and_accept(&ctx);
                }
                _ = sleep => {
                    let now = Instant::now();
                    {
                        let mut conn = inner.lock().unwrap();
                        conn.on_timeout(now);
                        if conn.is_closed() {
                            drop(conn);
                            notify_all(&stream_notifies);
                            notify_all(&channel_notifies);
                            conn_notify.notify_waiters();
                            return;
                        }
                    }
                    Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
                }
                _ = ping_interval.tick() => {
                    {
                        let mut conn = inner.lock().unwrap();
                        if conn.is_established() {
                            conn.ping(Instant::now());
                        }
                    }
                    Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
                }
                _ = send_notify.notified() => {
                    // Channel wrote data (or FIN on drop) — just flush to network.
                    // Don't call notify_and_accept here — it's only needed on recv.
                    Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }

        {
            let mut conn = inner.lock().unwrap();
            let _ = conn.close(0, b"shutdown");
        }
        Self::drain_send(&inner, &mut send_buf, &socket, addr).await;
        notify_all(&stream_notifies);
        notify_all(&channel_notifies);
        conn_notify.notify_waiters();
    }

    async fn drain_send(
        inner: &Mutex<Inner>,
        buf: &mut [u8],
        socket: &UdpSocket,
        addr: SocketAddr,
    ) {
        loop {
            let result = {
                let mut conn = inner.lock().unwrap();
                conn.send(buf, Instant::now())
            };
            match result {
                Ok((n, _)) => {
                    if let Err(err) = socket.send_to(&buf[..n], addr).await {
                        warn!(?err, "Failed to send packet");
                    }
                }
                Err(_) => break,
            }
        }
    }
}

/// Context for accept + notify operations inside main_loop.
struct AcceptCtx<'a> {
    inner: &'a Arc<Mutex<Inner>>,
    stream_notifies: &'a NotifyMap,
    channel_notifies: &'a NotifyMap,
    accept_stream_tx: &'a mpsc::Sender<StreamChannel>,
    accept_channel_tx: &'a mpsc::Sender<DatagramChannel>,
    socket: &'a Arc<UdpSocket>,
    send_notify: &'a Arc<Notify>,
    addr: SocketAddr,
    cancel_token: &'a CancellationToken,
}

/// Wake streams/channels with readable data. Auto-create channels for new IDs.
fn notify_and_accept(ctx: &AcceptCtx<'_>) {
    let conn = ctx.inner.lock().unwrap();

    let readable: Vec<u64> = conn.readable_streams().collect();
    let readable_ch: Vec<u64> = conn.readable_channels().collect();
    drop(conn);

    let mut sn = ctx.stream_notifies.lock().unwrap();
    for id in readable {
        if let Some(notify) = sn.get(&id) {
            notify.notify_one();
        } else {
            // New incoming stream — only create if accept queue has room.
            let Ok(permit) = ctx.accept_stream_tx.try_reserve() else {
                continue; // queue full, will retry on next packet
            };
            let notify = Arc::new(Notify::new());
            notify.notify_one();
            sn.insert(id, notify.clone());
            let ch = StreamChannel {
                stream_id: id,
                inner: ctx.inner.clone(),
                notify,
                send_notify: ctx.send_notify.clone(),
                stream_notifies: ctx.stream_notifies.clone(),
                socket: ctx.socket.clone(),
                addr: ctx.addr,
                cancel_token: ctx.cancel_token.child_token(),
            };
            permit.send(ch);
        }
    }
    drop(sn);

    let mut cn = ctx.channel_notifies.lock().unwrap();
    for id in readable_ch {
        if let Some(notify) = cn.get(&id) {
            notify.notify_one();
        } else {
            let Ok(permit) = ctx.accept_channel_tx.try_reserve() else {
                continue;
            };
            let notify = Arc::new(Notify::new());
            notify.notify_one();
            cn.insert(id, notify.clone());
            let ch = DatagramChannel {
                channel_id: id,
                inner: ctx.inner.clone(),
                notify,
                send_notify: ctx.send_notify.clone(),
                channel_notifies: ctx.channel_notifies.clone(),
                socket: ctx.socket.clone(),
                addr: ctx.addr,
                cancel_token: ctx.cancel_token.child_token(),
            };
            permit.send(ch);
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
