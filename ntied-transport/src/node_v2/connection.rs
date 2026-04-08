use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::io;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::connection::Connection as InnerConnection;
use crate::crypto::{
    EncryptionKeys, KemPrivateKey, PeerId, PrivateKey, PublicKey, compute_transcript_hash,
};
use crate::session::{Role, Session};
use crate::wire::{Data, Handshake, HandshakeAck, Packet};

use super::channel::{
    DatagramChannel, OwnedChannelId, StreamChannel, datagram_read_loop, stream_read_loop,
};
use super::util::build_auth_payload;

const FLUSH_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type ConnectionMap = Arc<RwLock<HashMap<u64, mpsc::Sender<Packet>>>>;

pub(crate) struct OwnedConnectionId {
    id: u64,
    connection_map: ConnectionMap,
}

impl OwnedConnectionId {
    pub(crate) fn new(id: u64, connection_map: &ConnectionMap, tx: mpsc::Sender<Packet>) -> Self {
        connection_map.write().unwrap().insert(id, tx);
        Self {
            id,
            connection_map: connection_map.clone(),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for OwnedConnectionId {
    fn drop(&mut self) {
        self.connection_map.write().unwrap().remove(&self.id);
    }
}

pub struct Connection {
    pub(crate) connection_id: OwnedConnectionId,
    pub(crate) inner: Arc<Mutex<InnerConnection>>,
    pub(crate) data_notify: Arc<Notify>,
    pub(crate) accept_stream_rx: TokioMutex<mpsc::Receiver<StreamChannel>>,
    pub(crate) accept_datagram_rx: TokioMutex<mpsc::Receiver<DatagramChannel>>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) addr: SocketAddr,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) main_task: Mutex<Option<JoinHandle<()>>>,
}

impl Connection {
    pub(crate) async fn accept(
        init: Handshake,
        connection_id: OwnedConnectionId,
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        rx: mpsc::Receiver<Packet>,
        accept_tx: mpsc::Sender<Connection>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) {
        let responder_connection_id = connection_id.id();

        let eph = KemPrivateKey::generate();
        let Some((ct, shared_secret)) = eph.encapsulate(&init.kem_public_key) else {
            return;
        };

        let keys = EncryptionKeys::new(&shared_secret, &init.kem_public_key, &ct);
        let transcript_hash = compute_transcript_hash(&init.kem_public_key, &ct);

        let response = HandshakeAck {
            responder_connection_id,
            initiator_connection_id: init.initiator_connection_id,
            kem_ciphertext: ct,
        };
        if let Err(err) = socket.send_to(&response.encode(), addr).await {
            warn!(?err, "Failed to send handshake ack");
        }

        let session = Session::new(Role::Responder, 1, keys, transcript_hash);
        let auth_payload = build_auth_payload(&identity, &transcript_hash);
        let mut conn = InnerConnection::new(
            session,
            responder_connection_id,
            init.initiator_connection_id,
            false,
            auth_payload,
        );

        let packets = conn.poll_packets(Instant::now());
        Self::send_packets(&socket, addr, packets).await;

        let inner = Arc::new(Mutex::new(conn));
        let data_notify = Arc::new(Notify::new());

        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(8);
        let (accept_datagram_tx, accept_datagram_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Self::main_loop(
            inner.clone(),
            rx,
            socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            data_notify.clone(),
            accept_stream_tx,
            accept_datagram_tx,
        ));

        match tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                cancel_token.cancel();
                warn!("Connection unexpectedly closed");
                return;
            }
            Err(_) => {
                cancel_token.cancel();
                warn!("Auth timed out");
                return;
            }
        }

        let connection = Connection {
            connection_id,
            inner,
            data_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_datagram_rx: TokioMutex::new(accept_datagram_rx),
            socket,
            addr,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        };
        if let Err(_) = accept_tx.send(connection).await {
            warn!("Failed to send connection to accept queue");
        }
    }

    pub(crate) async fn connect(
        owned_connection_id: OwnedConnectionId,
        eph: KemPrivateKey,
        init: Handshake,
        mut rx: mpsc::Receiver<Packet>,
        socket: Arc<UdpSocket>,
        identity: Arc<PrivateKey>,
        cancel_token: CancellationToken,
        addr: SocketAddr,
    ) -> io::Result<Connection> {
        let connection_id = owned_connection_id.id();

        let handshake_ack = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                let packet = rx.recv().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::ConnectionReset, "channel closed")
                })?;
                match packet {
                    Packet::HandshakeAck(v) => break Ok::<_, io::Error>(v),
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
        let auth_payload = build_auth_payload(&identity, &transcript_hash);
        let mut conn = InnerConnection::new(
            session,
            connection_id,
            handshake_ack.responder_connection_id,
            true,
            auth_payload,
        );

        let packets = conn.poll_packets(Instant::now());
        Self::send_packets(&socket, addr, packets).await;

        let inner = Arc::new(Mutex::new(conn));
        let data_notify = Arc::new(Notify::new());

        let (accept_stream_tx, accept_stream_rx) = mpsc::channel(8);
        let (accept_datagram_tx, accept_datagram_rx) = mpsc::channel(8);
        let (established_tx, established_rx) = oneshot::channel();
        let task = tokio::spawn(Self::main_loop(
            inner.clone(),
            rx,
            socket.clone(),
            addr,
            cancel_token.clone(),
            Some(established_tx),
            data_notify.clone(),
            accept_stream_tx,
            accept_datagram_tx,
        ));

        match tokio::time::timeout(HANDSHAKE_TIMEOUT, established_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                cancel_token.cancel();
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "Connection closed",
                ));
            }
            Err(_) => {
                cancel_token.cancel();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Auth timed out"));
            }
        }

        Ok(Connection {
            connection_id: owned_connection_id,
            inner,
            data_notify,
            accept_stream_rx: TokioMutex::new(accept_stream_rx),
            accept_datagram_rx: TokioMutex::new(accept_datagram_rx),
            socket,
            addr,
            cancel_token,
            main_task: Mutex::new(Some(task)),
        })
    }

    pub fn connection_id(&self) -> u64 {
        self.connection_id.id()
    }

    pub fn peer_public_key(&self) -> Option<PublicKey> {
        self.inner.lock().unwrap().peer_public_key().cloned()
    }

    pub fn peer_id(&self) -> Option<PeerId> {
        self.peer_public_key().map(|pk| pk.peer_id())
    }

    pub fn remote_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn close(&self) {
        let main_task = self.main_task.lock().unwrap().take();
        if let Some(task) = main_task {
            self.cancel_token.cancel();
            let _ = task.await;
        }
    }

    pub(crate) async fn main_loop(
        inner: Arc<Mutex<InnerConnection>>,
        mut rx: mpsc::Receiver<Packet>,
        socket: Arc<UdpSocket>,
        addr: SocketAddr,
        cancel_token: CancellationToken,
        established_tx: Option<oneshot::Sender<()>>,
        data_notify: Arc<Notify>,
        accept_stream_tx: mpsc::Sender<StreamChannel>,
        accept_datagram_tx: mpsc::Sender<DatagramChannel>,
    ) {
        let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ping_interval = tokio::time::interval(PING_INTERVAL);
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut established_tx = established_tx;
        let mut last_recv = Instant::now();
        let mut ping_counter: u32 = 0;

        loop {
            tokio::select! {
                packet = rx.recv() => {
                    let Some(packet) = packet else { break };
                    match packet {
                        Packet::Data(data) => {
                            last_recv = Instant::now();
                            let packets = {
                                let mut conn = inner.lock().unwrap();
                                conn.on_data_packet(data, last_recv);
                                if conn.got_connection_close() {
                                    drop(conn);
                                    data_notify.notify_waiters();
                                    return;
                                }
                                if conn.is_established() {
                                    if let Some(tx) = established_tx.take() {
                                        let _ = tx.send(());
                                    }
                                }
                                conn.send_packets()
                            };
                            Self::send_packets(&socket, addr, packets).await;
                            data_notify.notify_waiters();
                            Self::accept_new_channels(
                                &inner,
                                &data_notify,
                                &cancel_token,
                                &accept_stream_tx,
                                &accept_datagram_tx,
                            );
                        }
                        _ => {}
                    }
                }
                _ = flush_interval.tick() => {
                    let now = Instant::now();
                    if now.duration_since(last_recv) > CONNECTION_TIMEOUT {
                        warn!("Connection timed out");
                        break;
                    }
                    let packets = {
                        let mut conn = inner.lock().unwrap();
                        conn.poll_packets(now)
                    };
                    Self::send_packets(&socket, addr, packets).await;
                }
                _ = ping_interval.tick() => {
                    let packets = {
                        let mut conn = inner.lock().unwrap();
                        if conn.is_established() {
                            ping_counter = ping_counter.wrapping_add(1);
                            conn.queue_ping(ping_counter);
                            conn.send_packets()
                        } else {
                            Vec::new()
                        }
                    };
                    Self::send_packets(&socket, addr, packets).await;
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }

        let packets = {
            let mut conn = inner.lock().unwrap();
            if !conn.got_connection_close() {
                conn.queue_connection_close(0);
            }
            conn.poll_packets(Instant::now())
        };
        Self::send_packets(&socket, addr, packets).await;
        data_notify.notify_waiters();
    }

    fn accept_new_channels(
        inner: &Arc<Mutex<InnerConnection>>,
        data_notify: &Arc<Notify>,
        cancel_token: &CancellationToken,
        accept_stream_tx: &mpsc::Sender<StreamChannel>,
        accept_datagram_tx: &mpsc::Sender<DatagramChannel>,
    ) {
        let mut accepted = Vec::new();
        {
            let mut conn = inner.lock().unwrap();
            while let Some((id, purpose)) = conn.accept_stream() {
                accepted.push((id, purpose, true));
            }
            while let Some((id, purpose)) = conn.accept_datagram() {
                accepted.push((id, purpose, false));
            }
        }

        for (id, purpose, is_stream) in accepted {
            let channel_token = cancel_token.child_token();
            let (tx, rx) = mpsc::channel(1);

            if is_stream {
                let read_task = tokio::spawn(stream_read_loop(
                    id,
                    inner.clone(),
                    data_notify.clone(),
                    channel_token.clone(),
                    tx,
                ));
                let owned = OwnedChannelId::new(id, inner);
                let channel = StreamChannel {
                    owned,
                    purpose,
                    cancel_token: channel_token,
                    read_task: Mutex::new(Some(read_task)),
                    rx: TokioMutex::new(rx),
                };
                let accept_tx = accept_stream_tx.clone();
                let token = cancel_token.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        result = accept_tx.send(channel) => {
                            if result.is_err() {
                                warn!(channel_id = id, "Stream not accepted, closing");
                            }
                        }
                        _ = token.cancelled() => {}
                    }
                });
            } else {
                let read_task = tokio::spawn(datagram_read_loop(
                    id,
                    inner.clone(),
                    data_notify.clone(),
                    channel_token.clone(),
                    tx,
                ));
                let owned = OwnedChannelId::new(id, inner);
                let channel = DatagramChannel {
                    owned,
                    purpose,
                    cancel_token: channel_token,
                    read_task: Mutex::new(Some(read_task)),
                    rx: TokioMutex::new(rx),
                };
                let accept_tx = accept_datagram_tx.clone();
                let token = cancel_token.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        result = accept_tx.send(channel) => {
                            if result.is_err() {
                                warn!(channel_id = id, "Datagram not accepted, closing");
                            }
                        }
                        _ = token.cancelled() => {}
                    }
                });
            }
        }
    }

    async fn send_packets(socket: &UdpSocket, addr: SocketAddr, packets: Vec<Data>) {
        for packet in packets {
            if let Err(err) = socket.send_to(&packet.encode(), addr).await {
                warn!(?err, "Failed to send packet");
            }
        }
    }

    pub fn open_stream(&self, purpose: u16) -> StreamChannel {
        let channel_token = self.cancel_token.child_token();
        let (tx, rx) = mpsc::channel(1);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_stream(purpose)
        };
        let read_task = tokio::spawn(stream_read_loop(
            channel_id,
            self.inner.clone(),
            self.data_notify.clone(),
            channel_token.clone(),
            tx,
        ));
        let owned = OwnedChannelId::new(channel_id, &self.inner);
        StreamChannel {
            owned,
            purpose,
            cancel_token: channel_token,
            read_task: Mutex::new(Some(read_task)),
            rx: TokioMutex::new(rx),
        }
    }

    pub fn open_datagram(&self, purpose: u16) -> DatagramChannel {
        let channel_token = self.cancel_token.child_token();
        let (tx, rx) = mpsc::channel(1);
        let channel_id = {
            let mut conn = self.inner.lock().unwrap();
            conn.open_datagram(purpose)
        };
        let read_task = tokio::spawn(datagram_read_loop(
            channel_id,
            self.inner.clone(),
            self.data_notify.clone(),
            channel_token.clone(),
            tx,
        ));
        let owned = OwnedChannelId::new(channel_id, &self.inner);
        DatagramChannel {
            owned,
            purpose,
            cancel_token: channel_token,
            read_task: Mutex::new(Some(read_task)),
            rx: TokioMutex::new(rx),
        }
    }

    pub async fn accept_stream(&self) -> io::Result<StreamChannel> {
        self.accept_stream_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))
    }

    pub async fn accept_datagram(&self) -> io::Result<DatagramChannel> {
        self.accept_datagram_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed"))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let main_task = self.main_task.lock().unwrap().take();
        if let Some(task) = main_task {
            self.cancel_token.cancel();
            drop(task);
        }
    }
}
