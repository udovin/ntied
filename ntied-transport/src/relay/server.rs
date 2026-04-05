use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::crypto::{PeerId, PrivateKey};
use crate::node::{Connection, DatagramChannel, Node};

use super::protocol::{RelayMessage, PURPOSE_RELAY};

/// Per-client state tracked by the relay.
struct ClientState {
    datagram: DatagramChannel,
    /// The external address from which this client connects (UDP source addr).
    external_addr: Option<SocketAddr>,
}

pub struct RelayNode {
    node: Node,
    clients: Arc<Mutex<HashMap<PeerId, ClientState>>>,
    client_tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for RelayNode {
    fn drop(&mut self) {
        for task in self.client_tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

impl RelayNode {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        let node = Node::bind(addr, identity).await?;
        Ok(Self {
            node,
            clients: Arc::new(Mutex::new(HashMap::new())),
            client_tasks: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.node.local_addr()
    }

    pub fn peer_id(&self) -> PeerId {
        self.node.peer_id()
    }

    /// Run the relay accept loop. Spawns a task per client.
    pub async fn run(&self) {
        loop {
            let conn = match self.node.accept().await {
                Ok(c) => c,
                Err(e) => {
                    warn!("relay accept error: {e}");
                    continue;
                }
            };

            let clients = self.clients.clone();
            let task = tokio::spawn(async move {
                if let Err(e) = handle_client(conn, clients).await {
                    debug!("relay client disconnected: {e}");
                }
            });
            {
                let mut tasks = self.client_tasks.lock().unwrap();
                tasks.retain(|t| !t.is_finished());
                tasks.push(task);
            }
        }
    }
}

async fn handle_client(
    conn: Connection,
    clients: Arc<Mutex<HashMap<PeerId, ClientState>>>,
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

    info!(%peer_id, ?external_addr, "relay: client registered");

    let welcome = RelayMessage::Welcome {
        external_addr: external_addr.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap()),
    };
    datagram.send(&welcome.encode()).await?;

    {
        let mut map = clients.lock().await;
        map.insert(
            peer_id,
            ClientState {
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
                let dest_channel = {
                    let map = clients.lock().await;
                    map.get(&dest).map(|cs| cs.datagram.clone())
                };
                if let Some(dest_ch) = dest_channel {
                    let fwd = RelayMessage::Tunnel {
                        peer_id,
                        data: inner,
                    };
                    if let Err(e) = dest_ch.send(&fwd.encode()).await {
                        debug!(%dest, "relay: forward failed: {e}");
                    }
                } else {
                    debug!(%dest, "relay: destination not found");
                }
            }
            RelayMessage::HolePunchRequest { target } => {
                debug!(%peer_id, %target, "relay: hole punch request");
                let map = clients.lock().await;

                let requester_addrs: Vec<SocketAddr> = map
                    .get(&peer_id)
                    .and_then(|cs| cs.external_addr)
                    .into_iter()
                    .collect();

                let target_state = map.get(&target).map(|cs| {
                    (cs.datagram.clone(), cs.external_addr)
                });

                drop(map);

                if let Some((target_ch, target_addr)) = target_state {
                    let notify_target = RelayMessage::HolePunchNotify {
                        requester: peer_id,
                        addrs: requester_addrs,
                    };
                    if let Err(e) = target_ch.send(&notify_target.encode()).await {
                        debug!(%target, "relay: hole punch notify to target failed: {e}");
                    }

                    let target_addrs: Vec<SocketAddr> =
                        target_addr.into_iter().collect();
                    let notify_requester = RelayMessage::HolePunchNotify {
                        requester: target,
                        addrs: target_addrs,
                    };
                    if let Err(e) = datagram.send(&notify_requester.encode()).await {
                        debug!(%peer_id, "relay: hole punch notify to requester failed: {e}");
                    }
                } else {
                    debug!(%target, "relay: hole punch target not found");
                }
            }
            _ => {}
        }
    }

    {
        let mut map = clients.lock().await;
        map.remove(&peer_id);
    }
    info!(%peer_id, "relay: client disconnected");

    Ok(())
}
