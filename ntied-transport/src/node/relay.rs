use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::crypto::{PEER_ID_SIZE, PeerId};
use crate::wire::packet::{PacketHeader, parse_init, peek_header};

use super::channel::Channel;
use super::connection::{Connection, OwnedConnectionId, RawPacket};
use super::node::NodeCtx;
use super::transport::Transport;
use crate::relay::{ControlMsg, TUNNEL_HEADER_SIZE};

const ACCEPT_TUNNEL_BUFFER: usize = 64;

/// Receiver-side connection id from any non-Init packet header.
/// `Init` is handled separately (it spawns a new accept-side connection
/// rather than dispatching to an existing one).
fn header_dest_connection_id(h: &PacketHeader) -> Option<u64> {
    match *h {
        PacketHeader::Init { .. } => None,
        PacketHeader::InitAck {
            initiator_connection_id,
            ..
        } => Some(initiator_connection_id),
        PacketHeader::Data {
            receiver_connection_id,
            ..
        } => Some(receiver_connection_id),
    }
}

/// One connection to a relay, multiplexing tunnels to many peers
/// through a single `tunnel_channel`. Inbound dispatch is done by a
/// pump task that peeks the inner packet header for the receiver's
/// connection_id and forwards via `NodeCtx.connection_map`. New
/// incoming connections (`Init` packets) spawn `accept_tunneled`.
///
/// A second `control_channel` carries hole-punch signaling. Direct
/// addresses learned via `HolePunchNotify` are stashed in
/// `pending_holepunch` and consumed on demand by the per-peer
/// `Connection::main_loop`.
pub(crate) struct RelayConnection {
    pub(crate) addr: SocketAddr,
    /// Connection to the relay. Held to anchor its lifetime — when this
    /// `RelayConnection` drops, the connection drops, which cancels the
    /// inner main task and closes both channels.
    _conn: Arc<Connection>,
    pub(crate) tunnel_channel: Arc<Channel>,
    pub(crate) control_channel: Arc<Channel>,
    /// `peer_id → SocketAddr` learned via `HolePunchNotify`. Take-once.
    pending_holepunch: Mutex<HashMap<PeerId, SocketAddr>>,
    cancel_token: CancellationToken,
    /// Fired by `pump_loop` (or explicit drop) when this relay's underlying
    /// transport is no longer usable.  Pool supervisors await this to detect
    /// disconnects and trigger reconnect or shed.
    pub(crate) closed: CancellationToken,
    /// Number of live tunnels (outbound `open_tunnel` + accepted-inbound via
    /// `pump_loop` Init dispatch).  Incremented at tunnel creation, decremented
    /// when the corresponding `Transport::Tunnel` is dropped via [`TunnelGuard`].
    /// Used by the discovery-pool shed logic.
    pub(crate) active_tunnels: Arc<AtomicUsize>,
    pump_task: Mutex<Option<JoinHandle<()>>>,
    control_task: Mutex<Option<JoinHandle<()>>>,
}

/// RAII handle decrementing `RelayConnection::active_tunnels` on drop.
/// Embedded in `Transport::Tunnel` so the count tracks tunnel lifetime.
pub(crate) struct TunnelGuard {
    counter: Arc<AtomicUsize>,
}

impl TunnelGuard {
    pub(crate) fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

impl RelayConnection {
    pub(crate) fn new(
        addr: SocketAddr,
        conn: Connection,
        tunnel_channel: Channel,
        control_channel: Channel,
        ctx: NodeCtx,
    ) -> Arc<Self> {
        let conn = Arc::new(conn);
        let tunnel_channel = Arc::new(tunnel_channel);
        let control_channel = Arc::new(control_channel);
        let cancel_token = CancellationToken::new();

        let relay = Arc::new(Self {
            addr,
            _conn: conn,
            tunnel_channel,
            control_channel,
            pending_holepunch: Mutex::new(HashMap::new()),
            cancel_token,
            closed: CancellationToken::new(),
            active_tunnels: Arc::new(AtomicUsize::new(0)),
            pump_task: Mutex::new(None),
            control_task: Mutex::new(None),
        });

        let pump = tokio::spawn(Self::pump_loop(relay.clone(), ctx.clone()));
        *relay.pump_task.lock().unwrap() = Some(pump);
        let control = tokio::spawn(Self::control_loop(relay.clone(), ctx));
        *relay.control_task.lock().unwrap() = Some(control);
        relay
    }

    /// Send a `HolePunchRequest` to the relay so it forwards our external
    /// address to `target` and `target`'s external address back to us.
    pub(crate) async fn send_holepunch_request(&self, target: PeerId) -> tokio::io::Result<()> {
        self.control_channel
            .send(ControlMsg::HolePunchRequest { target }.encode())
            .await
    }

    /// Pop the direct address learned for `peer` via `HolePunchNotify`,
    /// if any. Consumed on read.
    pub(crate) fn take_pending_holepunch(&self, peer: &PeerId) -> Option<SocketAddr> {
        self.pending_holepunch.lock().unwrap().remove(peer)
    }

    async fn control_loop(self: Arc<Self>, ctx: NodeCtx) {
        loop {
            let msg = tokio::select! {
                msg = self.control_channel.recv() => msg,
                _ = self.cancel_token.cancelled() => return,
                _ = ctx.cancel_token.cancelled() => return,
            };
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(relay = %self.addr, ?e, "control channel closed, exiting");
                    return;
                }
            };
            let Some(parsed) = ControlMsg::decode(&msg) else {
                tracing::warn!(relay = %self.addr, len = msg.len(), "control msg decode failed");
                continue;
            };
            match parsed {
                ControlMsg::HolePunchNotify { from, addr } => {
                    tracing::debug!(
                        relay = %self.addr,
                        peer = %from.short(),
                        %addr,
                        "control: holepunch_notify",
                    );
                    self.pending_holepunch.lock().unwrap().insert(from, addr);
                }
                ControlMsg::HolePunchRequest { .. } => {
                    // Clients only consume Notify; Request is for the relay.
                    tracing::warn!(relay = %self.addr, "control: client received HolePunchRequest");
                }
            }
        }
    }

    /// Build a Tunnel transport targeting `peer_id` and return the per-peer
    /// inbound `(rx, tx)` mpsc. The caller MUST register `tx` in
    /// `Node::connection_map` (via `OwnedConnectionId::tunneled`) so that
    /// the pump can dispatch by connection_id.
    ///
    /// Tracks a tunnel slot via [`TunnelGuard`] inside `Transport::Tunnel`,
    /// auto-decremented when the Connection holding the Transport is dropped.
    pub(crate) fn open_tunnel(
        self: &Arc<Self>,
        peer_id: PeerId,
        inbound_buffer: usize,
    ) -> (
        Arc<Transport>,
        mpsc::Receiver<RawPacket>,
        mpsc::Sender<RawPacket>,
    ) {
        let (tx, rx) = mpsc::channel(inbound_buffer);
        let guard = TunnelGuard::new(self.active_tunnels.clone());
        let transport = Arc::new(Transport::Tunnel {
            relay: self.clone(),
            peer_id,
            _guard: guard,
        });
        (transport, rx, tx)
    }

    async fn pump_loop(self: Arc<Self>, ctx: NodeCtx) {
        struct ClosedOnExit(CancellationToken);
        impl Drop for ClosedOnExit {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        // Whatever causes pump_loop to exit (channel error, cancel, panic),
        // signal `closed` so the pool supervisor can react.
        let _closed_on_exit = ClosedOnExit(self.closed.clone());

        loop {
            let msg = tokio::select! {
                msg = self.tunnel_channel.recv() => msg,
                _ = self.cancel_token.cancelled() => return,
                _ = ctx.cancel_token.cancelled() => return,
            };
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(relay = %self.addr, ?e, "tunnel channel closed, pump exiting");
                    return;
                }
            };
            if msg.len() < TUNNEL_HEADER_SIZE {
                tracing::warn!(relay = %self.addr, len = msg.len(), "tunnel msg too small, dropping");
                continue;
            }
            let mut peer_bytes = [0u8; PEER_ID_SIZE];
            peer_bytes.copy_from_slice(&msg[..PEER_ID_SIZE]);
            let from_peer = PeerId::from_bytes(peer_bytes);
            let payload = &msg[TUNNEL_HEADER_SIZE..];

            let header = match peek_header(payload) {
                Ok(h) => h,
                Err(_) => {
                    tracing::trace!(relay = %self.addr, "tunnel: failed to peek header");
                    continue;
                }
            };

            // Init -> spawn accept-side connection (collisions OK: each gets
            // its own connection_id).
            if matches!(header, PacketHeader::Init { .. }) {
                let init = match parse_init(payload) {
                    Ok(i) => i,
                    Err(_) => {
                        tracing::warn!(
                            relay = %self.addr,
                            peer = %from_peer.short(),
                            "failed to parse tunneled Init",
                        );
                        continue;
                    }
                };
                let (tx, rx) = mpsc::channel(ACCEPT_TUNNEL_BUFFER);
                let local_id = ctx.next_connection_id.fetch_add(1, Ordering::Relaxed);
                let owned = OwnedConnectionId::tunneled(
                    local_id,
                    self.clone(),
                    ctx.connection_map.clone(),
                    tx,
                );
                let guard = TunnelGuard::new(self.active_tunnels.clone());
                let transport = Arc::new(Transport::Tunnel {
                    relay: self.clone(),
                    peer_id: from_peer,
                    _guard: guard,
                });
                let conn_cancel = ctx.cancel_token.child_token();
                tokio::spawn(Connection::accept_tunneled(
                    local_id,
                    init.initiator_connection_id,
                    init.kem_public_key,
                    owned,
                    transport,
                    self.addr,
                    ctx.socket.clone(),
                    (*ctx.identity).clone(),
                    rx,
                    ctx.accept_tx.clone(),
                    conn_cancel,
                    ctx.config.clone(),
                ));
                continue;
            }

            // Non-Init: route by receiver connection_id via connection_map.
            let Some(dest_id) = header_dest_connection_id(&header) else {
                continue;
            };
            let tx = ctx.connection_map.read().unwrap().get(&dest_id).cloned();
            let Some(tx) = tx else {
                tracing::trace!(relay = %self.addr, cid = dest_id, "tunnel: dest connection not found");
                continue;
            };
            if tx
                .try_send(RawPacket {
                    data: payload.to_vec(),
                    addr: self.addr,
                })
                .is_err()
            {
                tracing::trace!(relay = %self.addr, cid = dest_id, "peer rx queue full, dropping tunnel msg");
            }
        }
    }
}

impl Drop for RelayConnection {
    fn drop(&mut self) {
        self.cancel_token.cancel();
        if let Some(task) = self.pump_task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(task) = self.control_task.lock().unwrap().take() {
            task.abort();
        }
    }
}
