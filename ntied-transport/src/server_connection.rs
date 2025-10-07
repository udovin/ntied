use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ntied_crypto::PublicKey;
use tokio::sync::{Mutex as TokioMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{Address, Error, ServerRequest, ServerResponse, ToAddress, TransportInner};

pub(crate) struct ServerConnection {
    transport: Arc<TransportInner>,
    server_addr: SocketAddr,
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
        transport: Arc<TransportInner>,
        server_addr: SocketAddr,
        recv_rx: mpsc::Receiver<Vec<u8>>,
        address: Address,
    ) -> Result<Self, Error> {
        let requests = Arc::new(Mutex::new(HashMap::new()));
        let request_id = Arc::new(AtomicU32::new(0));
        let (accept_tx, accept_rx) = mpsc::channel(100);
        let accept_rx = TokioMutex::new(accept_rx);
        let alive = Arc::new(AtomicBool::new(true));
        let receiver_task = tokio::spawn(Self::receiver_loop(
            recv_rx,
            requests.clone(),
            accept_tx,
            alive.clone(),
        ));
        // Register with the server
        let public_key = transport.private_key.public_key().to_bytes()?;
        Self::register(
            transport.clone(),
            server_addr,
            requests.clone(),
            request_id.clone(),
            address,
            public_key,
        )
        .await?;
        let heartbeat_task = tokio::spawn(Self::heartbeat_loop(
            transport.clone(),
            server_addr,
            alive.clone(),
        ));
        Ok(Self {
            transport,
            server_addr,
            requests,
            request_id,
            receiver_task,
            heartbeat_task,
            accept_rx,
        })
    }

    pub async fn connect(
        &self,
        address: impl ToAddress,
        source_id: u32,
    ) -> Result<PeerInfo, Error> {
        let address = address.to_address()?;
        tracing::debug!(?address, "Requesting for connection to peer");
        let request_id = self.next_request_id();
        let request = ServerRequest::Connect(crate::ServerConnectRequest {
            request_id,
            address,
            source_id,
        });
        // Create a channel to receive the response
        let (tx, rx) = oneshot::channel();
        // Register the request with its request_id
        self.requests.lock().unwrap().insert(request_id, tx);
        // Send the request to the server
        self.transport
            .socket
            .send_to(&request.serialize(), self.server_addr)
            .await?;
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
                    peer_address = ?resp.address,
                    "Received connect response from server",
                );
                let public_key = PublicKey::from_bytes(&resp.public_key)?;
                Ok(PeerInfo {
                    addr: resp.addr,
                    address: resp.address,
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
        transport: Arc<TransportInner>,
        socket_addr: SocketAddr,
        requests: Arc<Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>>,
        request_id_counter: Arc<AtomicU32>,
        address: Address,
        public_key: Vec<u8>,
    ) -> Result<(), Error> {
        tracing::debug!("Registering with server");
        let request_id = Self::next_request_id_static(&request_id_counter);
        let request = ServerRequest::Register(crate::ServerRegisterRequest {
            request_id,
            public_key,
            address,
        });
        // Create a channel to receive the response
        let (tx, rx) = oneshot::channel();
        // Register the request with its request_id
        requests.lock().unwrap().insert(request_id, tx);
        // Send the request to the server
        transport
            .socket
            .send_to(&request.serialize(), socket_addr)
            .await?;
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
        mut recv_rx: mpsc::Receiver<Vec<u8>>,
        requests: Arc<Mutex<HashMap<u32, oneshot::Sender<ServerResponse>>>>,
        accept_tx: mpsc::Sender<PeerInfo>,
        alive: Arc<AtomicBool>,
    ) {
        loop {
            // Receive next packet from the server
            let data = match timeout(Self::CONNECTION_TIMEOUT, recv_rx.recv()).await {
                Ok(Some(data)) => data,
                Ok(None) => {
                    tracing::error!("Connection closed");
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
                        address: resp.address,
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

    async fn heartbeat_loop(
        transport: Arc<TransportInner>,
        server_addr: SocketAddr,
        alive: Arc<AtomicBool>,
    ) {
        loop {
            tokio::time::sleep(Self::HEARTBEAT_INTERVAL).await;
            if !alive.load(Ordering::Relaxed) {
                break;
            }
            tracing::debug!("Sending heartbeat to server");
            let request = ServerRequest::Heartbeat;
            if let Err(err) = transport
                .socket
                .send_to(&request.serialize(), server_addr)
                .await
            {
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
        // Clean up raw connection to server
        self.transport
            .raw_connections
            .write()
            .unwrap()
            .remove(&self.server_addr);
    }
}

pub(crate) struct PeerInfo {
    pub addr: SocketAddr,
    pub address: Address,
    pub public_key: PublicKey,
    pub source_id: Option<u32>,
}
