use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use tokio::io;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::PrivateKey;
use crate::wire::Packet;

use super::RECV_BUF_SIZE;

pub(super) struct NodeInner {
    socket: Arc<UdpSocket>,
    connection_map: Arc<RwLock<HashMap<u64, ()>>>,
    next_connection_id: Arc<AtomicU64>,
    recv_task: JoinHandle<()>,
}

impl NodeInner {}

pub struct Node2 {
    inner: Arc<NodeInner>,
}

impl Node2 {
    pub async fn bind(addr: SocketAddr, private_key: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let connection_map = Arc::default();
        let next_connection_id = Arc::default();
        let recv_task = tokio::spawn(Self::recv_loop(socket.clone()));
        let inner = Arc::new(NodeInner {
            socket,
            connection_map,
            next_connection_id,
            recv_task,
        });
        Ok(Self { inner })
    }

    async fn recv_loop(socket: Arc<UdpSocket>) {
        let mut buf = vec![0u8; RECV_BUF_SIZE].into_boxed_slice();
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let packet = match Packet::decode(&buf[..len]) {
                        Ok(packet) => packet,
                        Err(err) => continue,
                    };
                    match packet {
                        Packet::KeyExchangeInit(v) => {}
                        Packet::KeyExchangeResponse(v) => {}
                        Packet::Data(v) => {}
                    }
                }
                Err(_) => {
                    todo!()
                }
            }
        }
    }
}
