use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ntied_crypto::PublicKey;

use crate::{Error, TransportInner};

// Represents incoming connection request from peer.
//
// This request then should be used by transport to establish connection with incoming peer.
// This request is required due to the fact that transport should proceed NAT hole punching.
pub(crate) struct ConnectionRequest {
    pub socket_addr: SocketAddr,
    // Can be None if we can not get public_key of incoming connection request.
    // This can happen when Discovery implementation does not support sending public key notifications.
    pub public_key: Option<PublicKey>,
    // Can be None if we can not get source_id of incoming connection request.
    // This can happen when Discovery implementation does not support sending source_id notifications.
    pub source_id: Option<u32>,
}

// Represents factory for creating discovery service.
#[async_trait]
pub trait DiscoveryFactory: Send + Sync {
    async fn create(&self, transport: Arc<TransportInner>) -> Result<Arc<dyn Discovery>, Error>;
}

// Represents discovery service.
//
// This service should do the following:
//  - Send to network information about Transport UDP port (public, not local).
//    Service should publish always actual public socket address.
//  - Send to network connection request with specified peer.
//  - Receive from network connection request with specified peer.
#[async_trait]
pub trait Discovery: Send + Sync {
    // Should send to network notification that we want to connect with peer with specified public_key and source_id of connection.
    //
    // Should return public SocketAddr of peer that we want to connect with.
    async fn send_connection_request(
        &self,
        public_key: &PublicKey,
        source_id: u32,
    ) -> Result<SocketAddr, Error>;

    // Should receive from network connection request with specified peer.
    //
    // Should return ConnectionRequest with public SocketAddr of peer that we want to connect with.
    // Also, optional source_id and public_key of peer that we want to connect with that should be used for connection establishment.
    async fn recv_connection_request(&self) -> Result<ConnectionRequest, Error>;
}
