use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rand::{RngCore, thread_rng};
use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::crypto::{KemPrivateKey, PeerId, PrivateKey};
use crate::wire::{Handshake, Packet};

use super::connection::{Connection, ConnectionMap, OwnedConnectionId};

pub struct Node {
    socket: Arc<UdpSocket>,
    identity: Arc<PrivateKey>,
    next_connection_id: Arc<AtomicU64>,
    connection_map: ConnectionMap,
    cancel_token: CancellationToken,
    accept_rx: TokioMutex<mpsc::Receiver<Connection>>,
    recv_task: Mutex<Option<JoinHandle<()>>>,
}

impl Node {
    const PACKET_BUFFER_SIZE: usize = 64;
    const RECV_BUFFER_SIZE: usize = 2048;

    pub async fn bind(addr: SocketAddr, private_key: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let identity = Arc::new(private_key);
        let next_connection_id = Arc::new(AtomicU64::new(thread_rng().next_u64()));
        let connection_map: ConnectionMap = Default::default();
        let cancel_token = CancellationToken::new();
        let (accept_tx, accept_rx) = mpsc::channel(1);
        let recv_task = tokio::spawn(Self::recv_loop(
            socket.clone(),
            identity.clone(),
            next_connection_id.clone(),
            connection_map.clone(),
            cancel_token.clone(),
            accept_tx,
        ));
        Ok(Self {
            socket,
            identity,
            next_connection_id,
            connection_map,
            cancel_token,
            accept_rx: TokioMutex::new(accept_rx),
            recv_task: Mutex::new(Some(recv_task)),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.identity.public_key().peer_id()
    }

    pub async fn accept(&self) -> io::Result<Connection> {
        self.accept_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Node shutdown"))
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<Connection> {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(Self::PACKET_BUFFER_SIZE);
        let owned_connection_id = OwnedConnectionId::new(connection_id, &self.connection_map, tx);

        let eph = KemPrivateKey::generate();
        let init = Handshake {
            initiator_connection_id: connection_id,
            kem_public_key: eph.public_key(),
        };
        self.socket.send_to(&init.encode(), addr).await?;

        Connection::connect(
            owned_connection_id,
            eph,
            init,
            rx,
            self.socket.clone(),
            self.identity.clone(),
            self.cancel_token.child_token(),
            addr,
        )
        .await
    }

    pub async fn shutdown(&self) -> Result<(), JoinError> {
        let recv_task = self.recv_task.lock().unwrap().take();
        if let Some(task) = recv_task {
            self.cancel_token.cancel();
            task.await?;
        }
        Ok(())
    }

    async fn recv_loop(
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        next_connection_id: Arc<AtomicU64>,
        connection_map: ConnectionMap,
        cancel_token: CancellationToken,
        accept_tx: mpsc::Sender<Connection>,
    ) {
        let mut buf = vec![0u8; Self::RECV_BUFFER_SIZE];
        loop {
            tokio::select! {
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((len, addr)) => {
                            let packet = match Packet::decode(&buf[..len]) {
                                Ok(packet) => packet,
                                Err(err) => {
                                    warn!(?err, "Failed to decode packet");
                                    continue
                                },
                            };
                            match packet {
                                Packet::Handshake(v) => {
                                    trace!(
                                        peer_connection_id = v.initiator_connection_id,
                                        "Received Handshake packet"
                                    );
                                    let responder_connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
                                    let connection_cancel_token = cancel_token.child_token();
                                    let (tx, rx) = mpsc::channel(Self::PACKET_BUFFER_SIZE);
                                    let owned_id = OwnedConnectionId::new(responder_connection_id, &connection_map, tx);
                                    tokio::spawn(Connection::accept(
                                        v,
                                        owned_id,
                                        socket.clone(),
                                        identity.clone(),
                                        rx,
                                        accept_tx.clone(),
                                        connection_cancel_token,
                                        addr,
                                    ));
                                }
                                Packet::HandshakeAck(v) => {
                                    trace!(
                                        connection_id = v.initiator_connection_id,
                                        peer_connection_id = v.responder_connection_id,
                                        "Received HandshakeAck packet"
                                    );
                                    let map = connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&v.initiator_connection_id) {
                                        if let Err(err) = tx.try_send(Packet::HandshakeAck(v)) {
                                            warn!(?err, "Failed to send HandshakeAck packet");
                                        }
                                    } else {
                                        warn!("No connection found with connection_id: {}", v.initiator_connection_id);
                                    }
                                }
                                Packet::Data(v) => {
                                    trace!(
                                        connection_id = v.receiver_connection_id,
                                        epoch = v.epoch,
                                        "Received Data packet"
                                    );
                                    let map = connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&v.receiver_connection_id) {
                                        if let Err(err) = tx.try_send(Packet::Data(v)) {
                                            warn!(?err, "Failed to send Data packet");
                                        }
                                    } else {
                                        warn!("No connection found with connection_id: {}", v.receiver_connection_id);
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            if cfg!(target_os = "windows")
                                && err.kind() == io::ErrorKind::ConnectionReset
                            {
                                trace!("Ignored connection reset");
                                continue;
                            }
                            warn!(?err, "Failed to receive from UDP socket");
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    trace!("Receive loop for node is stopped");
                    return;
                }
            }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let recv_task = self.recv_task.lock().unwrap().take();
        if let Some(task) = recv_task {
            self.cancel_token.cancel();
            drop(task);
        }
    }
}
