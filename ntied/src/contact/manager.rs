use std::collections::{HashMap, hash_map};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use ntied_crypto::{PrivateKey, PublicKey};
use ntied_transport::Transport;
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc};
use tokio::task::JoinHandle;

use crate::packet::ContactProfile;

use super::{ContactHandle, ContactListener, StubListener};

#[derive(Clone, Debug)]
pub struct ContactInfo {
    pub public_key: PublicKey,
    pub connected: bool,
    pub name: String,
}

pub struct ContactManager {
    transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
    private_key: PrivateKey,
    own_profile: ContactProfile,
    contacts: Arc<TokioMutex<HashMap<PublicKey, ContactHandle>>>,
    connected: Arc<AtomicBool>,
    command_tx: mpsc::Sender<ManagerCommand>,
    accept_rx: TokioMutex<mpsc::Receiver<PublicKey>>,
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
        let main_task = tokio::spawn(Self::main_loop(
            server_addr,
            private_key.clone(),
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

    pub fn get_own_public_key(&self) -> PublicKey {
        self.private_key.public_key()
    }

    pub async fn add_contact(
        &self,
        public_key: PublicKey,
        profile: ContactProfile,
    ) -> ContactHandle {
        let mut contacts = self.contacts.lock().await;
        match contacts.entry(public_key) {
            hash_map::Entry::Occupied(entry) => entry.get().clone(),
            hash_map::Entry::Vacant(entry) => {
                let handle = ContactHandle::new_accepted(
                    self.transport.clone(),
                    public_key,
                    profile,
                    self.own_profile.clone(),
                    self.private_key.public_key(),
                    self.listener.clone(),
                );
                entry.insert(handle.clone());
                handle
            }
        }
    }

    pub async fn connect_contact(&self, public_key: PublicKey) -> ContactHandle {
        let mut contacts = self.contacts.lock().await;
        match contacts.entry(public_key) {
            hash_map::Entry::Occupied(entry) => entry.get().clone(),
            hash_map::Entry::Vacant(entry) => {
                let handle = ContactHandle::new_outgoing(
                    self.transport.clone(),
                    public_key,
                    self.own_profile.clone(),
                    self.private_key.public_key(),
                    self.listener.clone(),
                );
                entry.insert(handle.clone());
                handle
            }
        }
    }

    pub async fn remove_contact(&self, public_key: PublicKey) -> Option<ContactHandle> {
        let mut contacts = self.contacts.lock().await;
        contacts.remove(&public_key)
    }

    pub async fn list_contacts(&self) -> Vec<ContactHandle> {
        let mut result = Vec::new();
        let contacts = self.contacts.lock().await;
        for contact in contacts.values() {
            result.push(contact.clone());
        }
        result
    }

    pub async fn on_incoming_public_key(&self) -> Result<PublicKey, anyhow::Error> {
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
        transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
        contacts: Arc<TokioMutex<HashMap<PublicKey, ContactHandle>>>,
        connected: Arc<AtomicBool>,
        mut command_rx: mpsc::Receiver<ManagerCommand>,
        accept_tx: mpsc::Sender<PublicKey>,
        own_profile: ContactProfile,
        listener: Arc<dyn ContactListener>,
    ) {
        let own_public_key = private_key.public_key();
        loop {
            if connected.swap(false, Ordering::SeqCst) {
                tracing::debug!("Server connection is lost");
                listener.on_server_disconnected().await;
            }
            loop {
                match command_rx.try_recv() {
                    Ok(v) => match v {
                        ManagerCommand::ChangeServerAddr(addr) => {
                            server_addr = addr;
                        }
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        tracing::debug!("Stopping main loop");
                        return;
                    }
                }
            }
            tracing::debug!(?server_addr, "Connecting to server");
            let transport_arc =
                match Transport::bind("0.0.0.0:0", private_key.clone(), server_addr).await {
                    Ok(v) => Arc::new(v),
                    Err(err) => {
                        tracing::error!(?err, "Failed to connect to server");
                        continue;
                    }
                };
            tracing::debug!("Connected to server");
            {
                let mut transport_guard = transport.write().await;
                *transport_guard = Some(transport_arc.clone());
            }
            connected.store(true, Ordering::SeqCst);
            listener.on_server_connected().await;
            loop {
                tokio::select! {
                    v = transport_arc.accept() => {
                        match v {
                            Ok(connection) => {
                                let public_key = connection.peer_public_key().clone();
                                let mut contacts_guard = contacts.lock().await;
                                match contacts_guard.entry(public_key) {
                                    hash_map::Entry::Occupied(entry) => {
                                        let handle = entry.get().clone();
                                        drop(contacts_guard);
                                        if let Err(err) = handle.set_connection(connection).await {
                                            tracing::warn!(?public_key, ?err, "Failed to set connection");
                                        }
                                    }
                                    hash_map::Entry::Vacant(entry) => {
                                        let handle = ContactHandle::new_incoming(
                                            transport.clone(),
                                            connection,
                                            own_profile.clone(),
                                            own_public_key,
                                            listener.clone(),
                                        );
                                        let public_key = handle.public_key().unwrap();
                                        entry.insert(handle);
                                        drop(contacts_guard);
                                        if let Err(err) = accept_tx.try_send(public_key) {
                                            tracing::warn!(?public_key, ?err, "Failed to send incoming connection");
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::error!(?err, "Failed to accept connection");
                                listener.on_server_disconnected().await;
                                break;
                            }
                        }
                    }
                    v = command_rx.recv() => {
                        match v {
                            Some(v) => match v {
                                ManagerCommand::ChangeServerAddr(addr) => {
                                    tracing::debug!(?addr, "Changing server address");
                                    server_addr = addr;
                                    break;
                                }
                            },
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
