use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ntied_crypto::PublicKey;
use tokio::sync::{Mutex as TokioMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{
    ConnectionRequest, Discovery, DiscoveryFactory, Error, RawConnection, RawTransport,
    ServerConnectRequest, ServerRegisterRequest, ServerRequest, ServerResponse,
};

pub struct ServerDiscoveryFactory {
    server_addr: SocketAddr,
}

impl ServerDiscoveryFactory {
    pub fn new(server_addr: SocketAddr) -> Self {
        Self { server_addr }
    }
}

#[async_trait]
impl DiscoveryFactory for ServerDiscoveryFactory {
    async fn create(&self, transport: RawTransport) -> Result<Arc<dyn Discovery>, Error> {
        let raw_connection = transport.connect(self.server_addr)?;
        let public_key = transport.private_key().public_key();
        Ok(Arc::new(
            ServerConnection::new(raw_connection, public_key).await?,
        ))
    }
}

#[async_trait]
impl Discovery for ServerConnection {
    async fn send_connection_request(
        &self,
        public_key: &PublicKey,
        source_id: u32,
    ) -> Result<SocketAddr, Error> {
        let peer_info = self.connect(public_key, source_id).await?;
        Ok(peer_info.addr)
    }

    async fn recv_connection_request(&self) -> Result<ConnectionRequest, Error> {
        let peer_info = self.accept().await?;
        Ok(ConnectionRequest {
            socket_addr: peer_info.addr,
            public_key: Some(peer_info.public_key),
            source_id: peer_info.source_id,
        })
    }
}

pub(crate) struct ServerConnection {
    raw_connection: Arc<RawConnection>,
    requests: Arc<Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>>,
    request_id: Arc<AtomicU32>,
    receiver_task: JoinHandle<()>,
    heartbeat_task: JoinHandle<()>,
    accept_rx: TokioMutex<mpsc::Receiver<PeerInfo>>,
}

impl ServerConnection {
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(8);
    const CONNECTION_TIMEOUT: Duration = Duration::from_secs(32);

    pub(crate) async fn new(
        raw_connection: RawConnection,
        public_key: PublicKey,
    ) -> Result<Self, Error> {
        let raw_connection = Arc::new(raw_connection);
        let requests = Arc::new(Mutex::new(HashMap::new()));
        let request_id = Arc::new(AtomicU32::new(0));
        let (accept_tx, accept_rx) = mpsc::channel(100);
        let accept_rx = TokioMutex::new(accept_rx);
        let alive = Arc::new(AtomicBool::new(true));
        let receiver_task = tokio::spawn(Self::receiver_loop(
            raw_connection.clone(),
            requests.clone(),
            accept_tx,
            alive.clone(),
        ));
        // Register with the server
        Self::register(
            raw_connection.clone(),
            requests.clone(),
            request_id.clone(),
            public_key,
        )
        .await?;
        let heartbeat_task =
            tokio::spawn(Self::heartbeat_loop(raw_connection.clone(), alive.clone()));
        Ok(Self {
            raw_connection,
            requests,
            request_id,
            receiver_task,
            heartbeat_task,
            accept_rx,
        })
    }

    pub async fn connect(&self, public_key: &PublicKey, source_id: u32) -> Result<PeerInfo, Error> {
        tracing::debug!("Requesting for connection to peer");
        let request_id = self.next_request_id();
        let request = ServerRequest::Connect(ServerConnectRequest {
            request_id,
            public_key: public_key.to_bytes().unwrap(),
            source_id,
        });
        // Create a channel to receive the response
        let (tx, rx) = oneshot::channel();
        // Register the request with its request_id
        self.requests.lock().unwrap().insert(request_id, tx);
        // Send the request to the server
        self.raw_connection.send(request.serialize()).await?;
        // Wait for the response with timeout
        let response = timeout(Self::CONNECTION_TIMEOUT, rx)
            .await
            .map_err(|_| "Connection timeout")?
            .map_err(|_| "Channel closed")?;
        // Process the response
        match response {
            ServerResponse::Connect(resp) => {
                tracing::trace!(
                    peer_addr = ?resp.addr,
                    "Received connect response from server",
                );
                let public_key = PublicKey::from_bytes(&resp.public_key)?;
                Ok(PeerInfo {
                    addr: resp.addr,
                    public_key,
                    source_id: None,
                })
            }
            ServerResponse::ConnectError(err) => {
                Err(format!("Connect error: code {}", err.code).into())
            }
            _ => Err("Unexpected response type".into()),
        }
    }

    pub async fn accept(&self) -> Result<PeerInfo, Error> {
        self.accept_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Server connection closed".into())
    }

    async fn register(
        raw_connection: Arc<RawConnection>,
        requests: Arc<Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>>,
        request_id_counter: Arc<AtomicU32>,
        public_key: PublicKey,
    ) -> Result<(), Error> {
        tracing::debug!("Registering with server");
        let request_id = Self::next_request_id_static(&request_id_counter);
        let request = ServerRequest::Register(ServerRegisterRequest {
            request_id,
            public_key: public_key.to_bytes()?,
        });
        // Create a channel to receive the response
        let (tx, rx) = oneshot::channel();
        // Register the request with its request_id
        requests.lock().unwrap().insert(request_id, tx);
        // Send the request to the server
        raw_connection.send(request.serialize()).await?;
        // Wait for the response with timeout
        let response = timeout(Self::CONNECTION_TIMEOUT, rx)
            .await
            .map_err(|_| "Register timeout")?
            .map_err(|_| "Channel closed")?;
        // Process the response
        match response {
            ServerResponse::Register(_) => Ok(()),
            ServerResponse::RegisterError(err) => {
                Err(format!("Register error: code {}", err.code).into())
            }
            _ => Err("Unexpected response type".into()),
        }
    }

    async fn receiver_loop(
        raw_connection: Arc<RawConnection>,
        requests: Arc<Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>>,
        accept_tx: mpsc::Sender<PeerInfo>,
        alive: Arc<AtomicBool>,
    ) {
        loop {
            // Receive next packet from the server
            let data = match timeout(Self::CONNECTION_TIMEOUT, raw_connection.recv()).await {
                Ok(Ok(data)) => data,
                Ok(Err(err)) => {
                    tracing::error!(?err, "Connection closed");
                    break;
                }
                Err(_) => {
                    tracing::error!("Connection timeout");
                    break;
                }
            };
            // Deserialize the response
            let response = match ServerResponse::deserialize(&data) {
                Ok(response) => response,
                Err(err) => {
                    tracing::warn!(?err, "Failed to deserialize response");
                    continue;
                }
            };
            // Handle the response based on its type
            match &response {
                ServerResponse::Heartbeat => {
                    tracing::debug!("Received heartbeat response");
                }
                ServerResponse::Register(resp) => {
                    let request_id = resp.request_id;
                    let mut requests_guard = requests.lock().unwrap();
                    if let Some(sender) = requests_guard.remove(&request_id) {
                        tracing::debug!(request_id, "Routing register response to waiting request");
                        if sender.send(response).is_err() {
                            tracing::warn!(
                                request_id,
                                "Failed to send response to dropped receiver"
                            );
                        }
                    } else {
                        tracing::warn!(request_id, "Received response with unknown request_id");
                    }
                }
                ServerResponse::RegisterError(resp) => {
                    let request_id = resp.request_id;
                    let mut requests_guard = requests.lock().unwrap();
                    if let Some(sender) = requests_guard.remove(&request_id) {
                        tracing::debug!(
                            request_id,
                            "Routing register error response to waiting request"
                        );
                        if sender.send(response).is_err() {
                            tracing::warn!(
                                request_id,
                                "Failed to send response to dropped receiver"
                            );
                        }
                    } else {
                        tracing::warn!(request_id, "Received response with unknown request_id");
                    }
                }
                ServerResponse::Connect(resp) => {
                    let request_id = resp.request_id;
                    let mut requests_guard = requests.lock().unwrap();
                    if let Some(sender) = requests_guard.remove(&request_id) {
                        tracing::debug!(request_id, "Routing connect response to waiting request");
                        if sender.send(response).is_err() {
                            tracing::warn!(
                                request_id,
                                "Failed to send response to dropped receiver"
                            );
                        }
                    } else {
                        tracing::warn!(request_id, "Received response with unknown request_id");
                    }
                }
                ServerResponse::ConnectError(resp) => {
                    let request_id = resp.request_id;
                    let mut requests_guard = requests.lock().unwrap();
                    if let Some(sender) = requests_guard.remove(&request_id) {
                        tracing::debug!(
                            request_id,
                            "Routing connect error response to waiting request"
                        );
                        if sender.send(response).is_err() {
                            tracing::warn!(
                                request_id,
                                "Failed to send response to dropped receiver"
                            );
                        }
                    } else {
                        tracing::warn!(request_id, "Received response with unknown request_id");
                    }
                }
                ServerResponse::IncomingConnection(resp) => {
                    tracing::debug!(source_id = ?resp.source_id, peer_addr = ?resp.addr, "Received incoming connection notification");
                    let public_key = match PublicKey::from_bytes(&resp.public_key) {
                        Ok(pk) => pk,
                        Err(err) => {
                            tracing::warn!(?err, "Failed to parse public key");
                            continue;
                        }
                    };
                    let peer_info = PeerInfo {
                        addr: resp.addr,
                        public_key,
                        source_id: Some(resp.source_id),
                    };
                    if accept_tx.send(peer_info).await.is_err() {
                        tracing::warn!("Failed to send peer notification: receiver dropped");
                    }
                }
            }
        }
        alive.store(false, Ordering::Relaxed);
        // Clear all pending requests
        let mut requests_guard = requests.lock().unwrap();
        requests_guard.clear();
    }

    async fn heartbeat_loop(raw_connection: Arc<RawConnection>, alive: Arc<AtomicBool>) {
        loop {
            tokio::time::sleep(Self::HEARTBEAT_INTERVAL).await;
            if !alive.load(Ordering::Relaxed) {
                break;
            }
            tracing::debug!("Sending heartbeat to server");
            let request = ServerRequest::Heartbeat;
            if let Err(err) = raw_connection.send(request.serialize()).await {
                tracing::warn!(?err, "Failed to send heartbeat");
            }
        }
    }

    fn next_request_id(&self) -> u32 {
        Self::next_request_id_static(&self.request_id)
    }

    fn next_request_id_static(request_id: &AtomicU32) -> u32 {
        loop {
            let id = request_id.fetch_add(1, Ordering::SeqCst);
            if id != 0 {
                return id;
            }
        }
    }
}

impl Drop for ServerConnection {
    fn drop(&mut self) {
        self.receiver_task.abort();
        self.heartbeat_task.abort();
    }
}

pub(crate) struct PeerInfo {
    pub addr: SocketAddr,
    pub public_key: PublicKey,
    pub source_id: Option<u32>,
}
