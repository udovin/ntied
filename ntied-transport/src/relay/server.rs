use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::crypto::{PeerId, PrivateKey};
use crate::node::{Connection, DatagramChannel, Node};

use super::protocol::{RelayMessage, PURPOSE_RELAY};

pub struct RelayNode {
    node: Node,
    clients: Arc<Mutex<HashMap<PeerId, DatagramChannel>>>,
}

impl RelayNode {
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self> {
        let node = Node::bind(addr, identity).await?;
        Ok(Self {
            node,
            clients: Arc::new(Mutex::new(HashMap::new())),
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
            tokio::spawn(async move {
                if let Err(e) = handle_client(conn, clients).await {
                    debug!("relay client disconnected: {e}");
                }
            });
        }
    }
}

async fn handle_client(
    conn: Connection,
    clients: Arc<Mutex<HashMap<PeerId, DatagramChannel>>>,
) -> io::Result<()> {
    // Wait for relay channel
    let (datagram, purpose) = conn.accept_datagram().await?;
    if purpose != PURPOSE_RELAY {
        conn.close().await?;
        return Ok(());
    }

    let peer_id = conn
        .peer_id()
        .await
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no peer id"))?;

    info!(%peer_id, "relay: client registered");

    // Send welcome
    let welcome = RelayMessage::Welcome {
        external_addr: "0.0.0.0:0".parse().unwrap(),
    };
    datagram.send(&welcome.encode()).await?;

    // Register client's datagram channel for forwarding
    {
        let mut map = clients.lock().await;
        map.insert(peer_id, datagram.clone());
    }

    // Process relay messages from this client
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
                // Forward to destination client
                let dest_channel = {
                    let map = clients.lock().await;
                    map.get(&dest).cloned()
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
                debug!(%peer_id, %target, "relay: hole punch request (not implemented)");
            }
            _ => {}
        }
    }

    // Unregister
    {
        let mut map = clients.lock().await;
        map.remove(&peer_id);
    }
    info!(%peer_id, "relay: client disconnected");

    Ok(())
}
