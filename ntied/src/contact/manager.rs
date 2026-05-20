use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use ntied_transport::node::DiscoveryConfig;
use ntied_transport::{PeerId, PrivateKey};
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc};
use tokio::task::JoinHandle;

use crate::packet::ContactProfile;
use crate::transport::NtiedTransport;

use super::{ContactHandle, ContactListener, StubListener};

#[derive(Clone, Debug)]
pub struct ContactInfo {
    pub peer_id: PeerId,
    pub connected: bool,
    pub name: String,
}

pub struct ContactManager {
    transport: Arc<TokioRwLock<Option<Arc<NtiedTransport>>>>,
    private_key: PrivateKey,
    own_profile: ContactProfile,
    contacts: Arc<TokioMutex<HashMap<PeerId, ContactHandle>>>,
    connected: Arc<AtomicBool>,
    command_tx: mpsc::Sender<ManagerCommand>,
    accept_rx: TokioMutex<mpsc::Receiver<PeerId>>,
    main_task: JoinHandle<()>,
    listener: Arc<dyn ContactListener>,
}

impl ContactManager {
    /// Default-discovery constructor (real mainline DHT).  No relay is
    /// attached — call [`attach_relay`](Self::attach_relay) if you have
    /// one.  Outbound `connect_contact` still works via DHT discovery so
    /// long as the target peer is published (directly or via *some*
    /// relay).
    pub async fn new(private_key: PrivateKey, own_profile: ContactProfile) -> Self {
        Self::with_listener_and_discovery(
            private_key,
            own_profile,
            Arc::new(StubListener),
            DiscoveryConfig::default(),
        )
        .await
    }

    /// Construct with a custom `DiscoveryConfig` (e.g. a test running
    /// against `mainline::Testnet`) and the default stub listener.
    pub async fn with_discovery(
        private_key: PrivateKey,
        own_profile: ContactProfile,
        discovery_config: DiscoveryConfig,
    ) -> Self {
        Self::with_listener_and_discovery(
            private_key,
            own_profile,
            Arc::new(StubListener),
            discovery_config,
        )
        .await
    }

    pub async fn with_listener<L>(
        private_key: PrivateKey,
        own_profile: ContactProfile,
        listener: Arc<L>,
    ) -> Self
    where
        L: ContactListener + 'static,
    {
        Self::with_listener_and_discovery(
            private_key,
            own_profile,
            listener,
            DiscoveryConfig::default(),
        )
        .await
    }

    /// Full-control constructor with both listener and discovery config.
    pub async fn with_listener_and_discovery<L>(
        private_key: PrivateKey,
        own_profile: ContactProfile,
        listener: Arc<L>,
        discovery_config: DiscoveryConfig,
    ) -> Self
    where
        L: ContactListener + 'static,
    {
        let contacts = Arc::new(TokioMutex::new(HashMap::new()));
        let transport = Arc::new(TokioRwLock::new(None));
        let connected = Arc::new(AtomicBool::new(false));
        let (command_tx, command_rx) = mpsc::channel(1);
        let (accept_tx, accept_rx) = mpsc::channel(1);
        let accept_rx = TokioMutex::new(accept_rx);
        let own_peer_id = private_key.public_key().peer_id();
        let main_task = tokio::spawn(Self::main_loop(
            private_key.clone(),
            own_peer_id,
            transport.clone(),
            contacts.clone(),
            connected.clone(),
            command_rx,
            accept_tx,
            own_profile.clone(),
            listener.clone(),
            discovery_config,
        ));
        Self {
            transport,
            private_key,
            own_profile,
            contacts,
            connected,
            command_tx,
            accept_rx,
            main_task,
            listener,
        }
    }

    pub fn get_own_peer_id(&self) -> PeerId {
        self.private_key.public_key().peer_id()
    }

    pub async fn add_contact(&self, peer_id: PeerId, profile: ContactProfile) -> ContactHandle {
        let mut contacts = self.contacts.lock().await;
        match contacts.entry(peer_id) {
            hash_map::Entry::Occupied(entry) => entry.get().clone(),
            hash_map::Entry::Vacant(entry) => {
                let own_peer_id = self.private_key.public_key().peer_id();
                let handle = ContactHandle::new_accepted(
                    self.transport.clone(),
                    peer_id,
                    profile,
                    self.own_profile.clone(),
                    own_peer_id,
                    self.listener.clone(),
                );
                entry.insert(handle.clone());
                handle
            }
        }
    }

    pub async fn connect_contact(&self, peer_id: PeerId) -> ContactHandle {
        let mut contacts = self.contacts.lock().await;
        match contacts.entry(peer_id) {
            hash_map::Entry::Occupied(entry) => entry.get().clone(),
            hash_map::Entry::Vacant(entry) => {
                let own_peer_id = self.private_key.public_key().peer_id();
                let handle = ContactHandle::new_outgoing(
                    self.transport.clone(),
                    peer_id,
                    self.own_profile.clone(),
                    own_peer_id,
                    self.listener.clone(),
                );
                entry.insert(handle.clone());
                handle
            }
        }
    }

    pub async fn remove_contact(&self, peer_id: PeerId) -> Option<ContactHandle> {
        let mut contacts = self.contacts.lock().await;
        contacts.remove(&peer_id)
    }

    pub async fn list_contacts(&self) -> Vec<ContactHandle> {
        let mut result = Vec::new();
        let contacts = self.contacts.lock().await;
        for contact in contacts.values() {
            result.push(contact.clone());
        }
        result
    }

    pub async fn on_incoming_peer_id(&self) -> Result<PeerId, anyhow::Error> {
        self.accept_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(anyhow!("Cannot accept incoming contact"))
    }

    /// Attach a relay at runtime.  Idempotent.  If a different relay was
    /// previously attached as the primary, this *replaces* it (the old
    /// one is detached from the transport).
    pub async fn attach_relay(&self, server_addr: SocketAddr) -> Result<(), anyhow::Error> {
        self.command_tx
            .send(ManagerCommand::AttachRelay(server_addr))
            .await
            .map_err(|err| anyhow!("Cannot attach relay: {err}"))?;
        Ok(())
    }

    /// Detach the current primary relay (if any).  After this, outbound
    /// connectivity relies entirely on DHT discovery.
    pub async fn detach_relay(&self) -> Result<(), anyhow::Error> {
        self.command_tx
            .send(ManagerCommand::DetachRelay)
            .await
            .map_err(|err| anyhow!("Cannot detach relay: {err}"))?;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    async fn main_loop(
        private_key: PrivateKey,
        own_peer_id: PeerId,
        transport: Arc<TokioRwLock<Option<Arc<NtiedTransport>>>>,
        contacts: Arc<TokioMutex<HashMap<PeerId, ContactHandle>>>,
        connected: Arc<AtomicBool>,
        mut command_rx: mpsc::Receiver<ManagerCommand>,
        accept_tx: mpsc::Sender<PeerId>,
        own_profile: ContactProfile,
        listener: Arc<dyn ContactListener>,
        discovery_config: DiscoveryConfig,
    ) {
        // Create transport once — it survives relay reconnections.
        let transport_arc = match NtiedTransport::bind_with_discovery(
            "0.0.0.0:0",
            private_key.clone(),
            discovery_config,
        )
        .await
        {
            Ok(v) => Arc::new(v),
            Err(err) => {
                tracing::error!(?err, "Failed to bind transport");
                return;
            }
        };
        {
            let mut transport_guard = transport.write().await;
            *transport_guard = Some(transport_arc.clone());
        }

        // Primary relay address (single-slot).  `None` means we rely
        // entirely on DHT discovery for outbound and accept incoming
        // only via paths the transport already knows (direct + any
        // relays attached out-of-band on the transport itself).
        let mut primary_relay: Option<SocketAddr> = None;

        // Connection indicator: flip true/false on `has_live_relay` edges.
        // Polling cadence is 1 s; the actual underlying connect/disconnect
        // is event-driven inside the transport — this loop just lifts the
        // state into the manager-level `connected` flag + listener events.
        let mut relay_check = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                v = transport_arc.accept() => {
                    match v {
                        Ok(connection) => {
                            let peer_id = match connection.peer_id() {
                                Some(id) => *id,
                                None => {
                                    tracing::warn!("Accepted connection with unknown peer_id");
                                    continue;
                                }
                            };
                            let mut contacts_guard = contacts.lock().await;
                            match contacts_guard.entry(peer_id) {
                                hash_map::Entry::Occupied(entry) => {
                                    let handle = entry.get().clone();
                                    drop(contacts_guard);
                                    if let Err(err) = handle.set_connection(connection).await {
                                        tracing::warn!(?peer_id, ?err, "Failed to set connection");
                                    }
                                }
                                hash_map::Entry::Vacant(entry) => {
                                    let handle = ContactHandle::new_incoming(
                                        transport.clone(),
                                        connection,
                                        own_profile.clone(),
                                        own_peer_id,
                                        listener.clone(),
                                    );
                                    let peer_id = handle.peer_id().unwrap();
                                    entry.insert(handle);
                                    drop(contacts_guard);
                                    if let Err(err) = accept_tx.try_send(peer_id) {
                                        tracing::warn!(?peer_id, ?err, "Failed to send incoming connection");
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            // Transport accept channel closed — Node is
                            // shutting down.  Drop the indicator and exit.
                            tracing::error!(?err, "transport accept failed, exiting");
                            if connected.swap(false, Ordering::SeqCst) {
                                listener.on_server_disconnected().await;
                            }
                            return;
                        }
                    }
                }
                _ = relay_check.tick() => {
                    let live = transport_arc.has_live_relay().await;
                    let was = connected.load(Ordering::SeqCst);
                    if live && !was {
                        connected.store(true, Ordering::SeqCst);
                        tracing::info!("Relay connection up");
                        listener.on_server_connected().await;
                    } else if !live && was {
                        connected.store(false, Ordering::SeqCst);
                        tracing::warn!("Relay connection down");
                        listener.on_server_disconnected().await;
                    }
                }
                v = command_rx.recv() => {
                    match v {
                        Some(ManagerCommand::AttachRelay(addr)) => {
                            tracing::info!(?addr, "Attaching relay");
                            // Replace any current primary.
                            if let Some(old) = primary_relay {
                                if old != addr {
                                    let _ = transport_arc.detach_relay(old).await;
                                }
                            }
                            if let Err(err) = transport_arc.attach_relay(addr).await {
                                tracing::error!(?err, ?addr, "attach_relay failed");
                            }
                            primary_relay = Some(addr);
                        }
                        Some(ManagerCommand::DetachRelay) => {
                            if let Some(addr) = primary_relay.take() {
                                tracing::info!(?addr, "Detaching relay");
                                let _ = transport_arc.detach_relay(addr).await;
                            }
                        }
                        None => {
                            tracing::debug!("Stopping main loop");
                            return;
                        }
                    }
                }
            }
        }
    }
}

impl Drop for ContactManager {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

enum ManagerCommand {
    AttachRelay(SocketAddr),
    DetachRelay,
}
