use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::v2::crypto::PeerId;
use crate::v2::discovery::Discovery;
use crate::{ServerRegisterWithAddrRequest, ServerRequest, ServerResponse};

const RECV_BUF_SIZE: usize = 4096;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ServerDiscovery {
    shared: Arc<Shared>,
    _recv_task: JoinHandle<()>,
    _heartbeat_task: JoinHandle<()>,
}

struct Shared {
    socket: UdpSocket,
    server_addr: SocketAddr,
    request_id: AtomicU32,
    pending: Mutex<HashMap<u32, PendingRequest>>,
    resolve_notify: Notify,
    register_notify: Notify,
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
    pub async fn connect(server_addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let shared = Arc::new(Shared {
            socket,
            server_addr,
            request_id: AtomicU32::new(1),
            pending: Mutex::new(HashMap::new()),
            resolve_notify: Notify::new(),
            register_notify: Notify::new(),
        });

        let recv_shared = shared.clone();
        let recv_task = tokio::spawn(recv_loop(recv_shared));

        let hb_shared = shared.clone();
        let heartbeat_task = tokio::spawn(heartbeat_loop(hb_shared));

        Ok(Self {
            shared,
            _recv_task: recv_task,
            _heartbeat_task: heartbeat_task,
        })
    }
}

#[async_trait]
impl Discovery for ServerDiscovery {
    async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr> {
        let request_id = self.shared.next_request_id();
        let request = ServerRequest::Connect(crate::ServerConnectRequest {
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

    async fn register(&self, peer_id: PeerId, addr: SocketAddr) {
        let request_id = self.shared.next_request_id();
        let request = ServerRequest::RegisterWithAddr(ServerRegisterWithAddrRequest {
            request_id,
            public_key: peer_id.to_bytes().to_vec(),
            socket_addr: addr,
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
        self.socket.send_to(&data, self.server_addr).await?;
        Ok(())
    }
}

async fn recv_loop(shared: Arc<Shared>) {
    let mut buf = [0u8; RECV_BUF_SIZE];
    loop {
        let (len, _addr) = match shared.socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };

        let response = match ServerResponse::deserialize(&buf[..len]) {
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
        ServerResponse::IncomingConnection(_) => {}
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
