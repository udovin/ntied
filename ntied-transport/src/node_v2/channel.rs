use std::sync::{Arc, Mutex};

use tokio::io;
use tokio::sync::{Mutex as TokioMutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::connection::Connection as InnerConnection;

pub(crate) struct OwnedChannelId {
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
    cancel_token: CancellationToken,
}

impl OwnedChannelId {
    pub(crate) fn new(
        channel_id: u32,
        inner: &Arc<Mutex<InnerConnection>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            channel_id,
            inner: inner.clone(),
            cancel_token,
        }
    }
}

impl Drop for OwnedChannelId {
    fn drop(&mut self) {
        self.cancel_token.cancel();
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.close_channel(self.channel_id);
        }
    }
}

pub struct StreamChannel {
    pub(crate) owned: OwnedChannelId,
    pub(crate) purpose: u16,
    pub(crate) rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

pub struct DatagramChannel {
    pub(crate) owned: OwnedChannelId,
    pub(crate) purpose: u16,
    pub(crate) rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

impl StreamChannel {
    pub fn purpose(&self) -> u16 {
        self.purpose
    }

    pub fn channel_id(&self) -> u32 {
        self.owned.channel_id
    }

    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.owned.inner.lock().unwrap();
        conn.write(self.owned.channel_id, data)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "channel closed"))
    }
}

impl DatagramChannel {
    pub fn purpose(&self) -> u16 {
        self.purpose
    }

    pub fn channel_id(&self) -> u32 {
        self.owned.channel_id
    }

    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.owned.inner.lock().unwrap();
        conn.write_datagram(self.owned.channel_id, data)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "channel closed"))
    }
}

pub(crate) async fn stream_read_loop(
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
    data_notify: Arc<Notify>,
    cancel_token: CancellationToken,
    tx: mpsc::Sender<Vec<u8>>,
) {
    loop {
        tokio::select! {
            _ = data_notify.notified() => {
                loop {
                    let data = {
                        let mut conn = inner.lock().unwrap();
                        conn.read(channel_id)
                    };
                    match data {
                        Ok(Some(data)) => {
                            if tx.send(data).await.is_err() {
                                return;
                            }
                        }
                        _ => break,
                    }
                }
                if inner.lock().unwrap().is_channel_finished(channel_id) {
                    return;
                }
            }
            _ = cancel_token.cancelled() => {
                return;
            }
        }
    }
}

pub(crate) async fn datagram_read_loop(
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
    data_notify: Arc<Notify>,
    cancel_token: CancellationToken,
    tx: mpsc::Sender<Vec<u8>>,
) {
    loop {
        tokio::select! {
            _ = data_notify.notified() => {
                loop {
                    let data = {
                        let mut conn = inner.lock().unwrap();
                        conn.read_datagram(channel_id)
                    };
                    match data {
                        Ok(Some(data)) => {
                            if tx.send(data).await.is_err() {
                                return;
                            }
                        }
                        _ => break,
                    }
                }
                if inner.lock().unwrap().is_channel_finished(channel_id) {
                    return;
                }
            }
            _ = cancel_token.cancelled() => {
                return;
            }
        }
    }
}
