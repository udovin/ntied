use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ntied_crypto::{PrivateKey, PublicKey};
use rand::{Rng, thread_rng};
use tokio::net::{ToSocketAddrs, UdpSocket};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::{
    Connection, ConnectionRequest, Discovery, DiscoveryFactory, Packet, ServerDiscoveryFactory,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct Transport {
    inner: Arc<TransportInner>,
    discovery: Arc<dyn Discovery>,
}

impl Transport {
    const MAX_PACKETS: usize = 4;
    const PACKET_SIZE: usize = 65536;

    pub async fn bind(
        addr: impl ToSocketAddrs,
        private_key: PrivateKey,
        server_addr: SocketAddr,
    ) -> Result<Self, Error> {
        Self::bind_with_discovery(addr, private_key, ServerDiscoveryFactory::new(server_addr)).await
    }

    pub async fn bind_with_discovery(
        addr: impl ToSocketAddrs,
        private_key: PrivateKey,
        discovery_factory: impl DiscoveryFactory,
    ) -> Result<Self, Error> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let connection_counter = Arc::new(AtomicU32::new(1));
        let raw_connections = Arc::new(RwLock::new(HashMap::new()));
        let connections = Arc::new(RwLock::new(HashMap::new()));
        let peer_connection_ids = Arc::new(Mutex::new(HashMap::new()));
        let pending_handshakes = Arc::new(Mutex::new(HashMap::new()));
        let main_task = tokio::spawn(Self::main_loop(
            socket.clone(),
            raw_connections.clone(),
            connections.clone(),
            peer_connection_ids.clone(),
            pending_handshakes.clone(),
        ));
        let inner = Arc::new(TransportInner {
            socket,
            private_key,
            connection_counter,
            raw_connections,
            connections,
            peer_connection_ids,
            pending_handshakes,
            main_task,
        });
        let raw_transport = RawTransport(inner.clone());
        let discovery = discovery_factory.create(raw_transport).await?;
        Ok(Self { inner, discovery })
    }

    pub async fn connect(&self, public_key: &PublicKey) -> Result<Connection, Error> {
        let (connection_id, packet_rx) = self.new_connection_buffer()?;
        let peer_addr = self
            .discovery
            .send_connection_request(public_key, connection_id)
            .await?;
        match Connection::connect(
            self.inner.clone(),
            connection_id,
            peer_addr,
            public_key.clone(),
            packet_rx,
        )
        .await
        {
            Ok(v) => Ok(v),
            Err(err) => {
                tracing::trace!(
                    connection_id,
                    ?peer_addr,
                    ?public_key,
                    "Dropping failed connection source id",
                );
                let mut connections = self.inner.connections.write().unwrap();
                if connections.remove(&connection_id).is_none() {
                    tracing::error!(
                        connection_id,
                        ?peer_addr,
                        ?public_key,
                        "Inconsistent connection drop: Connection not found",
                    );
                };
                Err(err)
            }
        }
    }

    pub async fn accept(&self) -> Result<Connection, Error> {
        loop {
            let conn_request = self.discovery.recv_connection_request().await?;
            let connection_id = self.inner.connection_counter.fetch_add(1, Ordering::SeqCst);

            // Check if we have full peer info (public_key and connection_id)
            let has_full_info =
                conn_request.public_key.is_some() && conn_request.connection_id.is_some();

            if has_full_info {
                // Use regular accept with full peer info
                match self
                    .accept_with_full_info(connection_id, conn_request)
                    .await
                {
                    Ok(v) => return Ok(v),
                    Err(err) => {
                        tracing::warn!(err, connection_id, "Failed to accept connection");
                        continue;
                    }
                }
            } else {
                // Use holepunch accept when we don't have full peer info
                match self
                    .accept_with_holepunch(connection_id, conn_request)
                    .await
                {
                    Ok(v) => return Ok(v),
                    Err(err) => {
                        tracing::warn!(err, connection_id, "Failed to accept connection");
                        continue;
                    }
                }
            }
        }
    }

    async fn accept_with_full_info(
        &self,
        connection_id: u32,
        conn_request: ConnectionRequest,
    ) -> Result<Connection, Error> {
        let peer_public_key = conn_request.public_key.unwrap();
        let peer_connection_id = conn_request.connection_id.unwrap();
        let peer_addr = conn_request.socket_addr;

        let (packet_tx, packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        tracing::trace!(
            connection_id,
            peer_connection_id,
            ?peer_addr,
            ?peer_public_key,
            "Creating connection buffer (full info)",
        );
        {
            let mut connections = self.inner.connections.write().unwrap();
            match connections.entry(connection_id) {
                hash_map::Entry::Occupied(_) => {
                    return Err("Generated occupied source id".into());
                }
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(packet_tx);
                }
            }
            // Register handshake mapping for incoming connection
            {
                let mut peer_connection_ids_guard = self.inner.peer_connection_ids.lock().unwrap();
                match peer_connection_ids_guard.entry((peer_public_key.clone(), peer_connection_id))
                {
                    hash_map::Entry::Occupied(_) => {
                        tracing::debug!(
                            connection_id,
                            peer_connection_id,
                            "Handshake mapping already exists"
                        );
                        connections.remove(&connection_id);
                        return Err("Handshake mapping already exists".into());
                    }
                    hash_map::Entry::Vacant(entry) => {
                        entry.insert(connection_id);
                    }
                }
            }
        }
        match Connection::accept(
            self.inner.clone(),
            connection_id,
            peer_connection_id,
            peer_addr,
            peer_public_key.clone(),
            packet_rx,
        )
        .await
        {
            Ok(v) => {
                // Clean up handshake mapping
                self.inner
                    .peer_connection_ids
                    .lock()
                    .unwrap()
                    .remove(&(peer_public_key, peer_connection_id));
                Ok(v)
            }
            Err(err) => {
                tracing::trace!(
                    ?err,
                    connection_id,
                    peer_connection_id,
                    ?peer_addr,
                    "Dropping failed connection accept",
                );
                // Clean up handshake mapping
                self.inner
                    .peer_connection_ids
                    .lock()
                    .unwrap()
                    .remove(&(peer_public_key, peer_connection_id));
                self.inner
                    .connections
                    .write()
                    .unwrap()
                    .remove(&connection_id);
                Err(err)
            }
        }
    }

    async fn accept_with_holepunch(
        &self,
        connection_id: u32,
        conn_request: ConnectionRequest,
    ) -> Result<Connection, Error> {
        let peer_addr = conn_request.socket_addr;
        let (packet_tx, packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        tracing::trace!(
            connection_id,
            ?peer_addr,
            "Creating connection buffer (holepunch)",
        );
        {
            let mut connections = self.inner.connections.write().unwrap();
            match connections.entry(connection_id) {
                hash_map::Entry::Occupied(_) => {
                    return Err("Generated occupied source id".into());
                }
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(packet_tx);
                }
            }
            // Register handshake mapping for incoming connection
            {
                let mut pending_handshakes = self.inner.pending_handshakes.lock().unwrap();
                match pending_handshakes.entry(peer_addr) {
                    hash_map::Entry::Occupied(_) => {
                        tracing::debug!(connection_id, "Pending handshake mapping already exists");
                        connections.remove(&connection_id);
                        return Err("Pending handshake mapping already exists".into());
                    }
                    hash_map::Entry::Vacant(entry) => {
                        entry.insert(connection_id);
                    }
                }
            }
        }
        let connection = match Connection::accept_with_holepunch(
            self.inner.clone(),
            connection_id,
            peer_addr,
            packet_rx,
        )
        .await
        {
            Ok(v) => v,
            Err(err) => {
                tracing::trace!(
                    ?err,
                    connection_id,
                    ?peer_addr,
                    "Dropping failed holepunch connection",
                );
                // Clean up handshake mapping
                self.inner
                    .pending_handshakes
                    .lock()
                    .unwrap()
                    .remove(&peer_addr);
                self.inner
                    .connections
                    .write()
                    .unwrap()
                    .remove(&connection_id);
                return Err(err);
            }
        };
        Ok(connection)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.socket.local_addr().unwrap()
    }

    async fn main_loop(
        socket: Arc<UdpSocket>,
        raw_connections: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
        connections: Arc<RwLock<HashMap<u32, mpsc::Sender<(SocketAddr, Packet)>>>>,
        peer_connection_ids: Arc<Mutex<HashMap<(PublicKey, u32), u32>>>,
        pending_handshakes: Arc<Mutex<HashMap<SocketAddr, u32>>>,
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
                        tracing::trace!(?err, "Ignoring connection reset error");
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
                let connection_id = match &packet {
                    Packet::HolePunch(_) => {
                        // HolePunch packets are used for NAT traversal by acceptor.
                        // Initiator can safely ignore them.
                        tracing::trace!(?addr, "Ignoring HolePunch packet");
                        continue;
                    }
                    Packet::Encrypted(v) => v.peer_connection_id,
                    Packet::HandshakeAck(v) => v.peer_connection_id,
                    Packet::Handshake(v) => {
                        let public_key = match PublicKey::from_bytes(&v.public_key) {
                            Ok(key) => key,
                            Err(err) => {
                                tracing::warn!(?addr, "Invalid public key: {}", err);
                                continue;
                            }
                        };
                        let mut peer_connection_ids_guard = peer_connection_ids.lock().unwrap();
                        match peer_connection_ids_guard.get(&(public_key.clone(), v.connection_id))
                        {
                            Some(v) => *v,
                            None => {
                                let mut pending_handshakes_guard =
                                    pending_handshakes.lock().unwrap();
                                match pending_handshakes_guard.entry(addr) {
                                    hash_map::Entry::Occupied(entry) => {
                                        let connection_id = entry.remove();
                                        peer_connection_ids_guard
                                            .insert((public_key, v.connection_id), connection_id);
                                        connection_id
                                    }
                                    hash_map::Entry::Vacant(_) => {
                                        // TODO: We received a new incoming connection and should allocate it.
                                        // This is not necessary because Handshake packets will be sent many times.
                                        tracing::debug!(
                                            ?addr,
                                            "Received packet lost: Unknown handshake"
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                };
                tracing::trace!(
                    connection_id,
                    peer_addr = ?addr,
                    "Sending packet to connection buffer",
                );
                let connections_guard = connections.read().unwrap();
                if let Some(sender) = connections_guard.get(&connection_id) {
                    if let Err(err) = sender.try_send((addr, packet)) {
                        match err {
                            TrySendError::Closed(_) => {
                                tracing::debug!(
                                    ?err,
                                    connection_id,
                                    peer_addr = ?addr,
                                    "Received packet lost: Connection closed",
                                );
                            }
                            TrySendError::Full(_) => {
                                tracing::warn!(
                                    ?err,
                                    connection_id,
                                    peer_addr = ?addr,
                                    "Received packet lost: Connection buffer overflow",
                                );
                            }
                        }
                    } else {
                        tracing::trace!(
                            connection_id,
                            peer_addr = ?addr,
                            "Packet sent to connection buffer",
                        );
                    }
                } else {
                    tracing::warn!(
                        connection_id,
                        peer_addr = ?addr,
                        "Received packet lost: Connection not found",
                    );
                }
            }
        }
    }

    fn new_connection_buffer(&self) -> Result<(u32, mpsc::Receiver<(SocketAddr, Packet)>), Error> {
        let (tx, rx) = mpsc::channel(Self::MAX_PACKETS);
        let mut connections_guard = self.inner.connections.write().unwrap();
        for _ in 0..10 {
            let connection_id = thread_rng().r#gen();
            match connections_guard.entry(connection_id) {
                hash_map::Entry::Occupied(_) => continue,
                hash_map::Entry::Vacant(entry) => {
                    tracing::trace!(connection_id, "Created new connection buffer");
                    entry.insert(tx);
                    return Ok((connection_id, rx));
                }
            }
        }
        return Err("Failed to create new connection buffer".into());
    }
}

pub(crate) struct TransportInner {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) private_key: PrivateKey,
    connection_counter: Arc<AtomicU32>,
    #[allow(unused)]
    pub(crate) raw_connections: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
    pub(crate) connections: Arc<RwLock<HashMap<u32, mpsc::Sender<(SocketAddr, Packet)>>>>,
    peer_connection_ids: Arc<Mutex<HashMap<(PublicKey, u32), u32>>>,
    pending_handshakes: Arc<Mutex<HashMap<SocketAddr, u32>>>,
    main_task: JoinHandle<()>,
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

#[derive(Clone)]
pub struct RawTransport(Arc<TransportInner>);

impl RawTransport {
    pub fn private_key(&self) -> &PrivateKey {
        &self.0.private_key
    }

    pub fn connect(&self, addr: SocketAddr) -> Result<RawConnection, Error> {
        RawConnection::new(self.0.clone(), addr)
    }
}

pub struct RawConnection {
    transport: Arc<TransportInner>,
    addr: SocketAddr,
    rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
}

impl RawConnection {
    const MAX_PACKETS: usize = 4;

    pub(crate) fn new(transport: Arc<TransportInner>, addr: SocketAddr) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel(Self::MAX_PACKETS);
        let mut raw_connections = transport.raw_connections.write().unwrap();
        match raw_connections.entry(addr) {
            hash_map::Entry::Occupied(_) => return Err("Address already connected".into()),
            hash_map::Entry::Vacant(entry) => {
                entry.insert(tx);
                drop(raw_connections);
                Ok(Self {
                    transport,
                    addr,
                    rx: TokioMutex::new(rx),
                })
            }
        }
    }

    pub async fn send(&self, packet: Vec<u8>) -> Result<(), Error> {
        self.transport
            .socket
            .send_to(packet.as_slice(), self.addr)
            .await?;
        Ok(())
    }

    pub async fn recv(&self) -> Result<Vec<u8>, Error> {
        Ok(self
            .rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Connection closed")?)
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        self.transport
            .raw_connections
            .write()
            .unwrap()
            .remove(&self.addr);
    }
}
