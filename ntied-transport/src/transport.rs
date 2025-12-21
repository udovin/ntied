use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ntied_crypto::{PrivateKey, PublicKey};
use rand::{Rng, thread_rng};
use tokio::net::{ToSocketAddrs, UdpSocket};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::{
    Connection, ConnectionRequest, Discovery, DiscoveryFactory, Packet, RawTransport,
    ServerDiscoveryFactory,
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
        let peer_socket_addrs = Arc::new(Mutex::new(HashMap::new()));
        let main_task = tokio::spawn(Self::main_loop(
            socket.clone(),
            raw_connections.clone(),
            connections.clone(),
            peer_connection_ids.clone(),
            peer_socket_addrs.clone(),
        ));
        let inner = Arc::new(TransportInner {
            socket,
            private_key,
            connection_counter,
            raw_connections,
            connections,
            peer_connection_ids,
            peer_socket_addrs,
            main_task,
        });
        let raw_transport = RawTransport::new(inner.clone());
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
        }
        // Register handshake mapping for incoming connection (auto-cleanup via RAII)
        let owned_peer_connection_id = match OwnedPeerConnectionId::new(
            &self.inner,
            peer_public_key.clone(),
            peer_connection_id,
            connection_id,
        ) {
            Ok(v) => v,
            Err(err) => {
                tracing::debug!(
                    connection_id,
                    peer_connection_id,
                    "Handshake mapping already exists"
                );
                self.inner
                    .connections
                    .write()
                    .unwrap()
                    .remove(&connection_id);
                return Err(err);
            }
        };
        Connection::accept(
            self.inner.clone(),
            connection_id,
            peer_connection_id,
            peer_addr,
            peer_public_key.clone(),
            packet_rx,
            owned_peer_connection_id,
        )
        .await
        .map_err(|err| {
            tracing::trace!(
                ?err,
                connection_id,
                peer_connection_id,
                ?peer_addr,
                "Dropping failed connection accept",
            );
            // owned_peer_connection_id is dropped here automatically
            self.inner
                .connections
                .write()
                .unwrap()
                .remove(&connection_id);
            err
        })
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
        }
        // Register handshake mapping for incoming connection (auto-cleanup via RAII)
        let owned_peer_socket_addr =
            match OwnedPeerSocketAddr::new(&self.inner, peer_addr, connection_id) {
                Ok(v) => v,
                Err(err) => {
                    tracing::debug!(connection_id, "Pending handshake mapping already exists");
                    self.inner
                        .connections
                        .write()
                        .unwrap()
                        .remove(&connection_id);
                    return Err(err);
                }
            };
        Connection::accept_with_holepunch(
            self.inner.clone(),
            connection_id,
            peer_addr,
            packet_rx,
            owned_peer_socket_addr,
        )
        .await
        .map_err(|err| {
            tracing::trace!(
                ?err,
                connection_id,
                ?peer_addr,
                "Dropping failed holepunch connection",
            );
            // owned_peer_socket_addr is dropped here automatically
            self.inner
                .connections
                .write()
                .unwrap()
                .remove(&connection_id);
            err
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.socket.local_addr().unwrap()
    }

    async fn main_loop(
        socket: Arc<UdpSocket>,
        raw_connections: Arc<RwLock<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
        connections: Arc<RwLock<HashMap<u32, mpsc::Sender<(SocketAddr, Packet)>>>>,
        peer_connection_ids: Arc<Mutex<HashMap<(PublicKey, u32), u32>>>,
        peer_socket_addrs: Arc<Mutex<HashMap<SocketAddr, u32>>>,
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
                        // First check peer_connection_ids (for already-upgraded connections)
                        if let Some(&id) = peer_connection_ids
                            .lock()
                            .unwrap()
                            .get(&(public_key.clone(), v.connection_id))
                        {
                            id
                        } else if let Some(&id) = peer_socket_addrs.lock().unwrap().get(&addr) {
                            // Route to pending holepunch connection (upgrade happens in Connection)
                            id
                        } else {
                            // TODO: We received a new incoming connection and should allocate it.
                            // This is not necessary because Handshake packets will be sent many times.
                            tracing::debug!(?addr, "Received packet lost: Unknown handshake");
                            continue;
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
    pub(crate) peer_connection_ids: Arc<Mutex<HashMap<(PublicKey, u32), u32>>>,
    pub(crate) peer_socket_addrs: Arc<Mutex<HashMap<SocketAddr, u32>>>,
    main_task: JoinHandle<()>,
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

pub(crate) struct OwnedPeerSocketAddr {
    map: Arc<Mutex<HashMap<SocketAddr, u32>>>,
    peer_socket_addr: SocketAddr,
    connection_id: u32,
}

impl OwnedPeerSocketAddr {
    pub fn new(
        transport: &TransportInner,
        peer_socket_addr: SocketAddr,
        connection_id: u32,
    ) -> Result<Self, Error> {
        {
            let mut map_guard = transport.peer_socket_addrs.lock().unwrap();
            match map_guard.entry(peer_socket_addr) {
                hash_map::Entry::Occupied(_) => {
                    return Err("Peer socket_addr already in use".into());
                }
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(connection_id);
                }
            }
        }
        tracing::trace!("Peer socket_addr added");
        Ok(Self {
            map: transport.peer_socket_addrs.clone(),
            peer_socket_addr,
            connection_id,
        })
    }

    pub fn upgrade(
        self,
        transport: &TransportInner,
        peer_public_key: PublicKey,
        peer_connection_id: u32,
    ) -> Result<OwnedPeerConnectionId, Error> {
        let connection_id = self.connection_id;
        drop(self);
        OwnedPeerConnectionId::new(
            transport,
            peer_public_key,
            peer_connection_id,
            connection_id,
        )
    }
}

impl Drop for OwnedPeerSocketAddr {
    fn drop(&mut self) {
        let mut map_guard = self.map.lock().unwrap();
        match map_guard.entry(self.peer_socket_addr) {
            hash_map::Entry::Occupied(entry) => {
                if entry.get() == &self.connection_id {
                    tracing::trace!(
                        peer_socket_addr = ?self.peer_socket_addr,
                        connection_id = self.connection_id,
                        "Removing peer socket_addr"
                    );
                    entry.remove();
                }
                drop(map_guard);
            }
            hash_map::Entry::Vacant(_) => {}
        }
    }
}

pub(crate) struct OwnedPeerConnectionId {
    map: Arc<Mutex<HashMap<(PublicKey, u32), u32>>>,
    peer_public_key: PublicKey,
    peer_connection_id: u32,
    connection_id: u32,
}

impl OwnedPeerConnectionId {
    pub fn new(
        transport: &TransportInner,
        peer_public_key: PublicKey,
        peer_connection_id: u32,
        connection_id: u32,
    ) -> Result<Self, Error> {
        {
            let mut map_guard = transport.peer_connection_ids.lock().unwrap();
            match map_guard.entry((peer_public_key.clone(), peer_connection_id)) {
                hash_map::Entry::Occupied(_) => {
                    return Err("Peer connection already exists".into());
                }
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(connection_id);
                }
            }
        }
        tracing::trace!("Peer connection added");
        Ok(Self {
            map: transport.peer_connection_ids.clone(),
            peer_public_key,
            peer_connection_id,
            connection_id,
        })
    }
}

impl Drop for OwnedPeerConnectionId {
    fn drop(&mut self) {
        let mut map_guard = self.map.lock().unwrap();
        match map_guard.entry((self.peer_public_key.clone(), self.peer_connection_id)) {
            hash_map::Entry::Occupied(entry) => {
                if entry.get() == &self.connection_id {
                    tracing::trace!(
                        peer_connection_id = self.peer_connection_id,
                        connection_id = self.connection_id,
                        "Removing peer connection"
                    );
                    entry.remove();
                }
                drop(map_guard);
            }
            hash_map::Entry::Vacant(_) => {}
        }
    }
}
