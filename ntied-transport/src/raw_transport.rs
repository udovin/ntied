use std::net::SocketAddr;
use std::sync::Arc;

use ntied_crypto::PrivateKey;

use crate::{Error, RawConnection, TransportInner};

#[derive(Clone)]
pub struct RawTransport(Arc<TransportInner>);

impl RawTransport {
    pub(crate) fn new(transport: Arc<TransportInner>) -> Self {
        Self(transport)
    }

    pub fn private_key(&self) -> &PrivateKey {
        &self.0.private_key
    }

    // Creates exclusive connection to the given address.
    pub fn connect(&self, addr: SocketAddr) -> Result<RawConnection, Error> {
        RawConnection::new(self.0.clone(), addr)
    }
}
