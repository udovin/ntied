use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rand::{RngCore, thread_rng};
use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, trace, warn};

const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

use crate::connection::Connection as InnerConnection;
use crate::crypto::{EncryptionKeys, KemPrivateKey, PrivateKey, compute_transcript_hash};
use crate::session::{Role, Session};
use crate::wire::{Init, InitAck, Packet};

pub struct Node {
    socket: Arc<UdpSocket>,
    identity: Arc<PrivateKey>,
    next_connection_id: Arc<AtomicU64>,
    connection_map: Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>>,
    cancel_token: CancellationToken,
    accept_rx: mpsc::Receiver<Connection>,
    recv_task: Option<JoinHandle<()>>,
}

impl Node {
    const ACCEPT_BUFFER_SIZE: usize = 64;
    const RECV_BUFFER_SIZE: usize = 2048;

    pub async fn bind(addr: SocketAddr, private_key: PrivateKey) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let identity = Arc::new(private_key);
        let next_connection_id = Arc::new(AtomicU64::new(thread_rng().next_u64()));
        let connection_map: Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>> = Default::default();
        let cancel_token = CancellationToken::new();
        let (accept_tx, accept_rx) = mpsc::channel(2);
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
            accept_rx,
            recv_task: Some(recv_task),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn accept(&mut self) -> io::Result<Connection> {
        self.accept_rx
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "node shutdown"))
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<Connection> {
        let connection_id = self
            .next_connection_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let eph = KemPrivateKey::generate();
        let eph_pk = eph.public_key();

        let (tx, mut rx) = mpsc::channel(Self::ACCEPT_BUFFER_SIZE);
        let owned_connection_id = OwnedConnectionId::new(connection_id, &self.connection_map, tx);

        let init = Init {
            initiator_connection_id: connection_id,
            kem_public_key: eph_pk,
        };
        self.socket.send_to(&init.encode(), addr).await?;

        let handshake_ack = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                let packet = rx.recv().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::ConnectionReset, "channel closed")
                })?;
                match packet {
                    Packet::InitAck(v) => break Ok::<_, io::Error>(v),
                    _ => continue,
                }
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;

        let shared_secret = eph
            .decapsulate(&handshake_ack.kem_ciphertext)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "KEM decapsulation failed")
            })?;

        let keys = EncryptionKeys::new(
            &shared_secret,
            &init.kem_public_key,
            &handshake_ack.kem_ciphertext,
        );
        let transcript_hash =
            compute_transcript_hash(&init.kem_public_key, &handshake_ack.kem_ciphertext);
        let session = Session::new(Role::Initiator, 1, keys, transcript_hash);
        let auth_payload = build_auth_payload(&self.identity, &transcript_hash);
        let inner = Arc::new(Mutex::new(InnerConnection::new(
            session,
            connection_id,
            handshake_ack.responder_connection_id,
            true,
            auth_payload,
        )));

        {
            let packets = inner
                .lock()
                .unwrap()
                .poll_packets(std::time::Instant::now());
            for p in &packets {
                let _ = self.socket.send_to(&p.encode(), addr).await;
            }
        }

        let cancel_token = self.cancel_token.child_token();
        let channel_map: ChannelMap = Default::default();
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Connection::main_loop(
            inner.clone(),
            rx,
            self.socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            channel_map.clone(),
            accept_channel_tx,
        ));

        tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "auth timed out"))?
            .map_err(|_| io::Error::new(io::ErrorKind::ConnectionReset, "connection closed"))?;

        Ok(Connection {
            _owned_connection_id: owned_connection_id,
            inner,
            channel_map,
            accept_channel_rx: tokio::sync::Mutex::new(accept_channel_rx),
            socket: self.socket.clone(),
            addr,
            cancel_token,
            main_task: Some(task),
        })
    }

    pub async fn shutdown(mut self) -> Result<(), JoinError> {
        if let Some(recv_task) = self.recv_task.take() {
            self.cancel_token.cancel();
            recv_task.await?;
        }
        Ok(())
    }

    async fn recv_loop(
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        next_connection_id: Arc<AtomicU64>,
        connection_map: Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>>,
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
                                Packet::Init(v) => {
                                    trace!(
                                        initiator_connection_id = v.initiator_connection_id,
                                        "Received Handshake packet"
                                    );
                                    let responder_connection_id = next_connection_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    let connection_cancel_token = cancel_token.child_token();
                                    let (tx, rx) = mpsc::channel(Self::ACCEPT_BUFFER_SIZE);
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
                                Packet::InitAck(v) => {
                                    trace!(
                                        initiator_connection_id = v.initiator_connection_id,
                                        responder_connection_id = v.responder_connection_id,
                                        "Received HandshakeAck packet"
                                    );
                                    let map = connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&v.initiator_connection_id) {
                                        let _ = tx.try_send(Packet::InitAck(v));
                                    }
                                }
                                Packet::Data(v) => {
                                    trace!(
                                        receiver_connection_id = v.receiver_connection_id,
                                        epoch = v.epoch,
                                        "Received Data packet"
                                    );
                                    let map = connection_map.read().unwrap();
                                    if let Some(tx) = map.get(&v.receiver_connection_id) {
                                        let _ = tx.try_send(Packet::Data(v));
                                    }
                                }
                            }
                        }
                        Err(err) => {
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
        if let Some(recv_task) = self.recv_task.take() {
            self.cancel_token.cancel();
            drop(recv_task);
        }
    }
}

type ConnectionMap = Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>>;

struct OwnedConnectionId {
    id: u64,
    connection_map: ConnectionMap,
}

impl OwnedConnectionId {
    fn new(id: u64, connection_map: &ConnectionMap, tx: mpsc::Sender<Packet>) -> Self {
        connection_map.write().unwrap().insert(id, tx);
        Self {
            id,
            connection_map: connection_map.clone(),
        }
    }

    fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for OwnedConnectionId {
    fn drop(&mut self) {
        self.connection_map.write().unwrap().remove(&self.id);
    }
}

type ChannelMap = Arc<RwLock<HashMap<u32, mpsc::Sender<Vec<u8>>>>>;

pub struct Connection {
    _owned_connection_id: OwnedConnectionId,
    inner: Arc<Mutex<InnerConnection>>,
    channel_map: ChannelMap,
    accept_channel_rx: tokio::sync::Mutex<mpsc::Receiver<(u32, u16, mpsc::Receiver<Vec<u8>>)>>,
    socket: Arc<UdpSocket>,
    addr: SocketAddr,
    cancel_token: CancellationToken,
    main_task: Option<JoinHandle<()>>,
}

pub struct StreamChannel {
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
    channel_map: ChannelMap,
    rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
}

pub struct DatagramChannel {
    channel_id: u32,
    inner: Arc<Mutex<InnerConnection>>,
    channel_map: ChannelMap,
    rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
}

impl Connection {
    async fn accept(
        init: Init,
        owned_connection_id: OwnedConnectionId,
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        rx: mpsc::Receiver<Packet>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) {
        let responder_connection_id = owned_connection_id.id();

        let eph = KemPrivateKey::generate();
        let Some((ct, shared_secret)) = eph.encapsulate(&init.kem_public_key) else {
            return; // owned_id drops here, cleans up map
        };

        let keys = EncryptionKeys::new(&shared_secret, &init.kem_public_key, &ct);
        let transcript_hash = compute_transcript_hash(&init.kem_public_key, &ct);

        let response = InitAck {
            responder_connection_id,
            initiator_connection_id: init.initiator_connection_id,
            kem_ciphertext: ct,
        };
        let _ = socket.send_to(&response.encode(), addr).await;

        let session = Session::new(Role::Responder, 1, keys, transcript_hash);
        let auth_payload = build_auth_payload(&identity, &transcript_hash);
        let inner = Arc::new(Mutex::new(InnerConnection::new(
            session,
            responder_connection_id,
            init.initiator_connection_id,
            false,
            auth_payload,
        )));

        {
            let packets = inner.lock().unwrap().poll_packets(Instant::now());
            for p in packets {
                let _ = socket.send_to(&p.encode(), addr).await;
            }
        }

        let channel_map: ChannelMap = Default::default();
        let (accept_channel_tx, accept_channel_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Self::main_loop(
            inner.clone(),
            rx,
            socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            channel_map.clone(),
            accept_channel_tx,
        ));

        // Wait for auth to complete before delivering to accept
        let Ok(Ok(())) = tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx).await else {
            return; // timeout or connection closed — owned_id cleans up
        };

        let connection = Connection {
            _owned_connection_id: owned_connection_id,
            inner,
            channel_map,
            accept_channel_rx: tokio::sync::Mutex::new(accept_channel_rx),
            socket,
            addr,
            cancel_token,
            main_task: Some(task),
        };
        let _ = accept_tx.send(connection).await;
    }

    pub async fn close(mut self) {
        if let Some(task) = self.main_task.take() {
            self.cancel_token.cancel();
            let _ = task.await;
        }
    }

    async fn main_loop(
        inner: Arc<Mutex<InnerConnection>>,
        mut rx: mpsc::Receiver<Packet>,
        socket: Arc<UdpSocket>,
        addr: SocketAddr,
        cancel_token: CancellationToken,
        established_tx: Option<oneshot::Sender<()>>,
        channel_map: ChannelMap,
        accept_channel_tx: mpsc::Sender<(u32, u16, mpsc::Receiver<Vec<u8>>)>,
    ) {
        let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut established_tx = established_tx;

        loop {
            tokio::select! {
                packet = rx.recv() => {
                    let Some(packet) = packet else { break };
                    match packet {
                        Packet::Data(data) => {
                            let (packets, established) = {
                                let mut conn = inner.lock().unwrap();
                                conn.on_data_packet(data, Instant::now());
                                let packets = conn.send_packets();
                                (packets, conn.is_established())
                            };
                            for p in packets {
                                if let Err(err) = socket.send_to(&p.encode(), addr).await {
                                    warn!(?err, "Failed to send packet");
                                }
                            }
                            if established {
                                if let Some(tx) = established_tx.take() {
                                    let _ = tx.send(());
                                }
                            }
                            if Self::dispatch_channels(&inner, &channel_map, &accept_channel_tx) {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    let packets = {
                        let mut conn = inner.lock().unwrap();
                        conn.poll_packets(Instant::now())
                    };
                    for p in &packets {
                        let _ = socket.send_to(&p.encode(), addr).await;
                    }
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }

        // Send ConnectionClose before exiting
        let packets = {
            let mut conn = inner.lock().unwrap();
            conn.queue_connection_close(0);
            conn.poll_packets(Instant::now())
        };
        for p in packets {
            let _ = socket.send_to(&p.encode(), addr).await;
        }
    }

    /// Returns true if the connection should be stopped.
    fn dispatch_channels(
        inner: &Arc<Mutex<InnerConnection>>,
        channel_map: &ChannelMap,
        accept_channel_tx: &mpsc::Sender<(u32, u16, mpsc::Receiver<Vec<u8>>)>,
    ) -> bool {
        let mut conn = inner.lock().unwrap();

        if conn.got_connection_close() {
            channel_map.write().unwrap().clear();
            return true;
        }

        // Deliver accepted channels — register in map immediately so data isn't lost
        while let Some((id, purpose)) = conn.accept_stream() {
            let (tx, rx) = mpsc::channel(16);
            channel_map.write().unwrap().insert(id, tx);
            let _ = accept_channel_tx.try_send((id, purpose, rx));
        }
        while let Some((id, purpose)) = conn.accept_datagram() {
            let (tx, rx) = mpsc::channel(16);
            channel_map.write().unwrap().insert(id, tx);
            let _ = accept_channel_tx.try_send((id, purpose, rx));
        }

        // Dispatch data to registered channels
        let mut finished = Vec::new();
        {
            let map = channel_map.read().unwrap();
            for (&channel_id, tx) in map.iter() {
                // Try stream read
                while let Ok(Some(data)) = conn.read(channel_id) {
                    if tx.try_send(data).is_err() {
                        break;
                    }
                }
                // Try datagram read
                while let Ok(Some(data)) = conn.read_datagram(channel_id) {
                    if tx.try_send(data).is_err() {
                        break;
                    }
                }
                if conn.is_channel_finished(channel_id) {
                    finished.push(channel_id);
                }
            }
        }
        // Remove finished channels — drops tx, closing rx for the user
        if !finished.is_empty() {
            let mut map = channel_map.write().unwrap();
            for id in finished {
                map.remove(&id);
            }
        }
        false
    }

    pub fn open_stream(&self, purpose: u16) -> StreamChannel {
        let (tx, rx) = mpsc::channel(16);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_stream(purpose)
        };
        self.channel_map.write().unwrap().insert(channel_id, tx);
        StreamChannel {
            channel_id,
            inner: self.inner.clone(),
            channel_map: self.channel_map.clone(),
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    pub fn open_datagram(&self, purpose: u16) -> DatagramChannel {
        let (tx, rx) = mpsc::channel(16);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_datagram(purpose)
        };
        self.channel_map.write().unwrap().insert(channel_id, tx);
        DatagramChannel {
            channel_id,
            inner: self.inner.clone(),
            channel_map: self.channel_map.clone(),
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    pub async fn accept_stream(&self) -> io::Result<(StreamChannel, u16)> {
        let (channel_id, purpose, rx) = self
            .accept_channel_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))?;
        Ok((
            StreamChannel {
                channel_id,
                inner: self.inner.clone(),
                channel_map: self.channel_map.clone(),
                rx: tokio::sync::Mutex::new(rx),
            },
            purpose,
        ))
    }

    pub async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)> {
        let (channel_id, purpose, rx) = self
            .accept_channel_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))?;
        Ok((
            DatagramChannel {
                channel_id,
                inner: self.inner.clone(),
                channel_map: self.channel_map.clone(),
                rx: tokio::sync::Mutex::new(rx),
            },
            purpose,
        ))
    }
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

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(task) = self.main_task.take() {
            self.cancel_token.cancel();
            drop(task);
        }
    }
}

fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();
    let sig = identity.sign(transcript_hash);
    let mut payload = Vec::new();
    payload.extend_from_slice(&pk.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());
    payload
}
