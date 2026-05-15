use std::sync::{Arc, Mutex};

use tokio::io;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::connection::Connection as Inner;

use super::connection::NotifyMap;

pub struct Stream {
    pub(crate) stream_id: u64,
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) stream_notifies: NotifyMap,
    pub(crate) cancel_token: CancellationToken,
}

impl Stream {
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<usize> {
        loop {
            let notified = self.notify.notified();
            let written = {
                let mut conn = self.inner.lock().unwrap();
                conn.stream_write(self.stream_id, data, false)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?
            };
            if written > 0 {
                self.send_notify.notify_one();
                return Ok(written);
            }
            // self.send_notify.notify_one();
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

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, bool)> {
        loop {
            let notified = self.notify.notified();
            {
                let mut conn = self.inner.lock().unwrap();
                match conn.stream_read(self.stream_id, buf) {
                    Ok((n, fin)) if n > 0 || fin => {
                        drop(conn);
                        self.send_notify.notify_one();
                        return Ok((n, fin));
                    }
                    Ok(_) => {}
                    Err(crate::connection::Error::Done) => {}
                    Err(e) => return Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}"))),
                }
                if conn.is_closed() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "Connection closed",
                    ));
                }
            }
            // self.send_notify.notify_one();
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
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.stream_write(self.stream_id, &[], true);
        }
        self.send_notify.notify_one();
        self.cancel_token.cancel();
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.cancel_token.is_cancelled() {
            if let Ok(mut conn) = self.inner.lock() {
                let _ = conn.stream_write(self.stream_id, &[], true);
            }
            self.send_notify.notify_one();
        }
        self.stream_notifies.lock().unwrap().remove(&self.stream_id);
    }
}
