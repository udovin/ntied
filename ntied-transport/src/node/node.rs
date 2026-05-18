use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::{RngCore, thread_rng};
use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::connection::Config;
use crate::discovery::{Discovery, PeerRoutes};
use crate::wire::packet::{PacketHeader, parse_init, peek_header};
use crate::crypto::{PEER_ID_SIZE, PeerId, PrivateKey};

use super::channel::Channel;
use super::connection::{Connection, ConnectionMap, OwnedConnectionId, RawPacket};
use super::control::ControlMsg;
use super::pool::{self, PoolEntry, RelaySource};
use super::relay::TUNNEL_HEADER_SIZE;

/// Max time `connect_relay_peer` / `connect_peer` waits for an in-progress
/// relay supervisor to establish a live transport before erroring out.
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Tunable parameters for discovery's relay-pool top-up loop.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Appended to mainline's default bootstrap node list.  Empty means
    /// defaults only.
    pub extra_bootstrap: Vec<SocketAddr>,
    /// How many `Discovery`-sourced relays we aim to keep in the pool.
    /// `Attached` relays are unlimited and not counted toward this.
    pub relay_target: usize,
    /// How often the top-up loop runs (scan, shed, refill).
    pub topup_interval: Duration,
    /// `Discovery` entries younger than this are never shed, even if idle.
    /// Prevents a freshly-found relay from being killed before any tunnel
    /// has had a chance to open through it.
    pub grace_period: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            extra_bootstrap: Vec::new(),
            relay_target: 1,
            topup_interval: Duration::from_secs(30),
            grace_period: Duration::from_secs(60),
        }
    }
}

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
    /// One pool entry per relay address; tunnels for many peers multiplex
    /// over each entry's `tunnel_channel`.  Each entry owns a supervisor
    /// task that reconnects (for `Attached`) or exits on disconnect (for
    /// `Discovery`, with the top-up loop spawning a replacement).
    relay_pool: Arc<TokioMutex<HashMap<SocketAddr, Arc<PoolEntry>>>>,
    /// Canonical set of relay addresses the user has explicitly attached
    /// via `attach_relay`.  Persists across reconnects.  The corresponding
    /// pool entries are kept `Attached` while addr is in this set.
    attached: Arc<TokioMutex<HashSet<SocketAddr>>>,
    /// DHT-backed peer/relay discovery.  Lazily created by `enable_discovery`;
    /// cloned out into helper methods that need to await on the actor.
    discovery: TokioMutex<Option<Arc<Discovery>>>,
    /// Top-up task handle (active after `enable_discovery`).
    topup_task: Mutex<Option<JoinHandle<()>>>,
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
            relay_pool: Arc::new(TokioMutex::new(HashMap::new())),
            attached: Arc::new(TokioMutex::new(HashSet::new())),
            discovery: TokioMutex::new(None),
            topup_task: Mutex::new(None),
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
    /// (or reuses) a pool entry for the relay, waits for the supervisor's
    /// transport to come up, then runs the peer-to-peer handshake nested
    /// inside the relay's multiplex tunnel channel.
    ///
    /// If `relay_addr` is not yet in the pool, a transient `Discovery`
    /// entry is created (it will be shed by the top-up loop once idle
    /// past the grace period).
    pub async fn connect_relay_peer(
        &self,
        relay_addr: SocketAddr,
        peer_id: PeerId,
    ) -> io::Result<Connection> {
        let entry = self.get_or_create_entry(relay_addr, RelaySource::Discovery).await;
        let relay = pool::wait_for_connection(&entry, RELAY_CONNECT_TIMEOUT).await?;
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

    /// Attach `relay_addr` to the persistent relay list.  The supervisor
    /// keeps a live connection to this address with exponential-backoff
    /// reconnect (1s → 2s → … → 60s cap, reset on success).  Idempotent.
    ///
    /// If the address is already in the pool as `Discovery`, it is
    /// promoted to `Attached` (so the supervisor stops exiting on
    /// disconnect).  If the address is already attached, this is a no-op.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        self.attached.lock().await.insert(relay_addr);
        let entry = self
            .get_or_create_entry(relay_addr, RelaySource::Attached)
            .await;
        entry.set_source(RelaySource::Attached);
        pool::ensure_supervisor(&entry, &self.ctx, Self::PACKET_BUFFER_SIZE);
        Ok(())
    }

    /// Remove `relay_addr` from the persistent list.  Does NOT hard-close
    /// the connection — the entry is demoted to `Discovery` and lives on
    /// until either a tunnel is still using it (active tunnels keep it
    /// alive) or the top-up loop sheds it after the grace period.
    /// Returns `true` if the address was actually attached.
    pub async fn detach_relay(&self, relay_addr: SocketAddr) -> bool {
        let was = self.attached.lock().await.remove(&relay_addr);
        if let Some(entry) = self.relay_pool.lock().await.get(&relay_addr).cloned() {
            entry.set_source(RelaySource::Discovery);
        }
        was
    }

    /// Snapshot of the persistent attached relay set.
    pub async fn attached_relays(&self) -> Vec<SocketAddr> {
        self.attached.lock().await.iter().copied().collect()
    }

    /// Block until `relay_addr` has a live underlying connection, or
    /// `timeout` elapses.  `NotFound` if the address is not in the pool;
    /// `TimedOut` if the supervisor cannot connect in time (but stays
    /// alive, so subsequent waits may succeed).
    pub async fn wait_relay_connected(
        &self,
        relay_addr: SocketAddr,
        timeout: Duration,
    ) -> io::Result<()> {
        let entry = {
            let p = self.relay_pool.lock().await;
            p.get(&relay_addr).cloned()
        };
        let entry = entry.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "relay not in pool")
        })?;
        pool::wait_for_connection(&entry, timeout).await.map(|_| ())
    }

    /// True if at least one relay is in the persistent attached set.  Note:
    /// the underlying transport may still be reconnecting at the moment
    /// you call this.
    pub async fn is_relay_attached(&self) -> bool {
        !self.attached.lock().await.is_empty()
    }

    /// Get-or-create a pool entry.  Spawns a supervisor on creation.
    /// Idempotent: existing entries are returned as-is (no source change).
    async fn get_or_create_entry(
        &self,
        relay_addr: SocketAddr,
        initial_source: RelaySource,
    ) -> Arc<PoolEntry> {
        let mut pool = self.relay_pool.lock().await;
        if let Some(existing) = pool.get(&relay_addr) {
            // Make sure supervisor is alive (it may have exited as
            // Discovery; if caller is `attach_relay`, the source is set
            // separately and supervisor is respawned there).
            pool::ensure_supervisor(existing, &self.ctx, Self::PACKET_BUFFER_SIZE);
            return existing.clone();
        }
        let entry = PoolEntry::new(relay_addr, initial_source);
        pool::ensure_supervisor(&entry, &self.ctx, Self::PACKET_BUFFER_SIZE);
        pool.insert(relay_addr, entry.clone());
        entry
    }

    // -- Discovery (BitTorrent DHT) ------------------------------------------

    /// Initialise BitTorrent-DHT–based discovery.  Idempotent — subsequent
    /// calls return the existing handle and re-use the existing top-up task.
    ///
    /// Spawns a background top-up loop driven by [`DiscoveryConfig`]:
    /// periodically scans the pool, sheds idle Discovery entries past the
    /// grace period, and refills up to `relay_target` from `lookup_relays`.
    /// The DHT actor runs on its own thread with its own UDP socket.
    pub async fn enable_discovery(
        &self,
        config: DiscoveryConfig,
    ) -> io::Result<Arc<Discovery>> {
        let mut slot = self.discovery.lock().await;
        if let Some(existing) = slot.as_ref() {
            return Ok(existing.clone());
        }
        let d = Arc::new(Discovery::new(&config.extra_bootstrap)?);
        *slot = Some(d.clone());
        // Spawn the top-up loop.
        let pool = self.relay_pool.clone();
        let ctx = self.ctx.clone();
        let cancel = self.ctx.cancel_token.child_token();
        let discovery_clone = d.clone();
        let config_clone = config;
        let handle = tokio::spawn(topup_loop(
            pool,
            discovery_clone,
            config_clone,
            ctx,
            cancel,
            Self::PACKET_BUFFER_SIZE,
        ));
        *self.topup_task.lock().unwrap() = Some(handle);
        Ok(d)
    }

    /// Return the discovery handle if [`enable_discovery`](Self::enable_discovery)
    /// has been called.
    pub async fn discovery(&self) -> Option<Arc<Discovery>> {
        self.discovery.lock().await.clone()
    }

    fn discovery_or_err(d: Option<Arc<Discovery>>) -> io::Result<Arc<Discovery>> {
        d.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "discovery not enabled — call Node::enable_discovery first",
            )
        })
    }

    /// Publish this node as a public-IPv4 peer in the DHT.  Only meaningful
    /// when the bind address is reachable from the outside — the DHT
    /// records whichever IP it sees us announce from.  Re-announces
    /// every ~25 min until the `Node` is dropped.
    pub async fn enable_public_peer(&self) -> io::Result<()> {
        let d = Self::discovery_or_err(self.discovery().await)?;
        let port = self.local_addr()?.port();
        d.announce_self_direct(self.peer_id(), port).await;
        Ok(())
    }

    /// Publish this node as a public-IPv4 relay in the DHT (`H_relays`
    /// open registry).  Should only be enabled on nodes actually running
    /// `serve_as_relay`.  Re-announces periodically until dropped.
    pub async fn enable_public_relay(&self) -> io::Result<()> {
        let d = Self::discovery_or_err(self.discovery().await)?;
        let port = self.local_addr()?.port();
        d.announce_self_as_relay(port).await;
        Ok(())
    }

    /// Look up `peer_id` in the DHT.  Queries both the "direct" and "via
    /// relay" info_hashes in parallel; returns whatever is currently
    /// indexed (may be empty during bootstrap).
    pub async fn lookup_peer(&self, peer_id: PeerId) -> io::Result<PeerRoutes> {
        let d = Self::discovery_or_err(self.discovery().await)?;
        Ok(d.lookup_peer(peer_id).await)
    }

    /// Find relay addresses currently in the open `H_relays` registry.
    /// Order is DHT-response order — caller may want to ping / RTT-rank to
    /// pick a close one.
    pub async fn lookup_relays(&self) -> io::Result<Vec<SocketAddr>> {
        let d = Self::discovery_or_err(self.discovery().await)?;
        Ok(d.lookup_relays().await)
    }

    /// Connect to `peer_id` via DHT discovery.  Always queries the DHT;
    /// tries direct addresses first, then relay addresses.  Returns the
    /// first successful connection.  `NotFound` if no route works.
    pub async fn connect_peer(&self, peer_id: PeerId) -> io::Result<Connection> {
        let routes = self.lookup_peer(peer_id).await?;
        if routes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no DHT routes for peer",
            ));
        }
        let mut last_err: Option<io::Error> = None;
        for addr in routes.direct {
            match self.connect(addr).await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    trace!(?addr, ?e, "direct connect failed, trying next");
                    last_err = Some(e);
                }
            }
        }
        for relay_addr in routes.via_relay {
            match self.connect_relay_peer(relay_addr, peer_id).await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    trace!(?relay_addr, ?e, "via-relay connect failed, trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no DHT routes for peer")
        }))
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
    /// If [`enable_discovery`](Self::enable_discovery) was called before
    /// `serve_as_relay`, the relay publishes each attached peer in
    /// `H_peer_relay(peer_id)` automatically (stopped on disconnect).
    /// Use [`enable_public_relay`](Self::enable_public_relay) separately
    /// to publish the relay itself in the open `H_relays` registry.
    ///
    /// Returns when `shutdown()` is called or accept fails.
    pub async fn serve_as_relay(&self) -> io::Result<()> {
        let clients: Arc<TokioMutex<HashMap<PeerId, ClientHandles>>> = Default::default();
        let discovery = self.discovery().await;
        let transport_port = self.local_addr()?.port();
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
            if let Some(d) = discovery.as_ref() {
                d.announce_peer_via_relay(peer_id, transport_port).await;
            }
            let conn = Arc::new(conn);
            let cancel = self.ctx.cancel_token.child_token();
            let clients_tunnel = clients.clone();
            let clients_control = clients.clone();
            let conn_clone = conn.clone();
            let cancel_tunnel = cancel.clone();
            let discovery_tunnel = discovery.clone();
            tokio::spawn(async move {
                let _g = conn_clone;
                Self::relay_pump(
                    peer_id,
                    tunnel_channel,
                    clients_tunnel,
                    cancel_tunnel,
                    discovery_tunnel,
                    transport_port,
                )
                .await;
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
        discovery: Option<Arc<Discovery>>,
        transport_port: u16,
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
        if let Some(d) = discovery {
            d.stop_announce(
                crate::discovery::h_peer_relay(from_peer_id),
                transport_port,
            )
            .await;
        }
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

/// Discovery-pool top-up + shed loop.
///
/// Each `topup_interval` tick:
/// 1. Snapshot the pool.
/// 2. Shed: cancel + remove every `Discovery` entry that (a) has no active
///    tunnels AND (b) is older than `grace_period`.
/// 3. Top up: if `Discovery` entries remaining < `relay_target`, ask the
///    DHT for relay addresses and spawn pool entries for ones not already
///    in the pool, until target is reached (or DHT runs out).
async fn topup_loop(
    pool: Arc<TokioMutex<HashMap<SocketAddr, Arc<PoolEntry>>>>,
    discovery: Arc<Discovery>,
    config: DiscoveryConfig,
    ctx: NodeCtx,
    cancel: CancellationToken,
    packet_buffer: usize,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.topup_interval) => {}
            _ = cancel.cancelled() => return,
        }

        // -- Phase 1: shed stale Discovery entries -----------------------
        let snapshot: Vec<(SocketAddr, Arc<PoolEntry>)> = {
            pool.lock()
                .await
                .iter()
                .map(|(a, e)| (*a, e.clone()))
                .collect()
        };
        let now = Instant::now();
        let mut to_shed: Vec<SocketAddr> = Vec::new();
        for (addr, entry) in &snapshot {
            if entry.source() != RelaySource::Discovery {
                continue;
            }
            if now.duration_since(entry.added_at) < config.grace_period {
                continue;
            }
            if entry.active_tunnels() > 0 {
                continue;
            }
            to_shed.push(*addr);
        }
        if !to_shed.is_empty() {
            let mut p = pool.lock().await;
            for addr in &to_shed {
                if let Some(entry) = p.remove(addr) {
                    entry.cancel.cancel();
                    trace!(%addr, "topup: shed idle Discovery relay");
                }
            }
        }

        // -- Phase 2: top up to relay_target -----------------------------
        let discovery_count = snapshot
            .iter()
            .filter(|(addr, e)| {
                !to_shed.contains(addr) && e.source() == RelaySource::Discovery
            })
            .count();
        if discovery_count >= config.relay_target {
            continue;
        }
        let need = config.relay_target - discovery_count;
        let candidates = discovery.lookup_relays().await;
        if candidates.is_empty() {
            trace!("topup: DHT returned no relay candidates");
            continue;
        }
        let mut added = 0usize;
        let mut p = pool.lock().await;
        for addr in candidates {
            if added >= need {
                break;
            }
            if p.contains_key(&addr) {
                continue;
            }
            let entry = PoolEntry::new(addr, RelaySource::Discovery);
            pool::ensure_supervisor(&entry, &ctx, packet_buffer);
            p.insert(addr, entry);
            added += 1;
            trace!(%addr, "topup: added Discovery relay");
        }
    }
}
