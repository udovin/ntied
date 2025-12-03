use std::net::SocketAddr;
use std::sync::Arc;

use ntied_crypto::PublicKey;

use crate::{Error, TransportInner};

pub(crate) struct ConnectionRequest {
    pub public_key: PublicKey,
    pub source_id: u32,
    pub socket_addr: SocketAddr,
}

pub(crate) trait DiscoveryFactory: Send + Sync {
    type Discovery: Discovery;

    fn create(
        &self,
        transport: Arc<TransportInner>,
    ) -> impl Future<Output = Result<Self::Discovery, Error>> + Send;
}

pub(crate) trait Discovery: Send + Sync {
    fn send_connection_request(
        &self,
        public_key: &PublicKey,
        source_id: u32,
    ) -> impl Future<Output = Result<SocketAddr, Error>> + Send;

    fn recv_connection_request(
        &self,
    ) -> impl Future<Output = Result<ConnectionRequest, Error>> + Send;
}
