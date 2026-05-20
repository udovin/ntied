//! Relay-server implementation.
//!
//! [`RelayNode`] wraps a [`Node`] and adds the relay-server accept loop:
//! - Accepts incoming client connections via [`Node::accept`].
//! - Accepts each client's first two channels as `tunnel_channel` and
//!   `control_channel`.
//! - Forwards tunnel messages between attached peers (rewriting
//!   `[dest|payload]` -> `[src|payload]`).
//! - Relays hole-punch control signalling
//!   ([`ControlMsg::HolePunchRequest`] -> bidirectional
//!   [`ControlMsg::HolePunchNotify`]).
//! - If DHT discovery is enabled on the underlying `Node`, publishes each
//!   attached peer in `H_peer_relay(peer_id)` automatically.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use crate::crypto::{PEER_ID_SIZE, PeerId, PrivateKey};
use crate::discovery::Discovery;
use crate::node::{Channel, DiscoveryConfig, Node};

use super::{ControlMsg, TUNNEL_HEADER_SIZE};

/// Per-client state on the relay-server side.
#[derive(Clone)]
struct ClientHandles {
    tunnel_channel: Arc<Channel>,
    control_channel: Arc<Channel>,
    addr: SocketAddr,
}

/// Relay server -- accepts client connections and forwards multiplexed
/// tunnel + control messages between peers.
///
/// Discovery is enabled at bind time so [`run`](Self::run) can publish
/// attached peers in `H_peer_relay(peer_id)`.  Default config uses the
/// real mainline DHT; tests can override via
/// [`bind_with_discovery`](Self::bind_with_discovery) to point at a local
/// `mainline::Testnet`.
pub struct RelayNode {
    node: Node,
}

impl RelayNode {
    /// Bind a relay with the default DHT discovery configuration (real
    /// mainline network).
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        Self::bind_with_discovery(addr, identity, DiscoveryConfig::default()).await
    }

    /// Bind a relay with an explicit DHT discovery configuration.  Used by
    /// tests to bootstrap against a local `mainline::Testnet`.
    pub async fn bind_with_discovery(
        addr: SocketAddr,
        identity: PrivateKey,
        discovery_config: DiscoveryConfig,
    ) -> io::Result<Self> {
        let node = Node::bind(addr, identity).await?;
        node.enable_discovery(discovery_config).await?;
        Ok(Self { node })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.node.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.node.peer_id()
    }

    /// Also publish this relay in the global `H_relays` registry.  Optional;
    /// call after `bind_with_discovery` and before `run` for relays that
    /// should be discoverable by fresh peers.
    pub async fn enable_public_relay(&self) -> io::Result<()> {
        self.node.enable_public_relay().await
    }

    /// Borrow the underlying [`Node`] -- useful for tests that need to peek
    /// at low-level state.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// Run the relay accept loop until shutdown or accept fails.
    ///
    /// For each accepted client connection, the relay accepts the client's
    /// first two channels as the multiplex `tunnel_channel` and the
    /// `control_channel`.  Tunnel messages are forwarded between clients
    /// (rewriting `[dest|payload]` -> `[src|payload]`); control messages
    /// ([`ControlMsg::HolePunchRequest`]) trigger bidirectional
    /// [`ControlMsg::HolePunchNotify`] to the requester and target.
    ///
    /// If discovery was enabled at bind time, each attached peer is
    /// published in `H_peer_relay(peer_id)` automatically (stopped on
    /// disconnect).  Use [`enable_public_relay`](Self::enable_public_relay)
    /// separately to publish the relay itself in `H_relays`.
    pub async fn run(&self) -> io::Result<()> {
        let clients: Arc<TokioMutex<HashMap<PeerId, ClientHandles>>> = Default::default();
        let discovery = self.node.discovery().await;
        let transport_port = self.node.local_addr()?.port();
        loop {
            let conn = self.node.accept().await?;
            let peer_id = match conn.peer_id() {
                Some(p) => p,
                None => {
                    tracing::warn!("relay: accepted connection without peer_id");
                    continue;
                }
            };
            let client_addr = match conn.remote_addr() {
                Some(a) => a,
                None => {
                    tracing::warn!(
                        peer = %peer_id.short(),
                        "relay: accepted connection without remote_addr",
                    );
                    continue;
                }
            };
            let tunnel_channel = match conn.accept_channel().await {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    tracing::warn!(
                        peer = %peer_id.short(),
                        ?err,
                        "relay: failed to accept tunnel channel",
                    );
                    continue;
                }
            };
            let control_channel = match conn.accept_channel().await {
                Ok(c) => Arc::new(c),
                Err(err) => {
                    tracing::warn!(
                        peer = %peer_id.short(),
                        ?err,
                        "relay: failed to accept control channel",
                    );
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
            tracing::info!(
                peer = %peer_id.short(),
                from = %client_addr,
                "relay: client attached",
            );
            if let Some(d) = discovery.as_ref() {
                d.announce_peer_via_relay(peer_id, transport_port).await;
            }
            let conn = Arc::new(conn);
            let cancel = self.node.child_cancel_token();
            let clients_tunnel = clients.clone();
            let clients_control = clients.clone();
            let conn_clone = conn.clone();
            let cancel_tunnel = cancel.clone();
            let discovery_tunnel = discovery.clone();
            tokio::spawn(async move {
                let _g = conn_clone;
                pump_tunnel(
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
                pump_control(peer_id, control_channel, clients_control, cancel).await;
            });
        }
    }
}

async fn pump_tunnel(
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
            tracing::trace!(
                from = %from_peer_id.short(),
                to = %to_peer.short(),
                "relay: dest not connected, dropping",
            );
            continue;
        };

        let mut out = Vec::with_capacity(msg.len());
        out.extend_from_slice(from_peer_id.as_bytes());
        out.extend_from_slice(&msg[TUNNEL_HEADER_SIZE..]);
        if let Err(err) = to_channel.send(out).await {
            tracing::trace!(
                from = %from_peer_id.short(),
                to = %to_peer.short(),
                ?err,
                "relay: forward send failed",
            );
        }
    }
    clients.lock().await.remove(&from_peer_id);
    if let Some(d) = discovery {
        d.stop_announce(crate::discovery::h_peer_relay(from_peer_id), transport_port)
            .await;
    }
    tracing::info!(peer = %from_peer_id.short(), "relay: client detached");
}

async fn pump_control(
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
            tracing::warn!(
                peer = %from_peer_id.short(),
                "relay: control msg decode failed",
            );
            continue;
        };
        match parsed {
            ControlMsg::HolePunchRequest { target } => {
                let map = clients.lock().await;
                let target_handles = map.get(&target).cloned();
                let from_handles = map.get(&from_peer_id).cloned();
                drop(map);
                let (Some(t), Some(f)) = (target_handles, from_handles) else {
                    tracing::debug!(
                        from = %from_peer_id.short(),
                        target = %target.short(),
                        "relay: holepunch endpoint missing",
                    );
                    continue;
                };
                tracing::debug!(
                    from = %from_peer_id.short(),
                    target = %target.short(),
                    "relay: holepunch_request relaying notifies",
                );
                let notify_to_target = ControlMsg::HolePunchNotify {
                    from: from_peer_id,
                    addr: f.addr,
                }
                .encode();
                if let Err(err) = t.control_channel.send(notify_to_target).await {
                    tracing::trace!(
                        target = %target.short(),
                        ?err,
                        "relay: notify_to_target failed",
                    );
                }
                let notify_to_requester = ControlMsg::HolePunchNotify {
                    from: target,
                    addr: t.addr,
                }
                .encode();
                if let Err(err) = f.control_channel.send(notify_to_requester).await {
                    tracing::trace!(
                        from = %from_peer_id.short(),
                        ?err,
                        "relay: notify_to_requester failed",
                    );
                }
            }
            ControlMsg::HolePunchNotify { .. } => {
                tracing::warn!(
                    peer = %from_peer_id.short(),
                    "relay: unexpected HolePunchNotify",
                );
            }
        }
    }
}
