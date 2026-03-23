use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use ntied_transport::v2::api::{Connection, DatagramStream, Transport};
use ntied_transport::v2::crypto::{PeerId, PrivateKey, PublicKey};
use ntied_transport::v2::discovery::ServerDiscovery;

const DEFAULT_STREAM_PURPOSE: u16 = 0x0001;

pub struct NtiedTransport {
    inner: Transport,
}

impl NtiedTransport {
    pub async fn bind(
        addr: &str,
        private_key: PrivateKey,
        server_addr: SocketAddr,
    ) -> io::Result<Self> {
        let discovery = Arc::new(ServerDiscovery::connect(server_addr).await?);
        let bind_addr: SocketAddr = addr
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let inner = Transport::bind(bind_addr, private_key, discovery).await?;
        Ok(Self { inner })
    }

    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<NtiedConnection> {
        let conn = self.inner.connect(peer_id).await?;
        let peer_id = conn.peer_id().await;
        let stream = conn.open_datagram_stream(DEFAULT_STREAM_PURPOSE).await?;
        Ok(NtiedConnection {
            conn,
            stream,
            peer_id,
        })
    }

    pub async fn accept(&self) -> io::Result<NtiedConnection> {
        let conn = self.inner.accept().await?;
        let peer_id = conn.peer_id().await;
        let (stream, _purpose) = conn.accept_datagram_stream().await?;
        Ok(NtiedConnection {
            conn,
            stream,
            peer_id,
        })
    }
}

pub struct NtiedConnection {
    conn: Connection,
    stream: DatagramStream,
    peer_id: Option<PeerId>,
}

impl NtiedConnection {
    pub async fn send(&self, data: impl Into<Vec<u8>>) -> io::Result<()> {
        self.stream.send(&data.into()).await
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.stream.recv().await
    }

    pub fn peer_id(&self) -> Option<&PeerId> {
        self.peer_id.as_ref()
    }

    pub async fn peer_public_key(&self) -> Option<PublicKey> {
        self.conn.peer_public_key().await
    }
}
