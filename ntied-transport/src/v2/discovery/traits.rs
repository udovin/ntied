use std::net::SocketAddr;

use async_trait::async_trait;

use crate::v2::crypto::PeerId;

pub struct ConnectionRequest {
    pub peer_addr: SocketAddr,
    pub peer_id: Option<PeerId>,
}

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr>;
    async fn register(&self, peer_id: PeerId, addr: SocketAddr);

    async fn recv_connection_request(&self) -> ConnectionRequest {
        std::future::pending().await
    }
}
