use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use ntied_transport::connection::Config;
use ntied_transport::{Channel, Connection, Node, PeerId, PrivateKey, PublicKey, Stream};

pub struct NtiedTransport {
    inner: Node,
}

impl NtiedTransport {
    pub async fn bind(addr: &str, private_key: PrivateKey) -> io::Result<Self> {
        let bind_addr: SocketAddr = addr
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // Raise `channel_buf_size` so software-H.264 IDRs for 1080p
        // video fit into a single channel message. Audio channels keep
        // the tiny per-message footprint; this is only a ceiling.
        let config = Config {
            channel_buf_size: 2 * 1024 * 1024,
            ..Config::default()
        };
        let inner = Node::bind_with_config(bind_addr, private_key, config).await?;
        Ok(Self { inner })
    }

    /// Attach to a relay server. Can be called multiple times to add more.
    pub async fn attach_relay(&self, relay_addr: SocketAddr) -> io::Result<()> {
        self.inner.attach_relay(relay_addr).await
    }

    /// Connect to a peer through any attached relay.
    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<NtiedConnection> {
        let conn = self.inner.connect_peer(*peer_id).await?;
        let peer_id = conn.peer_id();
        let chat_stream = conn.open_stream()?;
        Ok(NtiedConnection {
            conn: Arc::new(conn),
            chat_stream,
            peer_id,
            recv_buf: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    /// Accept an incoming peer connection.
    pub async fn accept(&self) -> io::Result<NtiedConnection> {
        let conn = self.inner.accept().await?;
        let peer_id = conn.peer_id();
        let chat_stream = conn.accept_stream().await?;
        Ok(NtiedConnection {
            conn: Arc::new(conn),
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
    conn: Arc<Connection>,
    chat_stream: Stream,
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
        let mut written = 0;
        while written < framed.len() {
            let n = self.chat_stream.send(&framed[written..]).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "stream closed mid-send",
                ));
            }
            written += n;
        }
        Ok(())
    }

    /// Receive a length-prefixed message (reliable, ordered — for chat and signaling).
    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        let mut buf = self.recv_buf.lock().await;
        let mut chunk = [0u8; 4096];
        loop {
            if buf.len() >= 4 {
                let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
                if buf.len() >= 4 + len {
                    let msg = buf[4..4 + len].to_vec();
                    buf.drain(..4 + len);
                    return Ok(msg);
                }
            }
            let (n, _fin) = self.chat_stream.recv(&mut chunk).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed",
                ));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn peer_id(&self) -> Option<&PeerId> {
        self.peer_id.as_ref()
    }

    pub fn peer_public_key(&self) -> Option<PublicKey> {
        self.conn.peer_public_key()
    }

    pub fn is_relayed(&self) -> bool {
        !self.conn.is_using_direct_path()
    }

    /// Clone of the underlying transport connection — used by background
    /// tasks (e.g. path-status poller) that need to query state without
    /// owning `NtiedConnection`.
    pub fn connection_handle(&self) -> Arc<Connection> {
        self.conn.clone()
    }

    pub async fn try_direct(&self) -> io::Result<()> {
        self.conn.try_direct().await
    }

    /// Open a call channel (unreliable datagram for audio data).
    pub fn open_call(&self) -> io::Result<CallChannel> {
        let channel = self.conn.open_channel()?;
        Ok(CallChannel { channel })
    }

    /// Accept an incoming call channel.
    pub async fn accept_call(&self) -> io::Result<CallChannel> {
        let channel = self.conn.accept_channel().await?;
        Ok(CallChannel { channel })
    }

    /// Open a call video channel (unreliable datagram for video frames).
    pub fn open_call_video(&self) -> io::Result<CallVideoChannel> {
        let channel = self.conn.open_channel()?;
        Ok(CallVideoChannel { channel })
    }

    /// Accept an incoming call video channel.
    pub async fn accept_call_video(&self) -> io::Result<CallVideoChannel> {
        let channel = self.conn.accept_channel().await?;
        Ok(CallVideoChannel { channel })
    }

    /// Get a lightweight reference for accepting call channels concurrently.
    /// Holds an `Arc<Connection>`; multiple acceptors can coexist.
    pub fn call_acceptor(&self) -> CallAcceptor {
        CallAcceptor {
            conn: self.conn.clone(),
        }
    }
}

/// Lightweight handle for accepting call channels from a background task.
pub struct CallAcceptor {
    conn: Arc<Connection>,
}

impl CallAcceptor {
    pub async fn accept(&self) -> io::Result<CallChannel> {
        let channel = self.conn.accept_channel().await?;
        Ok(CallChannel { channel })
    }

    pub async fn accept_video(&self) -> io::Result<CallVideoChannel> {
        let channel = self.conn.accept_channel().await?;
        Ok(CallVideoChannel { channel })
    }
}

/// Unreliable datagram channel for call audio data.
pub struct CallChannel {
    channel: Channel,
}

impl CallChannel {
    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        self.channel.send(data.to_vec()).await
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.channel.recv().await
    }
}

/// Unreliable datagram channel for call video frames. Separate from
/// `CallChannel` so audio and video have independent back-pressure and
/// eviction policies.
pub struct CallVideoChannel {
    channel: Channel,
}

impl CallVideoChannel {
    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        self.channel.send(data.to_vec()).await
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.channel.recv().await
    }
}
