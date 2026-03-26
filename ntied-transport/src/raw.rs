use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use tokio::net::UdpSocket;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

pub type RouteMap = Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>;

const RAW_CHANNEL_SIZE: usize = 64;

pub struct TransportSocket {
    socket: Arc<UdpSocket>,
    routes: RouteMap,
}

impl TransportSocket {
    pub(crate) fn new(socket: Arc<UdpSocket>, routes: RouteMap) -> Self {
        Self { socket, routes }
    }

    pub fn connect(&self, addr: SocketAddr) -> io::Result<RawConnection> {
        RawConnection::new(self.socket.clone(), self.routes.clone(), addr)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

pub struct RawConnection {
    socket: Arc<UdpSocket>,
    addr: SocketAddr,
    rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
    routes: RouteMap,
}

impl RawConnection {
    fn new(socket: Arc<UdpSocket>, routes: RouteMap, addr: SocketAddr) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel(RAW_CHANNEL_SIZE);
        {
            let mut map = routes.write().unwrap();
            if map.contains_key(&addr) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "route already registered",
                ));
            }
            map.insert(addr, tx);
        }
        Ok(Self {
            socket,
            addr,
            rx: TokioMutex::new(rx),
            routes,
        })
    }

    pub async fn send(&self, data: &[u8]) -> io::Result<()> {
        self.socket.send_to(data, self.addr).await?;
        Ok(())
    }

    pub async fn recv(&self) -> Option<Vec<u8>> {
        self.rx.lock().await.recv().await
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        self.routes.write().unwrap().remove(&self.addr);
    }
}
