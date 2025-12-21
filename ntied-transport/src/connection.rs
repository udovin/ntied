use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ntied_crypto::{EphemeralKeyPair, PublicKey, SharedSecret};
use rand::Rng as _;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval, sleep_until};

use crate::byteio::Writer;
use crate::{
    DataPacket, DecryptedPacket, EncryptedPacket, EncryptionEpoch, Error, HandshakeAckPacket,
    HandshakePacket, HeartbeatPacket, HolePunchPacket, OwnedPeerConnectionId, OwnedPeerSocketAddr,
    Packet, RotatePacket, TransportInner,
};

pub struct Connection {
    transport: Arc<TransportInner>,
    connection_id: u32,
    peer_connection_id: u32,
    peer_addr: Arc<RwLock<SocketAddr>>,
    peer_public_key: PublicKey,
    encryption_state: Arc<Mutex<EncryptionState>>,
    data_rx: TokioMutex<mpsc::Receiver<Vec<u8>>>,
    main_task: JoinHandle<()>,
    #[allow(dead_code)]
    owned_peer_connection_id: Option<OwnedPeerConnectionId>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let peer_addr = *self.peer_addr.read().unwrap();
        tracing::trace!(
            connection_id = self.connection_id,
            peer_connection_id = self.peer_connection_id,
            ?peer_addr,
            peer_public_key = ?self.peer_public_key,
            "Dropping connection",
        );
        self.main_task.abort();
        let mut connections_guard = self.transport.connections.write().unwrap();
        if connections_guard.remove(&self.connection_id).is_none() {
            tracing::error!(
                connection_id = self.connection_id,
                peer_connection_id = self.peer_connection_id,
                ?peer_addr,
                peer_public_key = ?self.peer_public_key,
                "Inconsistent connection drop: Connection not found",
            );
        }
    }
}

impl Connection {
    const MAX_PACKETS: usize = 4;
    const HANDSHAKE_INTERVAL: Duration = Duration::from_millis(100);
    const HANDSHAKE_TRIES: usize = 20;
    const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(750);
    const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
    const ROTATE_INTERVAL: Duration = Duration::from_mins(15);

    pub(crate) async fn connect(
        transport: Arc<TransportInner>,
        connection_id: u32,
        mut peer_addr: SocketAddr,
        peer_public_key: PublicKey,
        mut packet_rx: mpsc::Receiver<(SocketAddr, Packet)>,
    ) -> Result<Self, Error> {
        let peer_verifying_key = peer_public_key.verifying_key();
        let mut encryption_state = EncryptionState::new();
        let (data_tx, data_rx) = mpsc::channel(Self::MAX_PACKETS);
        let data_rx = TokioMutex::new(data_rx);
        let handshake_task = async {
            for _ in 0..Self::HANDSHAKE_TRIES {
                let handshake = {
                    let public_key = transport
                        .private_key
                        .public_key()
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let peer_public_key = peer_public_key
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let ephemeral_public_key =
                        encryption_state.ephemeral_keypair.public_key_bytes();
                    let mut packet_bytes = Vec::new();
                    let mut packet_writer = Writer::new(&mut packet_bytes);
                    packet_writer.write_u32(connection_id);
                    packet_writer.write_bytes(&peer_public_key);
                    packet_writer.write_bytes(&public_key);
                    packet_writer.write_bytes(&ephemeral_public_key);
                    Packet::Handshake(HandshakePacket {
                        connection_id,
                        peer_public_key,
                        public_key,
                        ephemeral_public_key,
                        signature: transport.private_key.sign(packet_bytes),
                    })
                };
                let packet = handshake.serialize();
                tracing::trace!(addr = ?peer_addr, "Sending handshake packet");
                if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                    tracing::warn!(?err, "Failed to send handshake");
                }
                tokio::time::sleep(Self::HANDSHAKE_INTERVAL).await;
            }
        };
        let peer_connection_id = tokio::select! {
            _ = handshake_task => {
                return Err("Handshake failed".into());
            },
            v = packet_rx.recv() => {
                let (addr, packet) = match v {
                    Some(v) => v,
                    None => return Err("Handshake failed".into()),
                };
                peer_addr = addr;
                match packet {
                    Packet::HandshakeAck(handshake_ack_package) => {
                        let public_key = match PublicKey::from_bytes(&handshake_ack_package.public_key) {
                            Ok(pk) => pk,
                            Err(err) => {
                                tracing::warn!(?err, "Invalid public key in handshake ack");
                                return Err("Invalid public key".into());
                            }
                        };
                        if public_key != peer_public_key {
                            tracing::warn!("Public key mismatch in handshake ack");
                            return Err("Public key mismatch".into());
                        }
                        let mut packet_bytes = Vec::new();
                        let mut packet_writer = Writer::new(&mut packet_bytes);
                        packet_writer.write_u32(handshake_ack_package.peer_connection_id);
                        packet_writer.write_u32(handshake_ack_package.connection_id);
                        packet_writer.write_bytes(&handshake_ack_package.peer_public_key);
                        packet_writer.write_bytes(&handshake_ack_package.public_key);
                        packet_writer.write_bytes(&handshake_ack_package.ephemeral_public_key);
                        if !peer_verifying_key
                            .verify(&packet_bytes, &handshake_ack_package.signature)
                            .unwrap_or(false)
                        {
                            tracing::warn!("Invalid signature in handshake ack");
                            return Err("Invalid signature".into());
                        }
                        let shared_secret = match encryption_state
                            .ephemeral_keypair
                            .compute_shared_secret(&handshake_ack_package.ephemeral_public_key)
                        {
                            Ok(secret) => secret,
                            Err(err) => {
                                tracing::warn!(?err, "Failed to compute shared secret");
                                return Err("Failed to compute shared secret".into());
                            }
                        };
                        encryption_state.shared_secret = Some(shared_secret);
                        encryption_state.epoch = EncryptionEpoch::new(1);
                        handshake_ack_package.connection_id
                    }
                    _ => {
                        return Err("Unexpected packet".into());
                    }
                }
            }
        };
        let peer_addr = Arc::new(RwLock::new(peer_addr));
        let encryption_state = Arc::new(Mutex::new(encryption_state));
        let main_task = tokio::spawn(Self::main_loop(
            packet_rx,
            data_tx,
            encryption_state.clone(),
            peer_public_key.clone(),
            peer_addr.clone(),
            transport.clone(),
            peer_connection_id,
        ));
        Ok(Self {
            transport,
            connection_id,
            peer_connection_id,
            peer_addr,
            peer_public_key,
            encryption_state,
            data_rx,
            main_task,
            owned_peer_connection_id: None,
        })
    }

    pub(crate) async fn accept(
        transport: Arc<TransportInner>,
        connection_id: u32,
        peer_connection_id: u32,
        mut peer_addr: SocketAddr,
        peer_public_key: PublicKey,
        mut packet_rx: mpsc::Receiver<(SocketAddr, Packet)>,
        owned_peer_connection_id: OwnedPeerConnectionId,
    ) -> Result<Connection, Error> {
        let peer_verifying_key = peer_public_key.verifying_key();
        let mut encryption_state = EncryptionState::new();
        let (data_tx, data_rx) = mpsc::channel(Self::MAX_PACKETS);
        let data_rx = TokioMutex::new(data_rx);
        let handshake_ack_task = async {
            for _ in 0..Self::HANDSHAKE_TRIES {
                let handshake_ack = {
                    let public_key = transport
                        .private_key
                        .public_key()
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let peer_public_key = peer_public_key
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let ephemeral_public_key =
                        encryption_state.ephemeral_keypair.public_key_bytes();
                    let mut packet_bytes = Vec::new();
                    let mut packet_writer = Writer::new(&mut packet_bytes);
                    packet_writer.write_u32(peer_connection_id);
                    packet_writer.write_u32(connection_id);
                    packet_writer.write_bytes(&peer_public_key);
                    packet_writer.write_bytes(&public_key);
                    packet_writer.write_bytes(&ephemeral_public_key);
                    Packet::HandshakeAck(HandshakeAckPacket {
                        peer_connection_id,
                        connection_id,
                        public_key,
                        peer_public_key,
                        ephemeral_public_key,
                        signature: transport.private_key.sign(packet_bytes),
                    })
                };
                let packet = handshake_ack.serialize();
                tracing::trace!(
                    connection_id,
                    peer_connection_id,
                    ?peer_addr,
                    "Sending handshake ack packet",
                );
                if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                    tracing::warn!(
                        ?err,
                        connection_id,
                        peer_connection_id,
                        ?peer_addr,
                        "Failed to send handshake ack",
                    );
                }
                tokio::time::sleep(Self::HANDSHAKE_INTERVAL).await;
            }
        };
        tokio::select! {
            _ = handshake_ack_task => {
                return Err("Handshake failed".into());
            },
            v = packet_rx.recv() => {
                let (addr, packet) = match v {
                    Some(v) => v,
                    None => return Err("Handshake failed".into()),
                };
                peer_addr = addr;
                match packet {
                    Packet::Handshake(handshake_package) => {
                        let public_key = match PublicKey::from_bytes(&handshake_package.public_key) {
                            Ok(pk) => pk,
                            Err(err) => {
                                tracing::warn!(?err, "Invalid public key in handshake ack");
                                return Err("Invalid public key".into());
                            }
                        };
                        if public_key != peer_public_key {
                            tracing::warn!("Public key mismatch in handshake ack");
                            return Err("Public key mismatch".into());
                        }
                        let mut packet_bytes = Vec::new();
                        let mut packet_writer = Writer::new(&mut packet_bytes);
                        packet_writer.write_u32(handshake_package.connection_id);
                        packet_writer.write_bytes(&handshake_package.peer_public_key);
                        packet_writer.write_bytes(&handshake_package.public_key);
                        packet_writer.write_bytes(&handshake_package.ephemeral_public_key);
                        if !peer_verifying_key
                            .verify(&packet_bytes, &handshake_package.signature)
                            .unwrap_or(false)
                        {
                            tracing::warn!("Invalid signature in handshake ack");
                            return Err("Invalid signature".into());
                        }
                        let shared_secret = match encryption_state
                            .ephemeral_keypair
                            .compute_shared_secret(&handshake_package.ephemeral_public_key)
                        {
                            Ok(secret) => secret,
                            Err(err) => {
                                tracing::warn!(?err, "Failed to compute shared secret");
                                return Err("Failed to compute shared secret".into());
                            }
                        };
                        encryption_state.shared_secret = Some(shared_secret);
                        encryption_state.epoch = EncryptionEpoch::new(1);
                    }
                    _ => {
                        return Err("Unexpected packet".into());
                    }
                }
            }
        };
        let peer_addr = Arc::new(RwLock::new(peer_addr));
        let encryption_state = Arc::new(Mutex::new(encryption_state));
        let main_task = tokio::spawn(Self::main_loop(
            packet_rx,
            data_tx,
            encryption_state.clone(),
            peer_public_key.clone(),
            peer_addr.clone(),
            transport.clone(),
            peer_connection_id,
        ));
        Ok(Self {
            transport,
            connection_id,
            peer_connection_id,
            peer_addr,
            peer_public_key,
            encryption_state,
            data_rx,
            main_task,
            owned_peer_connection_id: Some(owned_peer_connection_id),
        })
    }

    /// Accept connection when we only know peer's address (no public_key/connection_id).
    /// Uses HolePunch packets to traverse NAT before receiving Handshake.
    pub(crate) async fn accept_with_holepunch(
        transport: Arc<TransportInner>,
        connection_id: u32,
        mut peer_addr: SocketAddr,
        mut packet_rx: mpsc::Receiver<(SocketAddr, Packet)>,
        owned_peer_socket_addr: OwnedPeerSocketAddr,
    ) -> Result<Connection, Error> {
        let mut encryption_state = EncryptionState::new();
        let (data_tx, data_rx) = mpsc::channel(Self::MAX_PACKETS);
        let data_rx = TokioMutex::new(data_rx);

        // Phase 1: Send HolePunch packets until we receive Handshake
        let our_public_key = transport
            .private_key
            .public_key()
            .to_bytes()
            .expect("Failed to serialize public key");

        let holepunch_task = async {
            loop {
                let holepunch = Packet::HolePunch(HolePunchPacket {
                    public_key: our_public_key.clone(),
                });
                let packet = holepunch.serialize();
                tracing::trace!(connection_id, ?peer_addr, "Sending hole punch packet");
                if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                    tracing::warn!(?err, connection_id, ?peer_addr, "Failed to send hole punch");
                }
                tokio::time::sleep(Self::HANDSHAKE_INTERVAL).await;
            }
        };

        // Wait for Handshake packet
        let (peer_public_key, peer_connection_id, peer_ephemeral_public_key) = tokio::select! {
            _ = holepunch_task => {
                unreachable!("HolePunch task should never complete")
            },
            v = packet_rx.recv() => {
                let (addr, packet) = match v {
                    Some(v) => v,
                    None => return Err("Connection closed while waiting for handshake".into()),
                };
                peer_addr = addr;
                match packet {
                    Packet::Handshake(handshake) => {
                        let public_key = PublicKey::from_bytes(&handshake.public_key)
                            .map_err(|_| "Invalid public key in handshake")?;
                        let verifying_key = public_key.verifying_key();

                        // Verify signature
                        let mut packet_bytes = Vec::new();
                        let mut packet_writer = Writer::new(&mut packet_bytes);
                        packet_writer.write_u32(handshake.connection_id);
                        packet_writer.write_bytes(&handshake.peer_public_key);
                        packet_writer.write_bytes(&handshake.public_key);
                        packet_writer.write_bytes(&handshake.ephemeral_public_key);
                        if !verifying_key
                            .verify(&packet_bytes, &handshake.signature)
                            .unwrap_or(false)
                        {
                            return Err("Invalid signature in handshake".into());
                        }

                        (public_key, handshake.connection_id, handshake.ephemeral_public_key)
                    }
                    Packet::HolePunch(_) => {
                        // Ignore HolePunch from peer, continue waiting
                        return Err("Received HolePunch instead of Handshake".into());
                    }
                    _ => {
                        return Err("Unexpected packet while waiting for handshake".into());
                    }
                }
            }
        };

        // Upgrade peer_socket_addr mapping to peer_connection_id mapping
        let owned_peer_connection_id = owned_peer_socket_addr.upgrade(
            &transport,
            peer_public_key.clone(),
            peer_connection_id,
        )?;

        let peer_verifying_key = peer_public_key.verifying_key();

        // Phase 2: Send HandshakeAck packets (same as regular accept)
        let handshake_ack_task = async {
            for _ in 0..Self::HANDSHAKE_TRIES {
                let handshake_ack = {
                    let public_key = transport
                        .private_key
                        .public_key()
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let peer_public_key_bytes = peer_public_key
                        .to_bytes()
                        .expect("Failed to serialize public key");
                    let ephemeral_public_key =
                        encryption_state.ephemeral_keypair.public_key_bytes();
                    let mut packet_bytes = Vec::new();
                    let mut packet_writer = Writer::new(&mut packet_bytes);
                    packet_writer.write_u32(peer_connection_id);
                    packet_writer.write_u32(connection_id);
                    packet_writer.write_bytes(&peer_public_key_bytes);
                    packet_writer.write_bytes(&public_key);
                    packet_writer.write_bytes(&ephemeral_public_key);
                    Packet::HandshakeAck(HandshakeAckPacket {
                        peer_connection_id,
                        connection_id,
                        public_key,
                        peer_public_key: peer_public_key_bytes,
                        ephemeral_public_key,
                        signature: transport.private_key.sign(packet_bytes),
                    })
                };
                let packet = handshake_ack.serialize();
                tracing::trace!(
                    connection_id,
                    peer_connection_id,
                    ?peer_addr,
                    "Sending handshake ack packet"
                );
                if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                    tracing::warn!(
                        ?err,
                        connection_id,
                        peer_connection_id,
                        ?peer_addr,
                        "Failed to send handshake ack"
                    );
                }
                tokio::time::sleep(Self::HANDSHAKE_INTERVAL).await;
            }
        };

        // Wait for repeated Handshake (confirmation) while sending HandshakeAck
        tokio::select! {
            _ = handshake_ack_task => {
                return Err("Handshake failed - no confirmation received".into());
            },
            v = packet_rx.recv() => {
                let (addr, packet) = match v {
                    Some(v) => v,
                    None => return Err("Handshake failed".into()),
                };
                peer_addr = addr;
                match packet {
                    Packet::Handshake(handshake) => {
                        // Verify it's from the same peer
                        let public_key = PublicKey::from_bytes(&handshake.public_key)
                            .map_err(|_| "Invalid public key in handshake")?;
                        if public_key != peer_public_key {
                            return Err("Public key mismatch in repeated handshake".into());
                        }
                        // Verify signature
                        let mut packet_bytes = Vec::new();
                        let mut packet_writer = Writer::new(&mut packet_bytes);
                        packet_writer.write_u32(handshake.connection_id);
                        packet_writer.write_bytes(&handshake.peer_public_key);
                        packet_writer.write_bytes(&handshake.public_key);
                        packet_writer.write_bytes(&handshake.ephemeral_public_key);
                        if !peer_verifying_key
                            .verify(&packet_bytes, &handshake.signature)
                            .unwrap_or(false)
                        {
                            return Err("Invalid signature in handshake".into());
                        }
                        // Compute shared secret
                        let shared_secret = encryption_state
                            .ephemeral_keypair
                            .compute_shared_secret(&peer_ephemeral_public_key)
                            .map_err(|_| "Failed to compute shared secret")?;
                        encryption_state.shared_secret = Some(shared_secret);
                        encryption_state.epoch = EncryptionEpoch::new(1);
                    }
                    _ => {
                        return Err("Unexpected packet".into());
                    }
                }
            }
        };

        let peer_addr = Arc::new(RwLock::new(peer_addr));
        let encryption_state = Arc::new(Mutex::new(encryption_state));
        let main_task = tokio::spawn(Self::main_loop(
            packet_rx,
            data_tx,
            encryption_state.clone(),
            peer_public_key.clone(),
            peer_addr.clone(),
            transport.clone(),
            peer_connection_id,
        ));
        Ok(Self {
            transport,
            connection_id,
            peer_connection_id,
            peer_addr,
            peer_public_key,
            encryption_state,
            data_rx,
            main_task,
            owned_peer_connection_id: Some(owned_peer_connection_id),
        })
    }

    pub async fn send(&self, data: impl Into<Vec<u8>>) -> Result<(), Error> {
        if self.main_task.is_finished() {
            return Err("Connection closed".into());
        }
        let decrypted_packet = DecryptedPacket::Data(DataPacket { data: data.into() });
        let encrypted_packet = {
            let mut state = self.encryption_state.lock().unwrap();
            if state.shared_secret.is_none() {
                return Err("Connection is not established yet".into());
            }
            let nonce = state.generate_nonce();
            let (epoch, shared_secret) = if let Some(shared_secret) = &state.next_shared_secret {
                (state.epoch.next(), shared_secret)
            } else {
                (state.epoch, state.shared_secret.as_ref().unwrap())
            };
            EncryptedPacket::encrypt(
                self.peer_connection_id,
                decrypted_packet,
                epoch,
                shared_secret,
                nonce,
            )?
        };
        let packet = Packet::Encrypted(encrypted_packet).serialize();
        let peer_addr = *self.peer_addr.read().unwrap();
        tracing::trace!(
            peer_connection_id = self.peer_connection_id,
            ?peer_addr,
            packet_len = packet.len(),
            "Sending packet to peer"
        );
        self.transport.socket.send_to(&packet, peer_addr).await?;
        Ok(())
    }

    pub async fn recv(&self) -> Result<Vec<u8>, Error> {
        self.data_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Connection closed".into())
    }

    pub fn peer_addr(&self) -> SocketAddr {
        *self.peer_addr.read().unwrap()
    }

    pub fn peer_public_key(&self) -> &PublicKey {
        &self.peer_public_key
    }

    async fn main_loop(
        mut packet_rx: mpsc::Receiver<(SocketAddr, Packet)>,
        data_tx: mpsc::Sender<Vec<u8>>,
        encryption_state: Arc<Mutex<EncryptionState>>,
        peer_public_key: PublicKey,
        peer_addr: Arc<RwLock<SocketAddr>>,
        transport: Arc<TransportInner>,
        peer_connection_id: u32,
    ) {
        let peer_verifying_key = peer_public_key.verifying_key();
        let mut last_heartbeat = Instant::now();
        let mut heartbeat_interval = interval(Self::HEARTBEAT_INTERVAL);
        let mut rotate_interval = interval(Self::ROTATE_INTERVAL);
        loop {
            let deadline = last_heartbeat + Self::CONNECTION_TIMEOUT;
            tokio::select! {
                _ = sleep_until(deadline) => {
                    tracing::debug!("Closing connection due to timeout");
                    return;
                }
                _ = heartbeat_interval.tick() => {
                    tracing::debug!("Sending heartbeat");
                    let peer_addr = *peer_addr.read().unwrap();
                    let encrypted = {
                        let mut state = encryption_state.lock().unwrap();
                        let heartbeat = DecryptedPacket::Heartbeat(HeartbeatPacket {});
                        let nonce = state.generate_nonce();
                        let (epoch, shared_secret) = if let Some(shared_secret) = &state.next_shared_secret {
                            (state.epoch.next(), shared_secret)
                        } else {
                            (state.epoch, state.shared_secret.as_ref().unwrap())
                        };
                        match EncryptedPacket::encrypt(
                            peer_connection_id,
                            heartbeat,
                            epoch,
                            shared_secret,
                            nonce,
                        ) {
                            Ok(v) => v,
                            Err(err) => {
                                tracing::warn!(?err, "Failed to encrypt heartbeat ack");
                                continue;
                            }
                        }
                    };
                    let packet = Packet::Encrypted(encrypted).serialize();
                    if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                        tracing::warn!(?err, "Failed to send heartbeat");
                    }
                }
                _ = rotate_interval.tick() => {
                    tracing::debug!("Starting rotation");
                    let peer_addr = *peer_addr.read().unwrap();
                    let encrypted = {
                        let mut state = encryption_state.lock().unwrap();
                        // Generate next keypair if needed
                        if state.next_ephemeral_keypair.is_none() {
                            state.next_ephemeral_keypair = Some(EphemeralKeyPair::generate());
                        }
                        let next_keypair = state.next_ephemeral_keypair.as_ref().unwrap();
                        let next_public_key = next_keypair.public_key_bytes();
                        // Send Rotate message
                        let rotate = DecryptedPacket::Rotate(RotatePacket {
                            ephemeral_public_key: next_public_key.clone(),
                            signature: transport.private_key.sign(&next_public_key),
                        });
                        let nonce = state.generate_nonce();
                        match EncryptedPacket::encrypt(
                            peer_connection_id,
                            rotate,
                            state.epoch,
                            state.shared_secret.as_ref().unwrap(),
                            nonce,
                        ) {
                            Ok(v) => v,
                            Err(err) => {
                                tracing::warn!(?err, "Failed to encrypt rotate message");
                                continue;
                            }
                        }
                    };
                    let packet = Packet::Encrypted(encrypted).serialize();
                    if let Err(err) = transport.socket.send_to(&packet, peer_addr).await {
                        tracing::warn!(?err, "Failed to send rotate message");
                    }
                }
                v = packet_rx.recv() => {
                    let (addr, packet) = match v {
                        Some(v) => v,
                        None => return,
                    };
                    tracing::trace!(peer_addr = ?addr, "Received packet");
                    match packet {
                        Packet::HolePunch(_) => {
                            tracing::trace!("Ignoring hole punch packet");
                        }
                        Packet::Handshake(_) => {
                            tracing::warn!("Ignoring handshake packet");
                        }
                        Packet::HandshakeAck(_) => {
                            tracing::warn!("Ignoring handshake ack packet");
                        }
                        Packet::Encrypted(encrypted_msg) => {
                            tracing::trace!("Received encrypted packet");
                            let decrypted = {
                                let mut state = encryption_state.lock().unwrap();
                                if encrypted_msg.epoch == state.epoch.next() {
                                    if let Some(shared_secret) = state.next_shared_secret.take() {
                                        tracing::debug!(
                                            epoch = encrypted_msg.epoch.as_u8(),
                                            "Completing rotation"
                                        );
                                        state.ephemeral_keypair = state.next_ephemeral_keypair.take().unwrap();
                                        state.shared_secret = Some(shared_secret);
                                        state.epoch = encrypted_msg.epoch;
                                        // Reset the rotation interval.
                                        rotate_interval.reset();
                                    } else {
                                        tracing::warn!("Missing shared secret for next epoch");
                                    }
                                }
                                if encrypted_msg.epoch != state.epoch {
                                    tracing::warn!("Invalid epoch in encrypted message");
                                    continue;
                                }
                                let shared_secret = match &state.shared_secret {
                                    Some(secret) => secret,
                                    None => {
                                        tracing::warn!(
                                            epoch = encrypted_msg.epoch.as_u8(),
                                            "No shared secret for epoch",
                                        );
                                        continue;
                                    }
                                };
                                match encrypted_msg.decrypt(shared_secret) {
                                    Ok(msg) => msg,
                                    Err(err) => {
                                        tracing::warn!(?err, "Failed to decrypt message");
                                        continue;
                                    }
                                }
                            };
                            // Update last heartbeat
                            last_heartbeat = Instant::now();
                            *peer_addr.write().unwrap() = addr;
                            match decrypted {
                                DecryptedPacket::Data(data_msg) => {
                                    if let Err(err) = data_tx.try_send(data_msg.data) {
                                        tracing::warn!(?err, "Received data packet is lost");
                                    }
                                }
                                DecryptedPacket::Rotate(rotate_msg) => {
                                    // Verify signature
                                    if !peer_verifying_key
                                        .verify(&rotate_msg.ephemeral_public_key, &rotate_msg.signature)
                                        .unwrap_or(false)
                                    {
                                        tracing::warn!("Invalid signature in rotate message");
                                        continue;
                                    }
                                    // Reset the rotation interval.
                                    rotate_interval.reset();
                                    let encrypted = {
                                        // Generate next keypair if needed
                                        let mut state = encryption_state.lock().unwrap();
                                        if state.next_ephemeral_keypair.is_none() {
                                            state.next_ephemeral_keypair = Some(EphemeralKeyPair::generate());
                                        }
                                        // Compute next shared secret
                                        let next_keypair = state.next_ephemeral_keypair.as_ref().unwrap();
                                        let next_public_key = next_keypair.public_key_bytes();
                                        let next_secret = match next_keypair
                                            .compute_shared_secret(&rotate_msg.ephemeral_public_key)
                                        {
                                            Ok(secret) => secret,
                                            Err(err) => {
                                                tracing::warn!(?err, "Failed to compute next shared secret");
                                                continue;
                                            }
                                        };
                                        state.next_shared_secret = Some(next_secret);
                                        // Send RotateAck
                                        let rotate_ack = DecryptedPacket::RotateAck(RotatePacket {
                                            ephemeral_public_key: next_public_key.clone(),
                                            signature: transport.private_key.sign(&next_public_key),
                                        });
                                        let nonce = state.generate_nonce();
                                        match EncryptedPacket::encrypt(
                                            peer_connection_id,
                                            rotate_ack,
                                            state.epoch,
                                            state.shared_secret.as_ref().unwrap(),
                                            nonce,
                                        ) {
                                            Ok(msg) => msg,
                                            Err(err) => {
                                                tracing::warn!(?err, "Failed to encrypt rotate ack");
                                                continue;
                                            }
                                        }
                                    };
                                    let packet = Packet::Encrypted(encrypted).serialize();
                                    if let Err(err) = transport.socket.send_to(&packet, addr).await {
                                        tracing::warn!(?err, "Failed to send rotate ack");
                                    }
                                }
                                DecryptedPacket::RotateAck(rotate_ack_msg) => {
                                    // Verify signature
                                    if !peer_verifying_key
                                        .verify(
                                            &rotate_ack_msg.ephemeral_public_key,
                                            &rotate_ack_msg.signature,
                                        )
                                        .unwrap_or(false)
                                    {
                                        tracing::warn!("Invalid signature in rotate ack message");
                                        continue;
                                    }
                                    // Reset the rotation interval.
                                    rotate_interval.reset();
                                    let mut state = encryption_state.lock().unwrap();
                                    if let Some(next_keypair) = &state.next_ephemeral_keypair {
                                        // Compute next shared secret
                                        let next_secret = match next_keypair
                                            .compute_shared_secret(&rotate_ack_msg.ephemeral_public_key)
                                        {
                                            Ok(secret) => secret,
                                            Err(err) => {
                                                tracing::warn!(?err, "Failed to compute next shared secret");
                                                continue;
                                            }
                                        };
                                        state.next_shared_secret = Some(next_secret);
                                    } else {
                                        tracing::warn!("Received rotate ack without next ephemeral keypair");
                                    }
                                }
                                DecryptedPacket::Heartbeat(_) => {
                                    // Send HeartbeatAck
                                    let encrypted = {
                                        let mut state = encryption_state.lock().unwrap();
                                        let heartbeat_ack = DecryptedPacket::HeartbeatAck(HeartbeatPacket {});
                                        let nonce = state.generate_nonce();
                                        match EncryptedPacket::encrypt(
                                            peer_connection_id,
                                            heartbeat_ack,
                                            state.epoch,
                                            state.shared_secret.as_ref().unwrap(),
                                            nonce,
                                        ) {
                                            Ok(v) => v,
                                            Err(err) => {
                                                tracing::warn!(?err, "Failed to encrypt heartbeat ack");
                                                continue;
                                            }
                                        }
                                    };
                                    let packet = Packet::Encrypted(encrypted).serialize();
                                    if let Err(err) = transport.socket.send_to(&packet, addr).await {
                                        tracing::warn!(?err, "Failed to send heartbeat ack");
                                    }
                                }
                                DecryptedPacket::HeartbeatAck(_) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

struct EncryptionState {
    epoch: EncryptionEpoch,
    ephemeral_keypair: EphemeralKeyPair,
    shared_secret: Option<SharedSecret>,
    next_ephemeral_keypair: Option<EphemeralKeyPair>,
    next_shared_secret: Option<SharedSecret>,
    nonce_counter: u64,
}

impl EncryptionState {
    fn new() -> Self {
        Self {
            epoch: EncryptionEpoch::new(0),
            ephemeral_keypair: EphemeralKeyPair::generate(),
            shared_secret: None,
            next_ephemeral_keypair: None,
            next_shared_secret: None,
            nonce_counter: 0,
        }
    }

    fn generate_nonce(&mut self) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.nonce_counter.to_le_bytes());
        self.nonce_counter += 1;
        rand::thread_rng().fill(&mut nonce[8..]);
        nonce
    }
}
