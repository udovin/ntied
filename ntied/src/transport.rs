use std::io;
use std::net::SocketAddr;

use ntied_transport::{Connection, ConnectionRef, DatagramChannel, Node, PeerId, PrivateKey, PublicKey, StreamChannel};

const PURPOSE_CHAT: u16 = 0x0010;
const PURPOSE_CALL: u16 = 0x0020;

pub struct NtiedTransport {
    inner: Node,
}

impl NtiedTransport {
    pub async fn bind(addr: &str, private_key: PrivateKey) -> io::Result<Self> {
        let bind_addr: SocketAddr = addr
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let inner = Node::bind(bind_addr, private_key).await?;
        Ok(Self { inner })
    }

    /// Attach to a relay server. Can be called multiple times to switch relay.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        self.inner.attach_relay(relay_addr).await
    }

    /// Connect to a peer through the attached relay.
    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<NtiedConnection> {
        let conn = self.inner.connect_peer(peer_id).await?;
        let peer_id = conn.peer_id().await;
        let chat_stream = conn.open_stream(PURPOSE_CHAT).await?;
        Ok(NtiedConnection {
            conn,
            chat_stream,
            peer_id,
            recv_buf: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    /// Accept an incoming peer connection.
    pub async fn accept(&self) -> io::Result<NtiedConnection> {
        let conn = self.inner.accept().await?;
        let peer_id = conn.peer_id().await;
        let (chat_stream, _purpose) = conn.accept_stream().await?;
        Ok(NtiedConnection {
            conn,
            chat_stream,
            peer_id,
            recv_buf: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn peer_id(&self) -> PeerId {
        self.inner.peer_id()
    }

    pub async fn is_relay_attached(&self) -> bool {
        self.inner.is_relay_attached().await
    }
}

pub struct NtiedConnection {
    conn: Connection,
    chat_stream: StreamChannel,
    peer_id: Option<PeerId>,
    recv_buf: tokio::sync::Mutex<Vec<u8>>,
}

impl NtiedConnection {
    /// Send a length-prefixed message (reliable, ordered — for chat and signaling).
    pub async fn send(&self, data: impl Into<Vec<u8>>) -> io::Result<()> {
        let data = data.into();
        let mut framed = Vec::with_capacity(4 + data.len());
        framed.extend_from_slice(&(data.len() as u32).to_be_bytes());
        framed.extend_from_slice(&data);
        self.chat_stream.send(&framed).await
    }

    /// Receive a length-prefixed message (reliable, ordered — for chat and signaling).
    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        let mut buf = self.recv_buf.lock().await;
        loop {
            // Try to extract a complete message
            if buf.len() >= 4 {
                let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
                if buf.len() >= 4 + len {
                    let msg = buf[4..4 + len].to_vec();
                    buf.drain(..4 + len);
                    return Ok(msg);
                }
            }
            // Need more data from stream
            let data = self.chat_stream.recv().await?;
            buf.extend_from_slice(&data);
        }
    }

    pub fn peer_id(&self) -> Option<&PeerId> {
        self.peer_id.as_ref()
    }

    pub async fn peer_public_key(&self) -> Option<PublicKey> {
        self.conn.peer_public_key().await
    }

    pub async fn is_relayed(&self) -> bool {
        self.conn.is_relayed().await
    }

    pub async fn try_direct(&self) -> io::Result<()> {
        self.conn.try_direct().await
    }

    /// Open a call channel (unreliable datagram for audio data).
    pub async fn open_call(&self) -> io::Result<CallChannel> {
        let datagram = self.conn.open_datagram(PURPOSE_CALL).await?;
        Ok(CallChannel { datagram })
    }

    /// Accept an incoming call channel.
    pub async fn accept_call(&self) -> io::Result<CallChannel> {
        let (datagram, _purpose) = self.conn.accept_datagram().await?;
        Ok(CallChannel { datagram })
    }

    /// Get a lightweight reference for accepting call channels concurrently.
    /// This can be called from a background task without blocking the main loop.
    pub fn call_acceptor(&self) -> CallAcceptor {
        CallAcceptor {
            conn_ref: self.conn.weak_ref(),
        }
    }
}

/// Lightweight handle for accepting call channels from a background task.
/// Does not block the main connection loop.
pub struct CallAcceptor {
    conn_ref: ConnectionRef,
}

impl CallAcceptor {
    pub async fn accept(&self) -> io::Result<CallChannel> {
        let (datagram, _purpose) = self.conn_ref.accept_datagram().await?;
        Ok(CallChannel { datagram })
    }
}

/// Unreliable datagram channel for call audio data.
pub struct CallChannel {
    datagram: DatagramChannel,
}

impl CallChannel {
    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        self.datagram.send(data).await
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.datagram.recv().await
    }
}
