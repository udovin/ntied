use std::io;
use std::net::SocketAddr;

use ntied_transport::node::{DiscoveryConfig, Node};
use ntied_transport::{PeerId, PrivateKey};

/// Relay server — accepts client connections and forwards multiplexed
/// tunnel + control messages between peers.
///
/// Discovery is enabled at bind time so `serve_as_relay` can publish
/// attached peers in `H_peer_relay(peer_id)`.  Default config uses the
/// real mainline DHT; tests can override via [`bind_with_discovery`] to
/// point at a local `mainline::Testnet`.
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

    /// Run until shutdown or accept failure.
    pub async fn run(&self) -> io::Result<()> {
        self.node.serve_as_relay().await
    }
}
