use std::net::SocketAddr;
use std::sync::Arc;

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
pub trait DiscoveryFactory: Send + Sync {
    type Discovery: Discovery;

    fn create(
        &self,
        transport: Arc<TransportInner>,
    ) -> impl Future<Output = Result<Self::Discovery, Error>> + Send;
}

// Represents discovery service.
//
// This service should do the following:
//  - Send to network information about Transport UDP port (public, not local).
//    Service should publish always actual public socket address.
//  - Send to network connection request with specified peer.
//  - Receive from network connection request with specified peer.
pub trait Discovery: Send + Sync {
    // Should send to network notification that we want to connect with peer with specified public_key and source_id of connection.
    //
    // Should return public SocketAddr of peer that we want to connect with.
    fn send_connection_request(
        &self,
        public_key: &PublicKey,
        source_id: u32,
    ) -> impl Future<Output = Result<SocketAddr, Error>> + Send;

    // Should receive from network connection request with specified peer.
    //
    // Should return ConnectionRequest with public SocketAddr of peer that we want to connect with.
    // Also, optional source_id and public_key of peer that we want to connect with that should be used for connection establishment.
    fn recv_connection_request(
        &self,
    ) -> impl Future<Output = Result<ConnectionRequest, Error>> + Send;
}
