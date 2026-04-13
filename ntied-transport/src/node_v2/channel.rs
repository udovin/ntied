use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::connection_v2::Connection as Inner;

use super::connection::NotifyMap;

pub struct StreamChannel {
    pub(crate) stream_id: u64,
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) stream_notifies: NotifyMap,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
}

pub struct DatagramChannel {
    pub(crate) channel_id: u64,
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) channel_notifies: NotifyMap,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
}

impl StreamChannel {
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<usize> {
        let written = {
            let mut conn = self.inner.lock().unwrap();
            conn.stream_write(self.stream_id, data, false)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
        };
        flush(&self.inner, &self.send_notify, &self.socket, self.addr).await;
        Ok(written)
    }

    pub async fn send_fin(&self, data: &[u8]) -> io::Result<usize> {
        let written = {
            let mut conn = self.inner.lock().unwrap();
            conn.stream_write(self.stream_id, data, true)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
        };
        flush(&self.inner, &self.send_notify, &self.socket, self.addr).await;
        Ok(written)
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, bool)> {
        loop {
            let notified = self.notify.notified();
            {
                let mut conn = self.inner.lock().unwrap();
                match conn.stream_read(self.stream_id, buf) {
                    Ok((n, fin)) if n > 0 || fin => return Ok((n, fin)),
                    Ok(_) => {}
                    Err(crate::connection_v2::Error::Done) => {}
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("{e:?}"),
                        ))
                    }
                }
                if conn.is_closed() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Connection closed",
                    ));
                }
            }
            flush(&self.inner, &self.send_notify, &self.socket, self.addr).await;
            tokio::select! {
                _ = notified => {}
                _ = self.cancel_token.cancelled() => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Stream closed",
                    ));
                }
            }
        }
    }

    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

impl Drop for StreamChannel {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.stream_write(self.stream_id, &[], true);
        }
        self.stream_notifies
            .lock()
            .unwrap()
            .remove(&self.stream_id);
        // Wake main_loop to flush the FIN.
        self.send_notify.notify_one();
    }
}

impl DatagramChannel {
    pub fn channel_id(&self) -> u64 {
        self.channel_id
    }

    pub async fn send(&self, data: Vec<u8>, deadline: Instant) -> io::Result<u64> {
        let msg_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.channel_send(self.channel_id, data, deadline)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
        };
        flush(&self.inner, &self.send_notify, &self.socket, self.addr).await;
        Ok(msg_id)
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            let notified = self.notify.notified();
            {
                let mut conn = self.inner.lock().unwrap();
                match conn.channel_recv(self.channel_id) {
                    Ok(data) => return Ok(data),
                    Err(crate::connection_v2::Error::Done) => {}
                    Err(e) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("{e:?}"),
                        ))
                    }
                }
                if conn.is_closed() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Connection closed",
                    ));
                }
            }
            flush(&self.inner, &self.send_notify, &self.socket, self.addr).await;
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
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.channel_close(self.channel_id);
        }
        self.cancel_token.cancel();
    }
}

impl Drop for DatagramChannel {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.channel_close(self.channel_id);
        }
        self.channel_notifies
            .lock()
            .unwrap()
            .remove(&self.channel_id);
        self.send_notify.notify_one();
    }
}

async fn flush(_inner: &Mutex<Inner>, send_notify: &Notify, _socket: &UdpSocket, _addr: SocketAddr) {
    // Wake main_loop to drain pending sends.
    send_notify.notify_one();
}
