use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::wire::packet::{PacketHeader, parse_init, peek_header};
use crate::crypto::{PEER_ID_SIZE, PeerId};

use super::channel::Channel;
use super::connection::{Connection, OwnedConnectionId, RawPacket};
use super::control::ControlMsg;
use super::node::NodeCtx;
use super::transport::Transport;

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

/// Wire header for every multiplexed tunnel message:
/// `[other_end_peer_id (PEER_ID_SIZE)] [inner packet]`.
///
/// Outbound (us → relay): `other_end_peer_id` = destination peer.
/// Inbound  (relay → us): `other_end_peer_id` = source peer (relay rewrote it).
pub(crate) const TUNNEL_HEADER_SIZE: usize = PEER_ID_SIZE;

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
    pump_task: Mutex<Option<JoinHandle<()>>>,
    control_task: Mutex<Option<JoinHandle<()>>>,
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
                    trace!(?e, "control channel recv error, exiting");
                    return;
                }
            };
            let Some(parsed) = ControlMsg::decode(&msg) else {
                warn!(len = msg.len(), "control msg decode failed");
                continue;
            };
            match parsed {
                ControlMsg::HolePunchNotify { from, addr } => {
                    trace!(?from, %addr, "control: HolePunchNotify");
                    self.pending_holepunch.lock().unwrap().insert(from, addr);
                }
                ControlMsg::HolePunchRequest { .. } => {
                    // Clients only consume Notify; Request is for the relay.
                    warn!("control: client received HolePunchRequest, ignoring");
                }
            }
        }
    }

    /// Build a Tunnel transport targeting `peer_id` and return the per-peer
    /// inbound `(rx, tx)` mpsc. The caller MUST register `tx` in
    /// `Node::connection_map` (via `OwnedConnectionId::tunneled`) so that
    /// the pump can dispatch by connection_id.
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
        let transport = Arc::new(Transport::Tunnel {
            relay: self.clone(),
            peer_id,
        });
        (transport, rx, tx)
    }

    async fn pump_loop(self: Arc<Self>, ctx: NodeCtx) {
        loop {
            let msg = tokio::select! {
                msg = self.tunnel_channel.recv() => msg,
                _ = self.cancel_token.cancelled() => return,
                _ = ctx.cancel_token.cancelled() => return,
            };
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    trace!(?e, "tunnel channel recv error, pump exiting");
                    return;
                }
            };
            if msg.len() < TUNNEL_HEADER_SIZE {
                warn!(len = msg.len(), "tunnel msg too small, dropping");
                continue;
            }
            let mut peer_bytes = [0u8; PEER_ID_SIZE];
            peer_bytes.copy_from_slice(&msg[..PEER_ID_SIZE]);
            let from_peer = PeerId::from_bytes(peer_bytes);
            let payload = &msg[TUNNEL_HEADER_SIZE..];

            let header = match peek_header(payload) {
                Ok(h) => h,
                Err(_) => {
                    trace!("tunnel: failed to peek header");
                    continue;
                }
            };

            // Init → spawn accept-side connection (collisions OK: each gets
            // its own connection_id).
            if matches!(header, PacketHeader::Init { .. }) {
                let init = match parse_init(payload) {
                    Ok(i) => i,
                    Err(_) => {
                        warn!(?from_peer, "failed to parse Init");
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
                let transport = Arc::new(Transport::Tunnel {
                    relay: self.clone(),
                    peer_id: from_peer,
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
                ));
                continue;
            }

            // Non-Init: route by receiver connection_id via connection_map.
            let Some(dest_id) = header_dest_connection_id(&header) else {
                continue;
            };
            let tx = ctx.connection_map.read().unwrap().get(&dest_id).cloned();
            let Some(tx) = tx else {
                trace!(dest_id, "tunnel: dest connection not found");
                continue;
            };
            if tx
                .try_send(RawPacket {
                    data: payload.to_vec(),
                    addr: self.addr,
                })
                .is_err()
            {
                trace!(dest_id, "peer rx queue full, dropping tunnel msg");
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
