use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use async_trait::async_trait;
use mainline::{Dht, Id, MutableItem, SigningKey};
use ntied_crypto::PublicKey;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{ConnectionRequest, Discovery, DiscoveryFactory, Error, RawTransport};

const STUN_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun2.l.google.com:19302",
];

const STUN_MAGIC_COOKIE: u32 = 0x2112A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;

pub struct DhtDiscoveryFactory {
    bootstrap_nodes: Option<Vec<String>>,
}

impl DhtDiscoveryFactory {
    pub fn new() -> Self {
        Self {
            bootstrap_nodes: None,
        }
    }

    pub fn with_bootstrap(bootstrap_nodes: Vec<String>) -> Self {
        Self {
            bootstrap_nodes: Some(bootstrap_nodes),
        }
    }
}

#[async_trait]
impl DiscoveryFactory for DhtDiscoveryFactory {
    async fn create(&self, transport: RawTransport) -> Result<Arc<dyn Discovery>, Error> {
        Ok(Arc::new(
            DhtDiscovery::new(transport, self.bootstrap_nodes.clone()).await?,
        ))
    }
}

pub struct DhtDiscovery {
    dht: Dht,
    /// Kept for potential future use (re-publishing, dynamic updates).
    #[allow(unused)]
    signing_key: SigningKey,
    /// Kept for potential future use (re-publishing, dynamic updates).
    #[allow(unused)]
    our_info_hash: Id,
    /// Kept for potential future use.
    #[allow(unused)]
    transport: RawTransport,
    /// Our public address discovered via STUN, used for announce_peer.
    our_public_addr: Arc<RwLock<Option<SocketAddr>>>,
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<ConnectionRequest>>,
    main_task: JoinHandle<()>,
}

impl DhtDiscovery {
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);
    const STUN_TIMEOUT: Duration = Duration::from_secs(3);
    const DHT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);

    pub async fn new(
        transport: RawTransport,
        bootstrap: Option<Vec<String>>,
    ) -> Result<Self, Error> {
        let signing_key = Self::derive_signing_key(&transport)?;

        let dht = Self::create_dht(bootstrap)?;

        // Wait for DHT bootstrap (blocking in spawned task to not block async)
        let dht_clone = dht.clone();
        tokio::task::spawn_blocking(move || {
            dht_clone.bootstrapped();
        })
        .await?;

        let our_info_hash = Self::calc_our_info_hash(&transport)?;

        let (incoming_tx, incoming_rx) = mpsc::channel(100);

        let our_public_addr = Arc::new(RwLock::new(None));

        let main_task = tokio::spawn(Self::main_loop(
            dht.clone(),
            signing_key.clone(),
            our_info_hash,
            transport.clone(),
            incoming_tx,
            our_public_addr.clone(),
        ));

        Ok(Self {
            dht,
            signing_key,
            our_info_hash,
            transport,
            our_public_addr,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            main_task,
        })
    }

    fn derive_signing_key(transport: &RawTransport) -> Result<SigningKey, Error> {
        // Use public key bytes so that other peers can derive the same verifying key
        let pubkey_bytes = transport.private_key().public_key().to_bytes()?;
        let hash = Self::sha256(&pubkey_bytes);
        Ok(SigningKey::from_bytes(&hash))
    }

    fn create_dht(bootstrap: Option<Vec<String>>) -> Result<Dht, Error> {
        let mut builder = Dht::builder();
        if let Some(ref nodes) = bootstrap {
            let nodes_ref: Vec<&str> = nodes.iter().map(|s| s.as_str()).collect();
            builder.bootstrap(&nodes_ref);
        }
        Ok(builder.build()?)
    }

    fn calc_our_info_hash(transport: &RawTransport) -> Result<Id, Error> {
        let pubkey_bytes = transport.private_key().public_key().to_bytes()?;
        let pubkey_hash = Self::sha256(&pubkey_bytes);
        Ok(Self::calc_info_hash(&pubkey_hash))
    }

    fn calc_info_hash(pubkey_hash: &[u8; 32]) -> Id {
        use sha1_smol::Sha1;
        let mut hasher = Sha1::new();
        hasher.update(b"ntied:");
        hasher.update(pubkey_hash);
        Id::from_bytes(hasher.digest().bytes()).unwrap()
    }

    fn sha256(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    async fn main_loop(
        dht: Dht,
        signing_key: SigningKey,
        our_info_hash: Id,
        transport: RawTransport,
        incoming_tx: mpsc::Sender<ConnectionRequest>,
        our_public_addr_shared: Arc<RwLock<Option<SocketAddr>>>,
    ) {
        let mut seen_peers: HashSet<SocketAddrV4> = HashSet::new();
        let mut last_publish = Instant::now() - Self::PUBLISH_INTERVAL; // Force immediate publish
        let mut our_public_addr: Option<SocketAddr> = None;

        tracing::info!(?our_info_hash, "DhtDiscovery main_loop started");

        loop {
            // 1. Discover/refresh our public address and publish to DHT
            if our_public_addr.is_none() || last_publish.elapsed() >= Self::PUBLISH_INTERVAL {
                // Use fixed address if provided, otherwise discover via STUN
                let addr_result = Self::discover_public_address(&transport).await;

                match addr_result {
                    Ok(addr) => {
                        let addr_changed = our_public_addr != Some(addr);
                        let should_publish =
                            addr_changed || last_publish.elapsed() >= Self::PUBLISH_INTERVAL;

                        if should_publish {
                            if addr_changed {
                                tracing::info!(
                                    old = ?our_public_addr,
                                    new = ?addr,
                                    "Public address changed"
                                );
                            }
                            our_public_addr = Some(addr);
                            // Update shared public address for use in send_connection_request
                            *our_public_addr_shared.write().await = Some(addr);
                            if let Err(e) = Self::publish_our_address(&dht, &signing_key, addr) {
                                tracing::warn!(?e, "Failed to publish address to DHT");
                            } else {
                                tracing::debug!(?addr, "Published public address to DHT");
                            }
                            last_publish = Instant::now();
                        }
                    }
                    Err(e) => {
                        tracing::warn!(?e, "Failed to discover public address via STUN");
                    }
                }
            }

            // 2. Poll for incoming connection requests (with timeout to avoid blocking forever)
            let dht_clone = dht.clone();
            let poll_future = tokio::task::spawn_blocking(move || {
                let mut new_peers = Vec::new();
                for peer_batch in dht_clone.get_peers(our_info_hash) {
                    for addr in peer_batch {
                        new_peers.push(addr);
                    }
                    // Only process first batch to avoid blocking too long
                    break;
                }
                new_peers
            });

            let poll_result = tokio::time::timeout(Self::DHT_LOOKUP_TIMEOUT, poll_future).await;

            match poll_result {
                Ok(Ok(new_peers)) => {
                    let new_count = new_peers
                        .iter()
                        .filter(|addr| !seen_peers.contains(addr))
                        .count();
                    if new_count > 0 {
                        tracing::debug!(
                            new_count,
                            total = new_peers.len(),
                            "Polled peers from DHT"
                        );
                    }

                    for addr in new_peers {
                        if seen_peers.insert(addr) {
                            tracing::debug!(?addr, "New incoming connection request from DHT");
                            let request = ConnectionRequest {
                                socket_addr: SocketAddr::V4(addr),
                                public_key: None,
                                connection_id: None,
                            };
                            if incoming_tx.send(request).await.is_err() {
                                tracing::debug!("Incoming channel closed, stopping main_loop");
                                return;
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "DHT poll task failed");
                }
                Err(_) => {
                    tracing::trace!("DHT poll timed out, will retry");
                }
            }

            tokio::time::sleep(Self::POLL_INTERVAL).await;
        }
    }

    async fn discover_public_address(transport: &RawTransport) -> Result<SocketAddr, Error> {
        let transaction_id: [u8; 12] = rand::random();
        let request = Self::build_stun_request(&transaction_id);

        for server in STUN_SERVERS {
            // Resolve hostname to IP address using DNS lookup
            let server_addr: SocketAddr = match tokio::net::lookup_host(server).await {
                Ok(mut addrs) => match addrs.next() {
                    Some(addr) => addr,
                    None => continue,
                },
                Err(e) => {
                    tracing::trace!(?e, ?server, "Failed to resolve STUN server address");
                    continue;
                }
            };

            // Create RawConnection to receive response routed by Transport's main_loop
            let raw_conn = match transport.connect(server_addr) {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::trace!(?e, ?server, "Failed to create raw connection for STUN");
                    continue;
                }
            };

            if let Err(e) = raw_conn.send(request.clone()).await {
                tracing::trace!(?e, ?server, "Failed to send STUN request");
                continue;
            }

            let recv_result = tokio::time::timeout(Self::STUN_TIMEOUT, raw_conn.recv()).await;

            match recv_result {
                Ok(Ok(data)) => {
                    if let Some(addr) = Self::parse_stun_response(&data, &transaction_id) {
                        tracing::debug!(?addr, ?server, "Discovered public address via STUN");
                        return Ok(addr);
                    }
                }
                Ok(Err(e)) => {
                    tracing::trace!(?e, ?server, "Failed to receive STUN response");
                }
                Err(_) => {
                    tracing::trace!(?server, "STUN request timed out");
                }
            }
        }

        Err("Failed to discover public address from any STUN server".into())
    }

    fn build_stun_request(transaction_id: &[u8; 12]) -> Vec<u8> {
        let mut request = Vec::with_capacity(20);
        request.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
        request.extend_from_slice(&0u16.to_be_bytes()); // Message length = 0 (no attributes)
        request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        request.extend_from_slice(transaction_id);
        request
    }

    fn parse_stun_response(data: &[u8], expected_tid: &[u8; 12]) -> Option<SocketAddr> {
        if data.len() < 20 {
            return None;
        }

        let msg_type = u16::from_be_bytes([data[0], data[1]]);
        if msg_type != STUN_BINDING_RESPONSE {
            return None;
        }

        let magic = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        if magic != STUN_MAGIC_COOKIE {
            return None;
        }

        if &data[8..20] != expected_tid {
            return None;
        }

        let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
        if data.len() < 20 + msg_len {
            return None;
        }

        let mut pos = 20;
        while pos + 4 <= data.len() {
            let attr_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
            let attr_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
            pos += 4;

            if pos + attr_len > data.len() {
                break;
            }

            if attr_type == STUN_ATTR_XOR_MAPPED_ADDRESS && attr_len >= 8 {
                let family = data[pos + 1];
                if family == 0x01 {
                    // IPv4
                    let xor_port = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                    let port = xor_port ^ (STUN_MAGIC_COOKIE >> 16) as u16;

                    let xor_ip = u32::from_be_bytes([
                        data[pos + 4],
                        data[pos + 5],
                        data[pos + 6],
                        data[pos + 7],
                    ]);
                    let ip = xor_ip ^ STUN_MAGIC_COOKIE;

                    return Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)));
                }
            } else if attr_type == STUN_ATTR_MAPPED_ADDRESS && attr_len >= 8 {
                let family = data[pos + 1];
                if family == 0x01 {
                    // IPv4
                    let port = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                    let ip =
                        Ipv4Addr::new(data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]);
                    return Some(SocketAddr::V4(SocketAddrV4::new(ip, port)));
                }
            }

            // Align to 4-byte boundary
            pos += (attr_len + 3) & !3;
        }

        None
    }

    fn publish_our_address(
        dht: &Dht,
        signing_key: &SigningKey,
        addr: SocketAddr,
    ) -> Result<(), Error> {
        let addr_v4 = match addr {
            SocketAddr::V4(a) => a,
            SocketAddr::V6(_) => return Err("IPv6 not supported".into()),
        };

        let value = Self::encode_socket_addr(addr_v4);
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let item = MutableItem::new(signing_key.clone(), &value, seq, Some(b"ntied:addr"));
        dht.put_mutable(item, None)?;

        Ok(())
    }

    fn encode_socket_addr(addr: SocketAddrV4) -> Vec<u8> {
        let mut buf = Vec::with_capacity(6);
        buf.extend_from_slice(&addr.ip().octets());
        buf.extend_from_slice(&addr.port().to_be_bytes());
        buf
    }

    fn decode_socket_addr(data: &[u8]) -> Result<SocketAddrV4, Error> {
        if data.len() < 6 {
            return Err("Invalid address data: too short".into());
        }
        let ip = std::net::Ipv4Addr::new(data[0], data[1], data[2], data[3]);
        let port = u16::from_be_bytes([data[4], data[5]]);
        Ok(SocketAddrV4::new(ip, port))
    }

    fn get_target_info_hash(public_key: &PublicKey) -> Result<Id, Error> {
        let pubkey_bytes = public_key.to_bytes()?;
        let pubkey_hash = Self::sha256(&pubkey_bytes);
        Ok(Self::calc_info_hash(&pubkey_hash))
    }

    fn get_target_ed25519_pubkey(public_key: &PublicKey) -> Result<[u8; 32], Error> {
        // Derive the same signing key that the target would use, then get its verifying key
        let pubkey_bytes = public_key.to_bytes()?;
        let hash = Self::sha256(&pubkey_bytes);
        let signing_key = SigningKey::from_bytes(&hash);
        Ok(signing_key.verifying_key().to_bytes())
    }
}

#[async_trait]
impl Discovery for DhtDiscovery {
    async fn send_connection_request(
        &self,
        public_key: &PublicKey,
        _connection_id: u32,
    ) -> Result<SocketAddr, Error> {
        let target_info_hash = Self::get_target_info_hash(public_key)?;
        let target_ed25519_pubkey = Self::get_target_ed25519_pubkey(public_key)?;

        tracing::debug!(?target_info_hash, "Looking up target address in DHT");

        // 1. Get target's address from DHT (with timeout)
        let dht = self.dht.clone();
        let lookup_result = tokio::time::timeout(
            Self::DHT_LOOKUP_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                dht.get_mutable(&target_ed25519_pubkey, Some(b"ntied:addr"), None)
                    .next()
            }),
        )
        .await;

        let target_addr = match lookup_result {
            Ok(Ok(Some(item))) => item,
            Ok(Ok(None)) => return Err("Target not found in DHT".into()),
            Ok(Err(e)) => return Err(format!("DHT lookup task failed: {}", e).into()),
            Err(_) => return Err("DHT lookup timed out".into()),
        };

        let peer_addr = Self::decode_socket_addr(target_addr.value())?;

        tracing::debug!(
            ?peer_addr,
            ?target_info_hash,
            "Found target address, announcing ourselves"
        );

        // 2. Announce ourselves to target's info_hash so they can find us
        // Use the public port from STUN, not the local port
        let dht = self.dht.clone();
        let our_public_addr = self.our_public_addr.read().await;
        let our_port = our_public_addr.map(|addr| addr.port());
        drop(our_public_addr); // Release lock before blocking call
        let announce_result =
            tokio::task::spawn_blocking(move || dht.announce_peer(target_info_hash, our_port))
                .await;

        match announce_result {
            Ok(Ok(_)) => {
                tracing::debug!(?peer_addr, "Successfully announced to target");
            }
            Ok(Err(e)) => {
                tracing::warn!(?e, "Failed to announce peer, continuing anyway");
            }
            Err(e) => {
                tracing::warn!(?e, "Announce task failed, continuing anyway");
            }
        }

        Ok(SocketAddr::V4(peer_addr))
    }

    async fn recv_connection_request(&self) -> Result<ConnectionRequest, Error> {
        let request = self
            .incoming_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| -> Error { "Discovery channel closed".into() })?;

        tracing::debug!(
            peer_addr = ?request.socket_addr,
            "Received incoming connection request"
        );

        Ok(request)
    }
}

impl Drop for DhtDiscovery {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

pub fn get_dht_key_for_public_key(public_key: &PublicKey) -> Result<[u8; 32], Error> {
    DhtDiscovery::get_target_ed25519_pubkey(public_key)
}
