use std::io;
use std::net::SocketAddr;

use ntied_transport::node::Node;
use ntied_transport::{PeerId, PrivateKey};

/// Relay server — accepts client connections and forwards multiplexed
/// tunnel + control messages between peers.
pub struct RelayNode {
    node: Node,
}

impl RelayNode {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        Ok(Self {
            node: Node::bind(addr, identity).await?,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.node.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.node.peer_id()
    }

    /// Run until shutdown or accept failure.
    pub async fn run(&self) -> io::Result<()> {
        self.node.serve_as_relay().await
    }
}
