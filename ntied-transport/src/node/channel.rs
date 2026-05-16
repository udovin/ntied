use std::sync::{Arc, Mutex};

use tokio::io;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::connection::Connection as Inner;

use super::connection::NotifyMap;

pub struct Channel {
    pub(crate) channel_id: u64,
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) notify: Arc<Notify>,
    pub(crate) send_notify: Arc<Notify>,
    pub(crate) channel_notifies: NotifyMap,
    pub(crate) cancel_token: CancellationToken,
}

impl Channel {
    pub fn channel_id(&self) -> u64 {
        self.channel_id
    }

    /// Send a reliable message — delivery is guaranteed as long as the
    /// connection stays alive.  May return `WouldBlock` if the send buffer
    /// is full of other reliable messages and the new one does not fit.
    pub async fn send(&self, data: Vec<u8>) -> io::Result<()> {
        {
            let mut conn = self.inner.lock().unwrap();
            conn.channel_send(self.channel_id, data, true)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        self.send_notify.notify_one();
        Ok(())
    }

    /// Send an unreliable message.  If the send buffer is full, the transport
    /// automatically evicts the oldest unreliable in-flight message(s) to
    /// make room — and notifies the peer to discard them.  The new message
    /// itself may be evicted later if a newer unreliable send needs space.
    pub async fn send_unreliable(&self, data: Vec<u8>) -> io::Result<()> {
        {
            let mut conn = self.inner.lock().unwrap();
            conn.channel_send(self.channel_id, data, false)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        }
        self.send_notify.notify_one();
        Ok(())
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

    /// Resize the local send-buffer cap for this channel.  Shrink is lazy:
    /// already-queued messages stay, new `send`/`send_unreliable` calls may
    /// return `WouldBlock` (or auto-evict an older unreliable for the
    /// unreliable variant) until the buffer drains under the new limit.
    pub fn set_send_buf_cap(&self, cap: u64) {
        let mut conn = self.inner.lock().unwrap();
        conn.set_channel_send_buf_cap(self.channel_id, cap);
    }

    /// Resize the receive-buffer cap for this channel.  Grow takes effect
    /// via the next `ChannelMaxData` advertisement; shrink does not revoke
    /// already-granted credit (wire monotonicity).
    pub fn set_recv_buf_cap(&self, cap: u64) {
        let mut conn = self.inner.lock().unwrap();
        conn.set_channel_recv_buf_cap(self.channel_id, cap);
        drop(conn);
        // Trigger a send pass so a ChannelMaxData can go out promptly when
        // the new cap unlocks credit.
        self.send_notify.notify_one();
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.channel_close(self.channel_id);
            // Release any messages we received but never polled — keeps the
            // per-channel flow-control window consistent so the channel can
            // be cleaned up after peer's ChannelFin.
            conn.channel_drain_recv(self.channel_id);
        }
        self.channel_notifies
            .lock()
            .unwrap()
            .remove(&self.channel_id);
        self.send_notify.notify_one();
    }
}
