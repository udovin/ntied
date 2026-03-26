use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;

use crate::crypto::PeerId;
use crate::raw::TransportSocket;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteInfo {
    Direct(SocketAddr),
    Relayed { gateway_addr: SocketAddr },
}

pub struct ConnectionRequest {
    pub peer_addr: SocketAddr,
    pub peer_id: Option<PeerId>,
}

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn resolve(&self, peer_id: &PeerId) -> Option<RouteInfo>;
    async fn register(&self, peer_id: PeerId, addr: SocketAddr);

    async fn recv_connection_request(&self) -> ConnectionRequest {
        std::future::pending().await
    }
}

#[async_trait]
pub trait DiscoveryFactory: Send + Sync {
    async fn create(&self, transport: &TransportSocket) -> io::Result<Arc<dyn Discovery>>;
}

#[async_trait]
impl<T: Discovery + 'static> DiscoveryFactory for Arc<T> {
    async fn create(&self, _transport: &TransportSocket) -> io::Result<Arc<dyn Discovery>> {
        Ok(self.clone())
    }
}

#[async_trait]
impl DiscoveryFactory for Arc<dyn Discovery> {
    async fn create(&self, _transport: &TransportSocket) -> io::Result<Arc<dyn Discovery>> {
        Ok(self.clone())
    }
}
