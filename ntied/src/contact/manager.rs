use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
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
    pub async fn new(
        server_addr: SocketAddr,
        private_key: PrivateKey,
        own_profile: ContactProfile,
    ) -> Self {
        Self::with_listener(
            server_addr,
            private_key,
            own_profile,
            Arc::new(StubListener),
        )
        .await
    }

    pub async fn with_listener<L>(
        server_addr: SocketAddr,
        private_key: PrivateKey,
        own_profile: ContactProfile,
        listener: Arc<L>,
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
            server_addr,
            private_key.clone(),
            own_peer_id,
            transport.clone(),
            contacts.clone(),
            connected.clone(),
            command_rx,
            accept_tx,
            own_profile.clone(),
            listener.clone(),
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

    pub async fn change_server_addr(&self, server_addr: SocketAddr) -> Result<(), anyhow::Error> {
        self.command_tx
            .send(ManagerCommand::ChangeServerAddr(server_addr))
            .await
            .map_err(|err| anyhow!("Cannot change server addr: {err}"))?;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    async fn main_loop(
        mut server_addr: SocketAddr,
        private_key: PrivateKey,
        own_peer_id: PeerId,
        transport: Arc<TokioRwLock<Option<Arc<NtiedTransport>>>>,
        contacts: Arc<TokioMutex<HashMap<PeerId, ContactHandle>>>,
        connected: Arc<AtomicBool>,
        mut command_rx: mpsc::Receiver<ManagerCommand>,
        accept_tx: mpsc::Sender<PeerId>,
        own_profile: ContactProfile,
        listener: Arc<dyn ContactListener>,
    ) {
        // Create transport once — it survives relay reconnections
        let transport_arc = match NtiedTransport::bind("0.0.0.0:0", private_key.clone()).await {
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

        loop {
            // Drain pending commands
            loop {
                match command_rx.try_recv() {
                    Ok(ManagerCommand::ChangeServerAddr(addr)) => {
                        server_addr = addr;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        tracing::debug!("Stopping main loop");
                        return;
                    }
                }
            }

            // Attach to relay
            tracing::debug!(?server_addr, "Attaching to relay");
            match transport_arc.attach_relay(server_addr).await {
                Ok(()) => {
                    tracing::info!(?server_addr, "Attached to relay");
                    connected.store(true, Ordering::SeqCst);
                    listener.on_server_connected().await;
                }
                Err(err) => {
                    tracing::error!(?err, ?server_addr, "Failed to attach to relay");
                    if connected.swap(false, Ordering::SeqCst) {
                        listener.on_server_disconnected().await;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
            }

            // Accept loop — runs until relay breaks or server addr changes
            let mut relay_check = tokio::time::interval(std::time::Duration::from_secs(5));
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
                                tracing::error!(?err, "Failed to accept connection");
                                if connected.swap(false, Ordering::SeqCst) {
                                    listener.on_server_disconnected().await;
                                }
                                // Retry relay attachment
                                break;
                            }
                        }
                    }
                    _ = relay_check.tick() => {
                        if !transport_arc.is_relay_attached().await {
                            tracing::warn!("Relay lost, re-attaching");
                            if connected.swap(false, Ordering::SeqCst) {
                                listener.on_server_disconnected().await;
                            }
                            break; // go to outer loop → re-attach
                        }
                    }
                    v = command_rx.recv() => {
                        match v {
                            Some(ManagerCommand::ChangeServerAddr(addr)) => {
                                tracing::info!(?addr, "Changing relay address");
                                server_addr = addr;
                                // Re-attach to new relay (existing connections survive)
                                break;
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
}

impl Drop for ContactManager {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

enum ManagerCommand {
    ChangeServerAddr(SocketAddr),
}
