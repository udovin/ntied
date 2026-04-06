use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use ntied_transport::relay::protocol::{RelayMessage, PURPOSE_RELAY};
use ntied_transport::{Node, PeerId, PrivateKey, RelayNode};

// ── Tracing ──

static TRACING_INIT: Once = Once::new();

pub fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_target(false)
            .with_test_writer()
            .init();
    });
}

// ── Addresses ──

pub fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

// ── Node pair helper ──

pub struct DirectPair {
    pub node_a: Node,
    pub node_b: Arc<Node>,
    pub conn_a: ntied_transport::Connection,
    pub conn_b: ntied_transport::Connection,
}

pub async fn connect_direct() -> DirectPair {
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    DirectPair {
        node_a,
        node_b,
        conn_a,
        conn_b,
    }
}

// ── Relay pair helper ──

pub struct RelayPair {
    pub node_a: Node,
    pub node_b: Arc<Node>,
    pub conn_a: ntied_transport::Connection,
    pub conn_b: ntied_transport::Connection,
    pub relay_task: JoinHandle<()>,
    pub relay_addr: SocketAddr,
}

pub async fn connect_via_relay() -> RelayPair {
    let relay = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    RelayPair {
        node_a,
        node_b,
        conn_a,
        conn_b,
        relay_task,
        relay_addr,
    }
}

// ── Payload helpers ──

pub fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for &b in data {
        sum = sum.wrapping_add(b as u32).wrapping_mul(31);
    }
    sum
}

pub fn make_payload(seq: u32, size: usize) -> Vec<u8> {
    let mut inner = Vec::with_capacity(4 + size + 4);
    inner.extend_from_slice(&seq.to_be_bytes());
    for i in 0..size {
        inner.push(((seq as usize * 7 + i * 13) & 0xFF) as u8);
    }
    let cs = checksum(&inner);
    inner.extend_from_slice(&cs.to_be_bytes());
    inner
}

pub fn verify_payload(data: &[u8]) -> Option<u32> {
    if data.len() < 8 {
        return None;
    }
    let seq = u32::from_be_bytes(data[..4].try_into().ok()?);
    let expected_cs = u32::from_be_bytes(data[data.len() - 4..].try_into().ok()?);
    let actual_cs = checksum(&data[..data.len() - 4]);
    if actual_cs != expected_cs {
        return None;
    }
    Some(seq)
}

/// Wrap payload with length prefix for stream (byte stream has no message boundaries).
pub fn frame_message(payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Read length-prefixed messages from a stream buffer.
pub struct StreamReader {
    buffer: Vec<u8>,
}

impl StreamReader {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    pub fn try_read(&mut self) -> Option<Vec<u8>> {
        if self.buffer.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        if self.buffer.len() < 4 + len {
            return None;
        }
        let msg = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(..4 + len);
        Some(msg)
    }
}

// ── Lossy relay ──

/// A relay node that randomly drops a configurable fraction of tunnel messages.
/// Implements the relay protocol manually for test purposes.
pub struct LossyRelayNode {
    pub node: Node,
    pub drop_rate: f64,
    /// Number of messages to forward without loss (allows handshake to complete).
    pub grace_messages: u64,
    pub dropped: Arc<AtomicU64>,
    pub forwarded: Arc<AtomicU64>,
}

struct LossyClientState {
    datagram: ntied_transport::DatagramChannel,
    external_addr: Option<SocketAddr>,
}

impl LossyRelayNode {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey, drop_rate: f64) -> io::Result<Self> {
        let node = Node::bind(addr, identity).await?;
        Ok(Self {
            node,
            drop_rate,
            // Allow 50 messages through without loss to let handshakes complete
            grace_messages: 50,
            dropped: Arc::new(AtomicU64::new(0)),
            forwarded: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.node.local_addr()
    }

    pub fn stats(&self) -> (u64, u64) {
        (
            self.forwarded.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
        )
    }

    pub async fn run(&self) {
        let clients: Arc<Mutex<HashMap<PeerId, LossyClientState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        loop {
            let conn = match self.node.accept().await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let clients = clients.clone();
            let drop_rate = self.drop_rate;
            let grace_messages = self.grace_messages;
            let dropped = self.dropped.clone();
            let forwarded = self.forwarded.clone();

            tokio::spawn(async move {
                let _ =
                    handle_lossy_client(conn, clients, drop_rate, grace_messages, dropped, forwarded)
                        .await;
            });
        }
    }
}

async fn handle_lossy_client(
    conn: ntied_transport::Connection,
    clients: Arc<Mutex<HashMap<PeerId, LossyClientState>>>,
    drop_rate: f64,
    grace_messages: u64,
    dropped: Arc<AtomicU64>,
    forwarded: Arc<AtomicU64>,
) -> io::Result<()> {
    let (datagram, purpose) = conn.accept_datagram().await?;
    if purpose != PURPOSE_RELAY {
        conn.close().await?;
        return Ok(());
    }

    let peer_id = conn
        .peer_id()
        .await
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no peer id"))?;

    let external_addr = conn.remote_addr().await;

    let welcome = RelayMessage::Welcome {
        external_addr: external_addr.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap()),
    };
    datagram.send(&welcome.encode()).await?;

    {
        let mut map = clients.lock().await;
        map.insert(
            peer_id,
            LossyClientState {
                datagram: datagram.clone(),
                external_addr,
            },
        );
    }

    loop {
        let data = match datagram.recv().await {
            Ok(d) => d,
            Err(_) => break,
        };

        let msg = match RelayMessage::decode(&data) {
            Some(m) => m,
            None => continue,
        };

        match msg {
            RelayMessage::Tunnel {
                peer_id: dest,
                data: inner,
            } => {
                // Only start dropping after grace period (lets handshake complete)
                let total = forwarded.load(Ordering::Relaxed) + dropped.load(Ordering::Relaxed);
                if total >= grace_messages && rand::random::<f64>() < drop_rate {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                let dest_channel = {
                    let map = clients.lock().await;
                    map.get(&dest).map(|cs| cs.datagram.clone())
                };
                if let Some(dest_ch) = dest_channel {
                    let fwd = RelayMessage::Tunnel {
                        peer_id,
                        data: inner,
                    };
                    let _ = dest_ch.send(&fwd.encode()).await;
                    forwarded.fetch_add(1, Ordering::Relaxed);
                }
            }
            RelayMessage::HolePunchRequest { target } => {
                let map = clients.lock().await;
                let requester_addrs: Vec<SocketAddr> = map
                    .get(&peer_id)
                    .and_then(|cs| cs.external_addr)
                    .into_iter()
                    .collect();
                let target_state = map.get(&target).map(|cs| (cs.datagram.clone(), cs.external_addr));
                drop(map);

                if let Some((target_ch, target_addr)) = target_state {
                    let notify_target = RelayMessage::HolePunchNotify {
                        requester: peer_id,
                        addrs: requester_addrs,
                    };
                    let _ = target_ch.send(&notify_target.encode()).await;

                    let target_addrs: Vec<SocketAddr> = target_addr.into_iter().collect();
                    let notify_requester = RelayMessage::HolePunchNotify {
                        requester: target,
                        addrs: target_addrs,
                    };
                    let _ = datagram.send(&notify_requester.encode()).await;
                }
            }
            _ => {}
        }
    }

    {
        let mut map = clients.lock().await;
        map.remove(&peer_id);
    }

    Ok(())
}

// ── Disconnecting relay ──

/// A relay that stops forwarding after a configurable number of messages,
/// simulating a connection drop.
pub struct DisconnectingRelayNode {
    pub node: Node,
    pub forward_limit: u64,
    pub total_forwarded: Arc<AtomicU64>,
}

impl DisconnectingRelayNode {
    pub async fn bind(
        addr: SocketAddr,
        identity: PrivateKey,
        forward_limit: u64,
    ) -> io::Result<Self> {
        let node = Node::bind(addr, identity).await?;
        Ok(Self {
            node,
            forward_limit,
            total_forwarded: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.node.local_addr()
    }

    pub async fn run(&self) {
        let clients: Arc<Mutex<HashMap<PeerId, LossyClientState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        loop {
            let conn = match self.node.accept().await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let clients = clients.clone();
            let forward_limit = self.forward_limit;
            let total_forwarded = self.total_forwarded.clone();

            tokio::spawn(async move {
                let _ = handle_disconnecting_client(conn, clients, forward_limit, total_forwarded)
                    .await;
            });
        }
    }
}

async fn handle_disconnecting_client(
    conn: ntied_transport::Connection,
    clients: Arc<Mutex<HashMap<PeerId, LossyClientState>>>,
    forward_limit: u64,
    total_forwarded: Arc<AtomicU64>,
) -> io::Result<()> {
    let (datagram, purpose) = conn.accept_datagram().await?;
    if purpose != PURPOSE_RELAY {
        conn.close().await?;
        return Ok(());
    }

    let peer_id = conn
        .peer_id()
        .await
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no peer id"))?;
    let external_addr = conn.remote_addr().await;

    let welcome = RelayMessage::Welcome {
        external_addr: external_addr.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap()),
    };
    datagram.send(&welcome.encode()).await?;

    {
        let mut map = clients.lock().await;
        map.insert(
            peer_id,
            LossyClientState {
                datagram: datagram.clone(),
                external_addr,
            },
        );
    }

    loop {
        let data = match datagram.recv().await {
            Ok(d) => d,
            Err(_) => break,
        };

        let msg = match RelayMessage::decode(&data) {
            Some(m) => m,
            None => continue,
        };

        match msg {
            RelayMessage::Tunnel {
                peer_id: dest,
                data: inner,
            } => {
                let count = total_forwarded.fetch_add(1, Ordering::Relaxed);
                if count >= forward_limit {
                    // Simulate disconnect by silently dropping
                    continue;
                }
                let dest_channel = {
                    let map = clients.lock().await;
                    map.get(&dest).map(|cs| cs.datagram.clone())
                };
                if let Some(dest_ch) = dest_channel {
                    let fwd = RelayMessage::Tunnel {
                        peer_id,
                        data: inner,
                    };
                    let _ = dest_ch.send(&fwd.encode()).await;
                }
            }
            RelayMessage::HolePunchRequest { target } => {
                let map = clients.lock().await;
                let requester_addrs: Vec<SocketAddr> = map
                    .get(&peer_id)
                    .and_then(|cs| cs.external_addr)
                    .into_iter()
                    .collect();
                let target_state = map.get(&target).map(|cs| (cs.datagram.clone(), cs.external_addr));
                drop(map);

                if let Some((target_ch, target_addr)) = target_state {
                    let notify_target = RelayMessage::HolePunchNotify {
                        requester: peer_id,
                        addrs: requester_addrs,
                    };
                    let _ = target_ch.send(&notify_target.encode()).await;
                    let target_addrs: Vec<SocketAddr> = target_addr.into_iter().collect();
                    let notify_requester = RelayMessage::HolePunchNotify {
                        requester: target,
                        addrs: target_addrs,
                    };
                    let _ = datagram.send(&notify_requester.encode()).await;
                }
            }
            _ => {}
        }
    }

    {
        let mut map = clients.lock().await;
        map.remove(&peer_id);
    }
    Ok(())
}
