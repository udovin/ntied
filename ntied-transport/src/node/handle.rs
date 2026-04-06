use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::channel::ChannelError;
use crate::crypto::{PeerId, PublicKey};

use super::state::{SendPath, Shared};
use super::transport::{flush_connection, flush_connection_direct, send_packets_with_probe};

pub struct Connection {
    pub(crate) shared: Arc<Shared>,
    pub(crate) connection_id: u64,
    pub(crate) closed: AtomicBool,
}

impl Connection {
    /// Create a lightweight reference that can call accept_stream/accept_datagram
    /// without owning the connection (no Drop/close behavior).
    pub fn weak_ref(&self) -> ConnectionRef {
        ConnectionRef {
            shared: self.shared.clone(),
            connection_id: self.connection_id,
        }
    }
}

/// Lightweight reference to a connection. Does not close on drop.
/// Can be used to accept streams/datagrams concurrently.
pub struct ConnectionRef {
    shared: Arc<Shared>,
    connection_id: u64,
}

impl ConnectionRef {
    pub async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)> {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap();
                if let Some(entry) = state.connections.get_mut(&self.connection_id) {
                    if let Some((channel_id, purpose)) = entry.conn.accept_datagram() {
                        return Ok((
                            DatagramChannel {
                                shared: self.shared.clone(),
                                connection_id: self.connection_id,
                                channel_id,
                            },
                            purpose,
                        ));
                    }
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection gone",
                    ));
                }
            }
            self.shared.stream_notify.notified().await;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.shared
                .pending_close
                .lock()
                .unwrap()
                .push(self.connection_id);
        }
    }
}

impl Connection {
    pub fn connection_id(&self) -> u64 {
        self.connection_id
    }

    pub async fn peer_public_key(&self) -> Option<PublicKey> {
        let state = self.shared.state.lock().unwrap();
        state
            .connections
            .get(&self.connection_id)
            .and_then(|e| e.peer_public_key().cloned())
    }

    pub async fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().await.map(|pk| pk.peer_id())
    }

    pub async fn remote_addr(&self) -> Option<SocketAddr> {
        let state = self.shared.state.lock().unwrap();
        state
            .connections
            .get(&self.connection_id)
            .and_then(|e| e.addr())
    }

    pub async fn is_established(&self) -> bool {
        let state = self.shared.state.lock().unwrap();
        state
            .connections
            .get(&self.connection_id)
            .map_or(false, |e| e.is_established() && !e.closed)
    }

    /// Returns true if this connection is currently using a relay path.
    pub async fn is_relayed(&self) -> bool {
        let state = self.shared.state.lock().unwrap();
        state
            .connections
            .get(&self.connection_id)
            .map_or(true, |e| matches!(e.send_path, SendPath::Relayed { .. }))
    }

    /// Initiate direct path migration via hole punching. Sends a
    /// `HolePunchRequest` to the relay, which notifies the remote peer of our
    /// addresses. Both sides then begin sending probing packets directly; when
    /// a direct packet is received the connection automatically switches to
    /// the direct path.
    pub async fn try_direct(&self) -> io::Result<()> {
        use crate::relay::protocol::RelayMessage;

        let target_peer_id = {
            let state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            match &entry.send_path {
                SendPath::Direct { .. } => return Ok(()),
                SendPath::Relayed { peer_id } => *peer_id,
            }
        };

        let relay = self.shared.relay.lock().await;
        let relay_state = relay
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no relay attached"))?;
        let msg = RelayMessage::HolePunchRequest {
            target: target_peer_id,
        };
        let relay_conn_id = relay_state.datagram.connection_id;
        let relay_ch_id = relay_state.datagram.channel_id;
        drop(relay);

        {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state.connections.get_mut(&relay_conn_id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "relay connection gone")
            })?;
            entry
                .conn
                .write_datagram(relay_ch_id, &msg.encode())
                .map_err(channel_err_to_io)?;
        }

        flush_connection_direct(&self.shared, relay_conn_id).await?;
        tracing::debug!(connection_id = self.connection_id, %target_peer_id, "try_direct: hole punch request sent");
        Ok(())
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let close_data = {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(entry) = state.connections.get_mut(&self.connection_id) {
                if !entry.closed {
                    entry.closed = true;
                    entry.queue_connection_close(0);
                    let packets = entry.poll_packets(Instant::now());
                    let send_path = entry.send_path.clone();
                    let direct_addr = entry.direct_addr;
                    Some((send_path, direct_addr, packets))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((send_path, direct_addr, packets)) = close_data {
            send_packets_with_probe(&self.shared, &send_path, direct_addr, &packets).await;
        }
        Ok(())
    }

    pub async fn open_stream(&self, purpose: u16) -> io::Result<StreamChannel> {
        let channel_id = {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_stream(purpose)
        };
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(StreamChannel {
            shared: self.shared.clone(),
            connection_id: self.connection_id,
            channel_id,
        })
    }

    pub async fn accept_stream(&self) -> io::Result<(StreamChannel, u16)> {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap();
                if let Some(entry) = state.connections.get_mut(&self.connection_id) {
                    if let Some((channel_id, purpose)) = entry.conn.accept_stream() {
                        return Ok((
                            StreamChannel {
                                shared: self.shared.clone(),
                                connection_id: self.connection_id,
                                channel_id,
                            },
                            purpose,
                        ));
                    }
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection gone",
                    ));
                }
            }
            self.shared.stream_notify.notified().await;
        }
    }

    pub async fn open_datagram(&self, purpose: u16) -> io::Result<DatagramChannel> {
        let channel_id = {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry.conn.open_datagram(purpose)
        };
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(DatagramChannel {
            shared: self.shared.clone(),
            connection_id: self.connection_id,
            channel_id,
        })
    }

    pub async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)> {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap();
                if let Some(entry) = state.connections.get_mut(&self.connection_id) {
                    if let Some((channel_id, purpose)) = entry.conn.accept_datagram() {
                        return Ok((
                            DatagramChannel {
                                shared: self.shared.clone(),
                                connection_id: self.connection_id,
                                channel_id,
                            },
                            purpose,
                        ));
                    }
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection gone",
                    ));
                }
            }
            self.shared.stream_notify.notified().await;
        }
    }
}

pub struct StreamChannel {
    shared: Arc<Shared>,
    connection_id: u64,
    channel_id: u32,
}

impl StreamChannel {
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write(self.channel_id, data)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap();
                let entry = state
                    .connections
                    .get_mut(&self.connection_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                    })?;
                match entry.conn.read(self.channel_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(channel_err_to_io(e)),
                }
            }
            self.shared.data_notify.notified().await;
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_channel(self.channel_id)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct DatagramChannel {
    pub(crate) shared: Arc<Shared>,
    pub(crate) connection_id: u64,
    pub(crate) channel_id: u32,
}

impl DatagramChannel {
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .write_datagram(self.channel_id, data)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut state = self.shared.state.lock().unwrap();
                let entry = state
                    .connections
                    .get_mut(&self.connection_id)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "connection gone")
                    })?;
                match entry.conn.read_datagram(self.channel_id) {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) => {}
                    Err(e) => return Err(channel_err_to_io(e)),
                }
            }
            self.shared.data_notify.notified().await;
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .connections
                .get_mut(&self.connection_id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
            entry
                .conn
                .close_channel(self.channel_id)
                .map_err(channel_err_to_io)?;
        }
        flush_connection(&self.shared, self.connection_id).await?;
        Ok(())
    }
}

pub(crate) fn channel_err_to_io(e: ChannelError) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, e.to_string())
}
