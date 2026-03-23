use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;

use crate::v2::crypto::PeerId;
use crate::v2::discovery::{ConnectionRequest, Discovery, DiscoveryFactory};
use crate::v2::raw::{RawConnection, TransportSocket};
use crate::{ServerConnectRequest, ServerRegisterRequest, ServerRequest, ServerResponse};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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
    async fn create(&self, transport: &TransportSocket) -> io::Result<Arc<dyn Discovery>> {
        let raw = transport.connect(self.server_addr)?;
        Ok(Arc::new(ServerDiscovery::new(raw)))
    }
}

pub struct ServerDiscovery {
    shared: Arc<Shared>,
    connection_rx: Mutex<mpsc::Receiver<ConnectionRequest>>,
    _recv_task: JoinHandle<()>,
    _heartbeat_task: JoinHandle<()>,
}

struct Shared {
    raw: RawConnection,
    request_id: AtomicU32,
    pending: Mutex<HashMap<u32, PendingRequest>>,
    resolve_notify: Notify,
    register_notify: Notify,
    connection_tx: mpsc::Sender<ConnectionRequest>,
    connection_notify: Notify,
}

struct PendingRequest {
    result: Option<RequestResult>,
}

enum RequestResult {
    Registered,
    Resolved(SocketAddr),
    #[allow(dead_code)]
    Error(u16),
}

impl ServerDiscovery {
    fn new(raw: RawConnection) -> Self {
        let (connection_tx, connection_rx) = mpsc::channel(64);
        let shared = Arc::new(Shared {
            raw,
            request_id: AtomicU32::new(1),
            pending: Mutex::new(HashMap::new()),
            resolve_notify: Notify::new(),
            register_notify: Notify::new(),
            connection_tx,
            connection_notify: Notify::new(),
        });

        let recv_shared = shared.clone();
        let recv_task = tokio::spawn(recv_loop(recv_shared));

        let hb_shared = shared.clone();
        let heartbeat_task = tokio::spawn(heartbeat_loop(hb_shared));

        Self {
            shared,
            connection_rx: Mutex::new(connection_rx),
            _recv_task: recv_task,
            _heartbeat_task: heartbeat_task,
        }
    }
}

#[async_trait]
impl Discovery for ServerDiscovery {
    async fn recv_connection_request(&self) -> ConnectionRequest {
        loop {
            {
                let mut rx = self.connection_rx.lock().await;
                match rx.try_recv() {
                    Ok(request) => return request,
                    Err(_) => {}
                }
            }
            self.shared.connection_notify.notified().await;
        }
    }

    async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr> {
        let request_id = self.shared.next_request_id();
        let request = ServerRequest::Connect(ServerConnectRequest {
            request_id,
            public_key: peer_id.to_bytes().to_vec(),
            connection_id: 0,
        });

        {
            let mut pending = self.shared.pending.lock().await;
            pending.insert(request_id, PendingRequest { result: None });
        }

        if self.shared.send_request(&request).await.is_err() {
            self.shared.pending.lock().await.remove(&request_id);
            return None;
        }

        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.resolve_notify.notified() => {
                    let mut pending = self.shared.pending.lock().await;
                    if let Some(entry) = pending.get(&request_id) {
                        if let Some(ref result) = entry.result {
                            let addr = match result {
                                RequestResult::Resolved(addr) => Some(*addr),
                                _ => None,
                            };
                            pending.remove(&request_id);
                            return addr;
                        }
                    } else {
                        return None;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    self.shared.pending.lock().await.remove(&request_id);
                    return None;
                }
            }
        }
    }

    async fn register(&self, peer_id: PeerId, _addr: SocketAddr) {
        let request_id = self.shared.next_request_id();
        let request = ServerRequest::Register(ServerRegisterRequest {
            request_id,
            public_key: peer_id.to_bytes().to_vec(),
        });

        {
            let mut pending = self.shared.pending.lock().await;
            pending.insert(request_id, PendingRequest { result: None });
        }

        if self.shared.send_request(&request).await.is_err() {
            self.shared.pending.lock().await.remove(&request_id);
            return;
        }

        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            tokio::select! {
                _ = self.shared.register_notify.notified() => {
                    let mut pending = self.shared.pending.lock().await;
                    if let Some(entry) = pending.get(&request_id) {
                        if entry.result.is_some() {
                            pending.remove(&request_id);
                            return;
                        }
                    } else {
                        return;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    self.shared.pending.lock().await.remove(&request_id);
                    return;
                }
            }
        }
    }
}

impl Shared {
    fn next_request_id(&self) -> u32 {
        loop {
            let id = self.request_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }

    async fn send_request(&self, request: &ServerRequest) -> io::Result<()> {
        let data = request.serialize();
        self.raw.send(&data).await
    }
}

async fn recv_loop(shared: Arc<Shared>) {
    loop {
        let data = match shared.raw.recv().await {
            Some(data) => data,
            None => break,
        };

        let response = match ServerResponse::deserialize(&data) {
            Ok(r) => r,
            Err(_) => continue,
        };

        dispatch_response(&shared, response).await;
    }
}

async fn dispatch_response(shared: &Shared, response: ServerResponse) {
    match response {
        ServerResponse::Heartbeat => {}
        ServerResponse::Register(resp) => {
            complete_request(shared, resp.request_id, RequestResult::Registered).await;
            shared.register_notify.notify_waiters();
        }
        ServerResponse::RegisterError(resp) => {
            complete_request(shared, resp.request_id, RequestResult::Error(resp.code)).await;
            shared.register_notify.notify_waiters();
        }
        ServerResponse::Connect(resp) => {
            complete_request(
                shared,
                resp.request_id,
                RequestResult::Resolved(resp.socket_addr),
            )
            .await;
            shared.resolve_notify.notify_waiters();
        }
        ServerResponse::ConnectError(resp) => {
            complete_request(shared, resp.request_id, RequestResult::Error(resp.code)).await;
            shared.resolve_notify.notify_waiters();
        }
        ServerResponse::IncomingConnection(resp) => {
            let peer_id = PeerId::from_bytes(
                resp.public_key
                    .as_slice()
                    .try_into()
                    .unwrap_or([0u8; crate::v2::crypto::PEER_ID_SIZE]),
            );
            let request = ConnectionRequest {
                peer_addr: resp.socket_addr,
                peer_id: Some(peer_id),
            };
            let _ = shared.connection_tx.send(request).await;
            shared.connection_notify.notify_waiters();
        }
    }
}

async fn complete_request(shared: &Shared, request_id: u32, result: RequestResult) {
    let mut pending = shared.pending.lock().await;
    if let Some(entry) = pending.get_mut(&request_id) {
        entry.result = Some(result);
    }
}

async fn heartbeat_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let _ = shared.send_request(&ServerRequest::Heartbeat).await;
    }
}

impl Drop for ServerDiscovery {
    fn drop(&mut self) {
        self._recv_task.abort();
        self._heartbeat_task.abort();
    }
}
