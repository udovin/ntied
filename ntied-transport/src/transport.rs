use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use ntied_crypto::PrivateKey;
use tokio::net::{ToSocketAddrs, UdpSocket};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::{Address, Connection, Packet, ServerConnection};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct Transport {
    inner: Arc<TransportInner>,
    server_connection: ServerConnection,
}

impl Transport {
    const MAX_PACKETS: usize = 4;
    const PACKET_SIZE: usize = 65536;

    pub async fn bind(
        addr: impl ToSocketAddrs,
        address: Address,
        private_key: PrivateKey,
        server_addr: SocketAddr,
    ) -> Result<Self, Error> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let source_counter = Arc::new(AtomicU32::new(1));
        let raw_connections = Arc::new(RwLock::new(HashMap::new()));
        let connections = Arc::new(RwLock::new(HashMap::new()));
        let handshakes = Arc::new(RwLock::new(HashMap::new()));
        let main_task = tokio::spawn(Self::main_loop(
            socket.clone(),
            raw_connections.clone(),
            connections.clone(),
            handshakes.clone(),
        ));
        let inner = Arc::new(TransportInner {
            socket,
            address,
            private_key,
            source_counter,
            raw_connections: raw_connections.clone(),
            connections,
            handshakes,
            main_task,
        });
        // TODO: Refactor this.
        let (server_tx, server_rx) = mpsc::channel(Self::MAX_PACKETS);
        raw_connections
            .write()
            .unwrap()
            .insert(server_addr, server_tx);
        let server_connection =
            ServerConnection::new(inner.clone(), server_addr, server_rx, address).await?;
        Ok(Self {
            inner,
            server_connection,
        })
    }

    pub async fn connect(&self, address: Address) -> Result<Connection, Error> {
        let source_id = self.inner.source_counter.fetch_add(1, Ordering::SeqCst);
        let peer_info = self.server_connection.connect(&address, source_id).await?;
        let (packet_tx, packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        tracing::trace!(
            source_id = source_id,
            peer_addr = ?peer_info.addr,
            peer_address = ?peer_info.address,
            "Creating connection buffer",
        );
        {
            let mut connections = self.inner.connections.write().unwrap();
            match connections.entry(source_id) {
                hash_map::Entry::Occupied(_) => {
                    return Err("Generated occupied source id".into());
                }
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(packet_tx);
                }
            }
        }
        match Connection::connect(
            self.inner.clone(),
            source_id,
            peer_info.addr,
            peer_info.address,
            peer_info.public_key,
            packet_rx,
        )
        .await
        {
            Ok(v) => Ok(v),
            Err(err) => {
                tracing::trace!(
                    source_id = source_id,
                    peer_addr = ?peer_info.addr,
                    peer_address = ?peer_info.address,
                    "Dropping failed connection source id",
                );
                let mut connections = self.inner.connections.write().unwrap();
                if connections.remove(&source_id).is_none() {
                    tracing::error!(
                        source_id = source_id,
                        peer_addr = ?peer_info.addr,
                        peer_address = ?peer_info.address,
                        "Inconsistent connection drop: Connection not found",
                    );
                };
                Err(err)
            }
        }
    }

    pub async fn accept(&self) -> Result<Connection, Error> {
        loop {
            let peer_info = self.server_connection.accept().await?;
            let source_id = self.inner.source_counter.fetch_add(1, Ordering::SeqCst);
            let target_id = peer_info.source_id.unwrap();
            let (packet_tx, packet_rx) = mpsc::channel(Self::MAX_PACKETS);
            tracing::trace!(
                source_id,
                target_id,
                peer_addr = ?peer_info.addr,
                peer_address = ?peer_info.address,
                "Creating connection buffer",
            );
            {
                let mut connections = self.inner.connections.write().unwrap();
                match connections.entry(source_id) {
                    hash_map::Entry::Occupied(_) => {
                        continue;
                    }
                    hash_map::Entry::Vacant(entry) => {
                        entry.insert(packet_tx);
                    }
                }
            }
            // Register handshake mapping for incoming connection
            {
                let mut handshakes = self.inner.handshakes.write().unwrap();
                match handshakes.entry((peer_info.address, target_id)) {
                    hash_map::Entry::Occupied(_) => {
                        drop(handshakes);
                        tracing::debug!(
                            source_id,
                            target_id,
                            peer_address = ?peer_info.address,
                            "Handshake mapping already exists"
                        );
                        self.inner.connections.write().unwrap().remove(&source_id);
                        continue;
                    }
                    hash_map::Entry::Vacant(entry) => {
                        entry.insert(source_id);
                    }
                }
            }
            let connection = match Connection::accept(
                self.inner.clone(),
                source_id,
                target_id,
                peer_info.addr,
                peer_info.address,
                peer_info.public_key,
                packet_rx,
            )
            .await
            {
                Ok(v) => {
                    // Clean up handshake mapping
                    self.inner
                        .handshakes
                        .write()
                        .unwrap()
                        .remove(&(peer_info.address, target_id));
                    v
                }
                Err(err) => {
                    tracing::trace!(
                        ?err,
                        source_id,
                        target_id,
                        peer_addr = ?peer_info.addr,
                        peer_address = ?peer_info.address,
                        "Dropping failed connection source id",
                    );
                    // Clean up handshake mapping
                    self.inner
                        .handshakes
                        .write()
                        .unwrap()
                        .remove(&(peer_info.address, target_id));
                    let mut connections = self.inner.connections.write().unwrap();
                    if connections.remove(&source_id).is_none() {
                        tracing::error!(
                            source_id = source_id,
                            target_id = target_id,
                            peer_addr = ?peer_info.addr,
                            peer_address = ?peer_info.address,
                            "Inconsistent connection drop: Connection not found"
                        );
                    };
                    continue;
                }
            };
            return Ok(connection);
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.socket.local_addr().unwrap()
    }

    pub fn address(&self) -> Address {
        self.inner.address
    }

    async fn main_loop(
        socket: Arc<UdpSocket>,
        raw_connections: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
        connections: Arc<RwLock<HashMap<u32, mpsc::Sender<(SocketAddr, Packet)>>>>,
        handshakes: Arc<RwLock<HashMap<(Address, u32), u32>>>,
    ) {
        let mut buf = [0u8; Self::PACKET_SIZE];
        loop {
            tracing::trace!("Waiting packet");
            let (len, addr) = match socket.recv_from(&mut buf).await {
                Ok((len, addr)) => (len, addr),
                Err(err) => {
                    if cfg!(target_os = "windows")
                        && err.kind() == std::io::ErrorKind::ConnectionReset
                    {
                        tracing::debug!(?err, "Ignoring connection reset error");
                        continue;
                    }
                    tracing::debug!(?err, "Cannot receive packet from transport socket");
                    continue;
                }
            };
            tracing::trace!(
                peer_addr = ?addr,
                packet_len = len,
                "Received packet",
            );
            {
                let raw_connections_guard = raw_connections.read().unwrap();
                if let Some(sender) = raw_connections_guard.get(&addr) {
                    tracing::trace!(
                        peer_addr = ?addr,
                        "Sending packet to raw connection buffer",
                    );
                    if let Err(err) = sender.try_send(buf[..len].to_vec()) {
                        match err {
                            TrySendError::Closed(_) => {
                                tracing::debug!(?err, "Received packet lost: Connection closed");
                            }
                            TrySendError::Full(_) => {
                                tracing::warn!(
                                    ?err,
                                    "Received packet lost: Connection buffer overflow"
                                );
                            }
                        }
                    } else {
                        tracing::trace!(peer_addr = ?addr, "Packet sent to raw connection buffer");
                    }
                    continue;
                }
            }
            {
                tracing::trace!(peer_addr = ?addr, "Parsing packet");
                let packet = match Packet::deserialize(&buf[..len]) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::debug!(?err, "Received packet lost: Invalid packet");
                        continue;
                    }
                };
                tracing::trace!(peer_addr = ?addr, "Extracting packet stream");
                let target_id = match &packet {
                    Packet::Encrypted(v) => v.target_id,
                    Packet::HandshakeAck(v) => v.target_id,
                    Packet::Handshake(v) => {
                        let handshakes_guard = handshakes.read().unwrap();
                        match handshakes_guard.get(&(v.address, v.source_id)) {
                            Some(v) => *v,
                            None => {
                                // TODO: We received a new incoming connection and should allocate it.
                                // This is not necessary because Handshake packets will be sent many times.
                                tracing::debug!(?addr, "Received packet lost: Unknown handshake");
                                continue;
                            }
                        }
                    }
                };
                tracing::trace!(
                    target_id,
                    peer_addr = ?addr,
                    "Sending packet to connection buffer",
                );
                let connections_guard = connections.read().unwrap();
                if let Some(sender) = connections_guard.get(&target_id) {
                    if let Err(err) = sender.try_send((addr, packet)) {
                        match err {
                            TrySendError::Closed(_) => {
                                tracing::debug!(
                                    ?err,
                                    target_id,
                                    peer_addr = ?addr,
                                    "Received packet lost: Connection closed",
                                );
                            }
                            TrySendError::Full(_) => {
                                tracing::warn!(
                                    ?err,
                                    target_id,
                                    peer_addr = ?addr,
                                    "Received packet lost: Connection buffer overflow",
                                );
                            }
                        }
                    } else {
                        tracing::trace!(
                            target_id,
                            peer_addr = ?addr,
                            "Packet sent to connection buffer",
                        );
                    }
                } else {
                    tracing::warn!(
                        target_id,
                        peer_addr = ?addr,
                        "Received packet lost: Connection not found",
                    );
                }
            }
        }
    }
}

pub(crate) struct TransportInner {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) address: Address,
    pub(crate) private_key: PrivateKey,
    source_counter: Arc<AtomicU32>,
    #[allow(unused)]
    pub(crate) raw_connections: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
    pub(crate) connections: Arc<RwLock<HashMap<u32, mpsc::Sender<(SocketAddr, Packet)>>>>,
    handshakes: Arc<RwLock<HashMap<(Address, u32), u32>>>,
    main_task: JoinHandle<()>,
}

pub(crate) struct RawConnection {
    addr: SocketAddr,
    rx: mpsc::Receiver<Vec<u8>>,
    transport: Arc<TransportInner>,
}

impl RawConnection {
    const MAX_PACKETS: usize = 4;

    pub fn new(transport: Arc<TransportInner>, addr: SocketAddr) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel(Self::MAX_PACKETS);
        transport.raw_connections.write().unwrap().insert(addr, tx);
        Ok(Self {
            addr,
            rx,
            transport,
        })
    }

    pub async fn send(&self, packet: Vec<u8>) -> Result<(), Error> {
        self.transport
            .socket
            .send_to(packet.as_slice(), self.addr)
            .await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>, Error> {
        Ok(self.rx.recv().await.ok_or("Connection closed")?)
    }
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}
