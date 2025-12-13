use std::collections::hash_map;
use std::net::SocketAddr;
use std::sync::Arc;

pub use tokio::sync::Mutex as TokioMutex;
pub use tokio::sync::mpsc;

use crate::Error;
use crate::TransportInner;

pub struct RawConnection {
    transport: Arc<TransportInner>,
    addr: SocketAddr,
    rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

impl RawConnection {
    const MAX_PACKETS: usize = 4;

    pub(crate) fn new(transport: Arc<TransportInner>, addr: SocketAddr) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel(Self::MAX_PACKETS);
        let mut raw_connections = transport.raw_connections.write().unwrap();
        match raw_connections.entry(addr) {
            hash_map::Entry::Occupied(_) => return Err("Address already connected".into()),
            hash_map::Entry::Vacant(entry) => {
                entry.insert(tx);
                drop(raw_connections);
                Ok(Self {
                    transport,
                    addr,
                    rx: TokioMutex::new(rx),
                })
            }
        }
    }

    pub async fn send(&self, packet: Vec<u8>) -> Result<(), Error> {
        self.transport
            .socket
            .send_to(packet.as_slice(), self.addr)
            .await?;
        Ok(())
    }

    pub async fn recv(&self) -> Result<Vec<u8>, Error> {
        Ok(self
            .rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Connection closed")?)
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        self.transport
            .raw_connections
            .write()
            .unwrap()
            .remove(&self.addr);
    }
}
