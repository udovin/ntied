use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::connection::Connection as InnerConnection;

pub(crate) struct OwnedChannelId {
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
}

impl OwnedChannelId {
    pub(crate) fn new(channel_id: u32, inner: &Arc<Mutex<InnerConnection>>) -> Self {
        Self {
            channel_id,
            inner: inner.clone(),
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.channel_id
    }
}

impl Drop for OwnedChannelId {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.close_channel(self.channel_id);
        }
    }
}

pub struct StreamChannel {
    pub(crate) owned: OwnedChannelId,
    pub(crate) purpose: u16,
    pub(crate) data_notify: Arc<Notify>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
}

pub struct DatagramChannel {
    pub(crate) owned: OwnedChannelId,
    pub(crate) purpose: u16,
    pub(crate) data_notify: Arc<Notify>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
}

impl StreamChannel {
    pub fn purpose(&self) -> u16 {
        self.purpose
    }

    pub fn channel_id(&self) -> u32 {
        self.owned.id()
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut conn = self.owned.inner.lock().unwrap();
            conn.write(self.owned.id(), data)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        flush(&self.owned.inner, &self.socket, self.addr).await;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            let notified = self.data_notify.notified();
            {
                let mut conn = self.owned.inner.lock().unwrap();
                if let Ok(Some(data)) = conn.read(self.owned.id()) {
                    return Ok(data);
                }
                if conn.is_channel_finished(self.owned.id()) || conn.got_connection_close() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Channel closed",
                    ));
                }
            }
            flush(&self.owned.inner, &self.socket, self.addr).await;
            tokio::select! {
                _ = notified => {}
                _ = self.cancel_token.cancelled() => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Channel closed",
                    ));
                }
            }
        }
    }

    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

impl DatagramChannel {
    pub fn purpose(&self) -> u16 {
        self.purpose
    }

    pub fn channel_id(&self) -> u32 {
        self.owned.id()
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        {
            let mut conn = self.owned.inner.lock().unwrap();
            conn.write_datagram(self.owned.id(), data)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        flush(&self.owned.inner, &self.socket, self.addr).await;
        Ok(())
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            let notified = self.data_notify.notified();
            {
                let mut conn = self.owned.inner.lock().unwrap();
                if let Ok(Some(data)) = conn.read_datagram(self.owned.id()) {
                    return Ok(data);
                }
                if conn.is_channel_finished(self.owned.id()) || conn.got_connection_close() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Channel closed",
                    ));
                }
            }
            flush(&self.owned.inner, &self.socket, self.addr).await;
            tokio::select! {
                _ = notified => {}
                _ = self.cancel_token.cancelled() => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Channel closed",
                    ));
                }
            }
        }
    }

    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

async fn flush(inner: &Mutex<InnerConnection>, socket: &UdpSocket, addr: SocketAddr) {
    let packets = {
        let mut conn = inner.lock().unwrap();
        conn.poll_packets(Instant::now())
    };
    for packet in packets {
        if let Err(err) = socket.send_to(&packet.encode(), addr).await {
            warn!(?err, "Failed to send packet");
        }
    }
}
