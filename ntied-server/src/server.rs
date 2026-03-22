use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ntied_transport::{
    ServerConnectRequest, ServerConnectResponse, ServerErrorResponse,
    ServerIncomingConnectionResponse, ServerRegisterResponse, ServerRequest, ServerResponse,
};
use tokio::net::{ToSocketAddrs, UdpSocket};
use tokio::sync::RwLock;
use tokio::time::interval;

#[derive(Debug, Clone)]
struct ClientInfo {
    socket_addr: SocketAddr,
    public_key: Vec<u8>,
    last_seen: Instant,
}

pub struct Server {
    socket: Arc<UdpSocket>,
    clients: Arc<RwLock<HashMap<Vec<u8>, ClientInfo>>>,
}

impl Server {
    const PACKET_SIZE: usize = 65536;
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(32);
    const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

    /// Create and bind a new coordination server
    pub async fn new(
        addr: impl ToSocketAddrs,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let socket = Arc::new(UdpSocket::bind(&addr).await?);
        tracing::info!(addr = ?socket.local_addr()?, "Server started listening");
        Ok(Self {
            socket,
            clients: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get the server's socket address
    pub fn local_addr(&self) -> Result<SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.socket.local_addr()?)
    }

    /// Run the server
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _cleanup_handle = self.spawn_cleanup_task();
        let mut buf = vec![0u8; Self::PACKET_SIZE];
        loop {
            let (len, addr) = match self.socket.recv_from(&mut buf).await {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!(?err, "Error receiving packet");
                    continue;
                }
            };
            let data = &buf[..len];
            let request = match ServerRequest::deserialize(data) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(?addr, ?err, "Failed to deserialize request");
                    continue;
                }
            };
            if let Err(err) = self.handle_request(request, addr).await {
                tracing::error!(?addr, ?err, "Error handling request");
            }
        }
    }

    /// Handle incoming request
    async fn handle_request(
        &self,
        request: ServerRequest,
        addr: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match request {
            ServerRequest::Heartbeat => {
                self.handle_heartbeat(addr).await?;
            }
            ServerRequest::Register(req) => {
                self.handle_register(addr, req.request_id, req.public_key)
                    .await?;
            }
            ServerRequest::RegisterWithAddr(req) => {
                self.handle_register(req.socket_addr, req.request_id, req.public_key)
                    .await?;
            }
            ServerRequest::Connect(req) => {
                self.handle_connect(addr, req).await?;
            }
        }
        Ok(())
    }

    async fn handle_register(
        &self,
        socket_addr: SocketAddr,
        request_id: u32,
        public_key: Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::debug!(
            ?socket_addr,
            key_len = public_key.len(),
            "Received registration request"
        );
        let client_info = ClientInfo {
            socket_addr,
            public_key: public_key.clone(),
            last_seen: Instant::now(),
        };
        {
            let mut clients = self.clients.write().await;
            clients.insert(public_key, client_info);
        }
        tracing::info!(?socket_addr, "Client registered");
        self.send_response(
            socket_addr,
            ServerResponse::Register(ServerRegisterResponse { request_id }),
        )
        .await?;
        Ok(())
    }

    /// Handle peer connection request
    async fn handle_connect(
        &self,
        socket_addr: SocketAddr,
        req: ServerConnectRequest,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::debug!(?socket_addr, "Received connection request");
        let (peer_info, requester_info) = {
            let mut clients = self.clients.write().await;
            let requester_info = match clients.values_mut().find(|c| c.socket_addr == socket_addr) {
                Some(client) => {
                    client.last_seen = Instant::now();
                    client.clone()
                }
                None => {
                    tracing::warn!(?socket_addr, "Connection request from unregistered client");
                    self.send_response(
                        socket_addr,
                        ServerResponse::ConnectError(ServerErrorResponse {
                            request_id: req.request_id,
                            code: 10, // Not registered
                        }),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let peer_info = match clients.get(&req.public_key) {
                Some(peer) => peer.clone(),
                None => {
                    tracing::debug!(key_len = req.public_key.len(), "Peer not found");
                    self.send_response(
                        socket_addr,
                        ServerResponse::ConnectError(ServerErrorResponse {
                            request_id: req.request_id,
                            code: 11, // Peer not found
                        }),
                    )
                    .await?;
                    return Ok(());
                }
            };
            (peer_info, requester_info)
        };
        // Check if trying to connect to self
        if peer_info.socket_addr == socket_addr {
            tracing::warn!(?socket_addr, "Client trying to connect to itself");
            self.send_response(
                socket_addr,
                ServerResponse::ConnectError(ServerErrorResponse {
                    request_id: req.request_id,
                    code: 12, // Cannot connect to self
                }),
            )
            .await?;
            return Ok(());
        }
        // Send peer info to requester
        self.send_response(
            socket_addr,
            ServerResponse::Connect(ServerConnectResponse {
                request_id: req.request_id,
                public_key: peer_info.public_key.clone(),
                socket_addr: peer_info.socket_addr,
            }),
        )
        .await?;
        // Notify peer about incoming connection
        self.send_response(
            peer_info.socket_addr,
            ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
                public_key: requester_info.public_key.clone(),
                socket_addr: requester_info.socket_addr,
                connection_id: req.connection_id,
            }),
        )
        .await?;
        tracing::info!(
            from_addr = ?socket_addr,
            to_addr = ?peer_info.socket_addr,
            "Peers initiated connection"
        );
        Ok(())
    }

    /// Handle heartbeat message
    async fn handle_heartbeat(
        &self,
        socket_addr: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::debug!(?socket_addr, "Received heartbeat");
        let mut clients = self.clients.write().await;
        match clients.values_mut().find(|c| c.socket_addr == socket_addr) {
            Some(client) => {
                client.last_seen = Instant::now();
                drop(clients);
                self.send_response(socket_addr, ServerResponse::Heartbeat)
                    .await
            }
            None => {
                drop(clients);
                tracing::warn!(?socket_addr, "Heartbeat from unregistered client");
                Ok(())
            }
        }
    }

    /// Send response to a client
    async fn send_response(
        &self,
        socket_addr: SocketAddr,
        response: ServerResponse,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::debug!(?socket_addr, "Sending response to client");
        let data = response.serialize();
        self.socket.send_to(&data, socket_addr).await?;
        Ok(())
    }

    /// Spawn cleanup task to remove inactive clients
    fn spawn_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        let clients = self.clients.clone();
        tokio::spawn(async move {
            let mut interval = interval(Self::CLEANUP_INTERVAL);
            loop {
                interval.tick().await;
                let mut clients = clients.write().await;
                let now = Instant::now();
                clients.retain(|public_key, client| {
                    let keep = now.duration_since(client.last_seen) < Self::CLIENT_TIMEOUT;
                    if !keep {
                        tracing::info!(
                            ?public_key,
                            socket_addr = ?client.socket_addr,
                            "Removing inactive client"
                        );
                    }
                    keep
                });
            }
        })
    }
}
