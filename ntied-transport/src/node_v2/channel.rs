use std::sync::{Arc, Mutex};

use tokio::io;
use tokio::sync::mpsc;

use crate::connection::Connection as InnerConnection;

use super::connection::ChannelMap;

pub struct StreamChannel {
    pub(crate) channel_id: u32,
    pub(crate) inner: Arc<Mutex<InnerConnection>>,
    pub(crate) channel_map: ChannelMap,
    pub(crate) rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
}

pub struct DatagramChannel {
    pub(crate) channel_id: u32,
    pub(crate) inner: Arc<Mutex<InnerConnection>>,
    pub(crate) channel_map: ChannelMap,
    pub(crate) rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
}

impl StreamChannel {
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.inner.lock().unwrap();
        conn.write(self.channel_id, data)
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

impl Drop for StreamChannel {
    fn drop(&mut self) {
        self.channel_map.write().unwrap().remove(&self.channel_id);
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.close_channel(self.channel_id);
        }
    }
}

impl DatagramChannel {
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut conn = self.inner.lock().unwrap();
        conn.write_datagram(self.channel_id, data)
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

impl Drop for DatagramChannel {
    fn drop(&mut self) {
        self.channel_map.write().unwrap().remove(&self.channel_id);
        if let Ok(mut conn) = self.inner.lock() {
            let _ = conn.close_channel(self.channel_id);
        }
    }
}
