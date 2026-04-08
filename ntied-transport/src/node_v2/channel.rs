use std::sync::{Arc, Mutex};

use tokio::io;
use tokio::sync::{Mutex as TokioMutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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
    pub(crate) cancel_token: CancellationToken,
    pub(crate) read_task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

pub struct DatagramChannel {
    pub(crate) owned: OwnedChannelId,
    pub(crate) purpose: u16,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) read_task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

impl StreamChannel {
    pub fn purpose(&self) -> u16 {
        self.purpose
    }

    pub fn channel_id(&self) -> u32 {
        self.owned.id()
    }

    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.owned.inner.lock().unwrap();
        conn.write(self.owned.id(), data)
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

    pub async fn close(&self) {
        let task = self.read_task.lock().unwrap().take();
        if let Some(task) = task {
            self.cancel_token.cancel();
            let _ = task.await;
        }
    }
}

impl Drop for StreamChannel {
    fn drop(&mut self) {
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

    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.owned.inner.lock().unwrap();
        conn.write_datagram(self.owned.id(), data)
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

    pub async fn close(&self) {
        let task = self.read_task.lock().unwrap().take();
        if let Some(task) = task {
            self.cancel_token.cancel();
            let _ = task.await;
        }
    }
}

impl Drop for DatagramChannel {
    fn drop(&mut self) {
        let read_task = self.read_task.lock().unwrap().take();
        if let Some(task) = read_task {
            self.cancel_token.cancel();
            drop(task);
        }
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
        let notified = data_notify.notified();

        if drain_stream(&inner, channel_id, &tx).await {
            return;
        }

        tokio::select! {
            _ = notified => {}
            _ = cancel_token.cancelled() => return,
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
        let notified = data_notify.notified();

        if drain_datagram(&inner, channel_id, &tx).await {
            return;
        }

        tokio::select! {
            _ = notified => {}
            _ = cancel_token.cancelled() => return,
        }
    }
}

/// Drain all available data. Returns true if the loop should exit
/// (channel finished, connection closed, or consumer dropped).
async fn drain_stream(
    inner: &Mutex<InnerConnection>,
    channel_id: u32,
    tx: &mpsc::Sender<Vec<u8>>,
) -> bool {
    loop {
        let data = {
            let mut conn = inner.lock().unwrap();
            conn.read(channel_id)
        };
        match data {
            Ok(Some(data)) => {
                if tx.send(data).await.is_err() {
                    return true;
                }
            }
            _ => break,
        }
    }
    let conn = inner.lock().unwrap();
    conn.is_channel_finished(channel_id) || conn.got_connection_close()
}

async fn drain_datagram(
    inner: &Mutex<InnerConnection>,
    channel_id: u32,
    tx: &mpsc::Sender<Vec<u8>>,
) -> bool {
    loop {
        let data = {
            let mut conn = inner.lock().unwrap();
            conn.read_datagram(channel_id)
        };
        match data {
            Ok(Some(data)) => {
                if tx.send(data).await.is_err() {
                    return true;
                }
            }
            _ => break,
        }
    }
    let conn = inner.lock().unwrap();
    conn.is_channel_finished(channel_id) || conn.got_connection_close()
}
