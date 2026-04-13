use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::connection_v2::Connection as Inner;

use super::connection::NotifyMap;

pub struct Channel {
    pub(crate) channel_id: u64,
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) channel_notifies: NotifyMap,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
}

impl Channel {
    pub fn channel_id(&self) -> u64 {
        self.channel_id
    }

    pub async fn send(&self, data: Vec<u8>, deadline: Instant) -> io::Result<u64> {
        let msg_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.channel_send(self.channel_id, data, deadline)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
        };
        self.send_notify.notify_one();
        Ok(msg_id)
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            let notified = self.notify.notified();
            {
                let mut conn = self.inner.lock().unwrap();
                match conn.channel_recv(self.channel_id) {
                    Ok(data) => {
                        drop(conn);
                        self.send_notify.notify_one();
                        return Ok(data);
                    }
                    Err(crate::connection_v2::Error::Done) => {}
                    Err(e) => return Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}"))),
                }
                if conn.is_closed() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Connection closed",
                    ));
                }
            }
            self.send_notify.notify_one();
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

impl Drop for Channel {
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
