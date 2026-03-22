use std::net::SocketAddr;

use async_trait::async_trait;

use crate::v2::crypto::PeerId;

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr>;
    async fn register(&self, peer_id: PeerId, addr: SocketAddr);
}
