use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rand::{RngCore, thread_rng};
use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::connection::Config;
use crate::wire::packet::{PacketHeader, parse_init, peek_header};
use crate::crypto::{PEER_ID_SIZE, PeerId, PrivateKey};

use super::channel::Channel;
use super::connection::{Connection, ConnectionMap, OwnedConnectionId, RawPacket};
use super::control::ControlMsg;
use super::relay::{RelayConnection, TUNNEL_HEADER_SIZE};

/// Shared Node-level context: identity, id allocator, accept queue sink,
/// shutdown token, primary UDP socket (needed to mint Direct paths for
/// hole-punched upgrades). Cloned cheaply (all fields are `Arc`/clonable
/// handles) and threaded into background tasks (`recv_loop`,
/// `RelayConnection` pump, tunneled accept spawn).
#[derive(Clone)]
pub(crate) struct NodeCtx {
    pub(crate) identity: Arc<PrivateKey>,
    pub(crate) next_connection_id: Arc<AtomicU64>,
    pub(crate) connection_map: ConnectionMap,
    pub(crate) accept_tx: mpsc::Sender<Connection>,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) socket: Arc<UdpSocket>,
    /// Default `Config` applied to every `Connection` this Node opens or
    /// accepts — both direct and relay-tunneled. Cheap to clone.
    pub(crate) config: Config,
}

/// Per-client state on the relay-server side.
#[derive(Clone)]
struct ClientHandles {
    tunnel_channel: Arc<Channel>,
    control_channel: Arc<Channel>,
    addr: SocketAddr,
}

pub struct Node {
    socket: Arc<UdpSocket>,
    ctx: NodeCtx,
    accept_rx: TokioMutex<mpsc::Receiver<Connection>>,
    recv_task: Mutex<Option<JoinHandle<()>>>,
    /// One connection per relay address; tunnels for many peers
    /// multiplex over the relay's tunnel_channel.
    relay_pool: TokioMutex<HashMap<SocketAddr, Arc<RelayConnection>>>,
}

impl Node {
    const PACKET_BUFFER_SIZE: usize = 64;
    const RECV_BUFFER_SIZE: usize = 2048;

    pub async fn bind(addr: SocketAddr, private_key: PrivateKey) -> io::Result<Self> {
        Self::bind_with_config(addr, private_key, Config::default()).await
    }

    /// Bind with a caller-provided `Config`. The `Config` is stored in
    /// the Node and applied to every `Connection` it opens or accepts —
    /// both direct and relay-tunneled. Use this to raise, e.g.,
    /// `channel_buf_size` for applications that carry large messages
    /// (software-encoded video frames) on call channels.
    pub async fn bind_with_config(
        addr: SocketAddr,
        private_key: PrivateKey,
        config: Config,
    ) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let (accept_tx, accept_rx) = mpsc::channel(1);
        let ctx = NodeCtx {
            identity: Arc::new(private_key),
            next_connection_id: Arc::new(AtomicU64::new(thread_rng().next_u64())),
            connection_map: Default::default(),
            accept_tx,
            cancel_token: CancellationToken::new(),
            socket: socket.clone(),
            config,
        };
        let recv_task = tokio::spawn(Self::recv_loop(socket.clone(), ctx.clone()));
        Ok(Self {
            socket,
            ctx,
            accept_rx: TokioMutex::new(accept_rx),
            recv_task: Mutex::new(Some(recv_task)),
            relay_pool: TokioMutex::new(HashMap::new()),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.ctx.identity.public_key().peer_id()
    }

    pub async fn accept(&self) -> io::Result<Connection> {
        self.accept_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Node shutdown"))
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<Connection> {
        let connection_id = self.ctx.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(Self::PACKET_BUFFER_SIZE);
        let owned_connection_id =
            OwnedConnectionId::new(connection_id, &self.ctx.connection_map, tx);

        Connection::connect(
            owned_connection_id,
            rx,
            self.socket.clone(),
            (*self.ctx.identity).clone(),
            self.ctx.cancel_token.child_token(),
            addr,
            self.ctx.config.clone(),
        )
        .await
    }

    /// Connect to `peer_id` through the relay at `relay_addr`. Establishes
    /// (or reuses) a connection to the relay, then runs the peer-to-peer
    /// handshake nested inside the relay's multiplex tunnel channel.
    pub async fn connect_via_relay(
        &self,
        peer_id: PeerId,
        relay_addr: SocketAddr,
    ) -> io::Result<Connection> {
        let relay = self.open_relay(relay_addr).await?;
        let (transport, rx, tx) = relay.open_tunnel(peer_id, Self::PACKET_BUFFER_SIZE);
        let cid = self.ctx.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let owned = OwnedConnectionId::tunneled(
            cid,
            relay,
            self.ctx.connection_map.clone(),
            tx,
        );
        Connection::connect_tunneled(
            owned,
            rx,
            transport,
            relay_addr,
            self.socket.clone(),
            (*self.ctx.identity).clone(),
            self.ctx.cancel_token.child_token(),
            self.ctx.config.clone(),
        )
        .await
    }

    /// Attach to `relay_addr` so this node can receive incoming connections
    /// routed through it. Idempotent: subsequent calls reuse the existing
    /// relay connection.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        self.open_relay(relay_addr).await?;
        Ok(())
    }

    /// Connect to `peer_id` via any currently attached relay. Returns
    /// `NotFound` if no relay is attached. For multi-relay setups, prefer
    /// `connect_via_relay(peer_id, relay_addr)` to pick explicitly.
    pub async fn connect_peer(&self, peer_id: PeerId) -> io::Result<Connection> {
        let relay_addr = {
            let pool = self.relay_pool.lock().await;
            pool.keys().next().copied()
        };
        let Some(relay_addr) = relay_addr else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no relay attached",
            ));
        };
        self.connect_via_relay(peer_id, relay_addr).await
    }

    /// True if at least one relay is currently attached to this node.
    pub async fn is_relay_attached(&self) -> bool {
        !self.relay_pool.lock().await.is_empty()
    }

    /// Get or open a connection to a relay. The first time we see
    /// `relay_addr`, we establish a connection and open its multiplex
    /// `tunnel_channel` and `control_channel`; subsequent calls return the
    /// cached relay handle.
    pub(crate) async fn open_relay(
        &self,
        relay_addr: SocketAddr,
    ) -> io::Result<Arc<RelayConnection>> {
        let mut pool = self.relay_pool.lock().await;
        if let Some(existing) = pool.get(&relay_addr) {
            return Ok(existing.clone());
        }
        let conn = self.connect(relay_addr).await?;
        let tunnel_channel = conn
            .open_channel()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("open_channel: {e:?}")))?;
        let control_channel = conn
            .open_channel()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("open_channel: {e:?}")))?;
        let relay = RelayConnection::new(
            relay_addr,
            conn,
            tunnel_channel,
            control_channel,
            self.ctx.clone(),
        );
        pool.insert(relay_addr, relay.clone());
        Ok(relay)
    }

    /// Run this node as a minimal relay server.
    ///
    /// For each accepted client connection, the relay accepts the client's
    /// first two channels as the multiplex `tunnel_channel` and the
    /// `control_channel`. Tunnel messages are forwarded between clients
    /// (rewriting `[dest|payload]` → `[src|payload]`); control messages
    /// (`HolePunchRequest`) trigger bidirectional `HolePunchNotify` to
    /// the requester and target.
    ///
    /// Returns when `shutdown()` is called or accept fails.
    pub async fn serve_as_relay(&self) -> io::Result<()> {
        let clients: Arc<TokioMutex<HashMap<PeerId, ClientHandles>>> = Default::default();
        loop {
            let conn = self.accept().await?;
            let peer_id = match conn.peer_id() {
                Some(p) => p,
                None => {
                    warn!("relay: accepted connection without peer_id");
                    continue;
                }
            };
            let client_addr = match conn.remote_addr() {
                Some(a) => a,
                None => {
                    warn!(?peer_id, "relay: accepted connection without remote_addr");
                    continue;
                }
            };
            let tunnel_channel = match conn.accept_channel().await {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    warn!(?err, ?peer_id, "relay: failed to accept tunnel channel");
                    continue;
                }
            };
            let control_channel = match conn.accept_channel().await {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    warn!(?err, ?peer_id, "relay: failed to accept control channel");
                    continue;
                }
            };
            clients.lock().await.insert(
                peer_id,
                ClientHandles {
                    tunnel_channel: tunnel_channel.clone(),
                    control_channel: control_channel.clone(),
                    addr: client_addr,
                },
            );
            let conn = Arc::new(conn);
            let cancel = self.ctx.cancel_token.child_token();
            let clients_tunnel = clients.clone();
            let clients_control = clients.clone();
            let conn_clone = conn.clone();
            let cancel_tunnel = cancel.clone();
            tokio::spawn(async move {
                let _g = conn_clone;
                Self::relay_pump(peer_id, tunnel_channel, clients_tunnel, cancel_tunnel).await;
            });
            tokio::spawn(async move {
                let _g = conn;
                Self::relay_control_pump(peer_id, control_channel, clients_control, cancel).await;
            });
        }
    }

    async fn relay_pump(
        from_peer_id: PeerId,
        from_channel: Arc<Channel>,
        clients: Arc<TokioMutex<HashMap<PeerId, ClientHandles>>>,
        cancel: CancellationToken,
    ) {
        loop {
            let msg = tokio::select! {
                msg = from_channel.recv() => msg,
                _ = cancel.cancelled() => break,
            };
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            if msg.len() < TUNNEL_HEADER_SIZE {
                continue;
            }
            let mut to_bytes = [0u8; PEER_ID_SIZE];
            to_bytes.copy_from_slice(&msg[..PEER_ID_SIZE]);
            let to_peer = PeerId::from_bytes(to_bytes);

            let to_channel = clients
                .lock()
                .await
                .get(&to_peer)
                .map(|h| h.tunnel_channel.clone());
            let Some(to_channel) = to_channel else {
                trace!(?to_peer, "relay: destination not connected, dropping");
                continue;
            };

            let mut out = Vec::with_capacity(msg.len());
            out.extend_from_slice(from_peer_id.as_bytes());
            out.extend_from_slice(&msg[TUNNEL_HEADER_SIZE..]);
            if let Err(err) = to_channel.send(out).await {
                trace!(?err, ?to_peer, "relay: forward send failed");
            }
        }
        clients.lock().await.remove(&from_peer_id);
        trace!(?from_peer_id, "relay: client removed");
    }

    async fn relay_control_pump(
        from_peer_id: PeerId,
        control: Arc<Channel>,
        clients: Arc<TokioMutex<HashMap<PeerId, ClientHandles>>>,
        cancel: CancellationToken,
    ) {
        loop {
            let msg = tokio::select! {
                msg = control.recv() => msg,
                _ = cancel.cancelled() => break,
            };
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            let Some(parsed) = ControlMsg::decode(&msg) else {
                warn!("relay: control msg decode failed");
                continue;
            };
            match parsed {
                ControlMsg::HolePunchRequest { target } => {
                    let map = clients.lock().await;
                    let target_handles = map.get(&target).cloned();
                    let from_handles = map.get(&from_peer_id).cloned();
                    drop(map);
                    let (Some(t), Some(f)) = (target_handles, from_handles) else {
                        trace!(
                            ?target,
                            ?from_peer_id,
                            "relay: holepunch target or source missing"
                        );
                        continue;
                    };
                    // Notify the target about the requester.
                    let notify_to_target = ControlMsg::HolePunchNotify {
                        from: from_peer_id,
                        addr: f.addr,
                    }
                    .encode();
                    if let Err(err) = t.control_channel.send(notify_to_target).await {
                        trace!(?err, "relay: notify_to_target failed");
                    }
                    // Notify the requester about the target.
                    let notify_to_requester = ControlMsg::HolePunchNotify {
                        from: target,
                        addr: t.addr,
                    }
                    .encode();
                    if let Err(err) = f.control_channel.send(notify_to_requester).await {
                        trace!(?err, "relay: notify_to_requester failed");
                    }
                }
                ControlMsg::HolePunchNotify { .. } => {
                    warn!("relay: server received HolePunchNotify, ignoring");
                }
            }
        }
    }

    pub async fn shutdown(&self) -> Result<(), JoinError> {
        let recv_task = self.recv_task.lock().unwrap().take();
        if let Some(task) = recv_task {
            self.ctx.cancel_token.cancel();
            task.await?;
        }
        Ok(())
    }

    async fn recv_loop(socket: Arc<UdpSocket>, ctx: NodeCtx) {
        let mut buf = vec![0u8; Self::RECV_BUFFER_SIZE];
        loop {
            tokio::select! {
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((len, addr)) => {
                            let data = &buf[..len];
                            let header = match peek_header(data) {
                                Ok(h) => h,
                                Err(_) => {
                                    warn!("Failed to peek packet header");
                                    continue;
                                }
                            };
                            match header {
                                PacketHeader::Init { initiator_connection_id } => {
                                    trace!(
                                        peer_connection_id = initiator_connection_id,
                                        "Received Init packet"
                                    );
                                    let init = match parse_init(data) {
                                        Ok(p) => p,
                                        Err(_) => {
                                            warn!("Failed to parse Init");
                                            continue;
                                        }
                                    };
                                    let responder_id = ctx.next_connection_id.fetch_add(1, Ordering::Relaxed);
                                    let (tx, rx) = mpsc::channel(Self::PACKET_BUFFER_SIZE);
                                    let owned_id = OwnedConnectionId::new(
                                        responder_id,
                                        &ctx.connection_map,
                                        tx,
                                    );
                                    let conn_cancel = ctx.cancel_token.child_token();
                                    tokio::spawn(Connection::accept(
                                        responder_id,
                                        init.initiator_connection_id,
                                        init.kem_public_key,
                                        owned_id,
                                        socket.clone(),
                                        (*ctx.identity).clone(),
                                        rx,
                                        ctx.accept_tx.clone(),
                                        conn_cancel,
                                        addr,
                                        ctx.config.clone(),
                                    ));
                                }
                                PacketHeader::InitAck { initiator_connection_id, .. } => {
                                    trace!(
                                        connection_id = initiator_connection_id,
                                        "Received InitAck packet"
                                    );
                                    let map = ctx.connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&initiator_connection_id) {
                                        let raw = RawPacket { data: data.to_vec(), addr };
                                        if let Err(err) = tx.try_send(raw) {
                                            warn!(?err, "Failed to route InitAck");
                                        }
                                    }
                                }
                                PacketHeader::Data { receiver_connection_id, .. } => {
                                    let map = ctx.connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&receiver_connection_id) {
                                        let raw = RawPacket { data: data.to_vec(), addr };
                                        if let Err(err) = tx.try_send(raw) {
                                            trace!(?err, "Failed to route Data packet");
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            if cfg!(target_os = "windows")
                                && err.kind() == io::ErrorKind::ConnectionReset
                            {
                                trace!("Ignored connection reset");
                                continue;
                            }
                            warn!(?err, "Failed to receive from UDP socket");
                        }
                    }
                }
                _ = ctx.cancel_token.cancelled() => {
                    trace!("Receive loop stopped");
                    return;
                }
            }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let recv_task = self.recv_task.lock().unwrap().take();
        if let Some(task) = recv_task {
            self.ctx.cancel_token.cancel();
            drop(task);
        }
    }
}
