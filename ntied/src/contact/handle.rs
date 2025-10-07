use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ntied_crypto::PublicKey;
use ntied_transport::{Address, Connection, Error, Transport};
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc, oneshot};

use crate::packet::{
    CallPacket, ChatPacket, ContactAcceptPacket, ContactPacket, ContactProfile,
    ContactRejectPacket, ContactRequestPacket, Packet,
};

use super::ContactListener;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactStatus {
    PendingIncoming,
    PendingOutgoing,
    RejectedIncoming,
    RejectedOutgoing,
    Accepted,
}

#[derive(Clone)]
pub struct ContactHandle {
    inner: Arc<ContactHandleInner>,
}

impl ContactHandle {
    const MAX_PACKETS: usize = 4;

    pub(super) fn new_accepted(
        transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
        address: Address,
        public_key: PublicKey,
        profile: ContactProfile,
        own_profile: ContactProfile,
        own_address: Address,
        listener: Arc<dyn ContactListener>,
    ) -> Self {
        let public_key = Arc::new(Mutex::new(Some(public_key)));
        let status = Arc::new(Mutex::new(ContactStatus::Accepted));
        let connected = Arc::new(AtomicBool::new(false));
        let profile = Arc::new(Mutex::new(Some(profile)));
        let (command_tx, command_rx) = mpsc::channel(Self::MAX_PACKETS);
        let (chat_packet_tx, chat_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let chat_packet_rx = TokioMutex::new(chat_packet_rx);
        let (call_packet_tx, call_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let call_packet_rx = TokioMutex::new(call_packet_rx);
        let main_task = ContactHandleTask {
            transport,
            connection: None,
            address,
            public_key: public_key.clone(),
            status: status.clone(),
            connected: connected.clone(),
            profile: profile.clone(),
            own_profile,
            own_address,
            listener,
            command_rx,
            chat_packet_tx,
            call_packet_tx,
        };
        let main_task = tokio::spawn(main_task.run());
        Self {
            inner: Arc::new(ContactHandleInner {
                address,
                public_key,
                status,
                connected,
                profile,
                command_tx,
                chat_packet_rx,
                call_packet_rx,
                main_task,
            }),
        }
    }

    pub(super) fn new_outgoing(
        transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
        address: Address,
        own_profile: ContactProfile,
        own_address: Address,
        listener: Arc<dyn ContactListener>,
    ) -> Self {
        let public_key = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(ContactStatus::PendingOutgoing));
        let connected = Arc::new(AtomicBool::new(false));
        let profile = Arc::new(Mutex::new(None));
        let (command_tx, command_rx) = mpsc::channel(Self::MAX_PACKETS);
        let (chat_packet_tx, chat_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let chat_packet_rx = TokioMutex::new(chat_packet_rx);
        let (call_packet_tx, call_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let call_packet_rx = TokioMutex::new(call_packet_rx);
        let main_task = ContactHandleTask {
            transport,
            connection: None,
            address,
            public_key: public_key.clone(),
            status: status.clone(),
            connected: connected.clone(),
            profile: profile.clone(),
            own_profile,
            own_address,
            listener,
            command_rx,
            chat_packet_tx,
            call_packet_tx,
        };
        let main_task = tokio::spawn(main_task.run());
        Self {
            inner: Arc::new(ContactHandleInner {
                address,
                public_key,
                status,
                connected,
                profile,
                command_tx,
                chat_packet_rx,
                call_packet_rx,
                main_task,
            }),
        }
    }

    pub(super) fn new_incoming(
        transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
        connection: Connection,
        address: Address,
        own_profile: ContactProfile,
        own_address: Address,
        listener: Arc<dyn ContactListener>,
    ) -> Self {
        let public_key = Arc::new(Mutex::new(Some(connection.peer_public_key().clone())));
        let status = Arc::new(Mutex::new(ContactStatus::PendingIncoming));
        let connected = Arc::new(AtomicBool::new(true));
        let profile = Arc::new(Mutex::new(None));
        let (command_tx, command_rx) = mpsc::channel(Self::MAX_PACKETS);
        let (chat_packet_tx, chat_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let chat_packet_rx = TokioMutex::new(chat_packet_rx);
        let (call_packet_tx, call_packet_rx) = mpsc::channel(Self::MAX_PACKETS);
        let call_packet_rx = TokioMutex::new(call_packet_rx);
        let main_task = ContactHandleTask {
            transport,
            connection: Some(connection),
            address,
            public_key: public_key.clone(),
            status: status.clone(),
            connected: connected.clone(),
            profile: profile.clone(),
            own_profile,
            own_address,
            listener,
            command_rx,
            chat_packet_tx,
            call_packet_tx,
        };
        let main_task = tokio::spawn(main_task.run());
        Self {
            inner: Arc::new(ContactHandleInner {
                address,
                public_key,
                status,
                connected,
                profile,
                command_tx,
                chat_packet_rx,
                call_packet_rx,
                main_task,
            }),
        }
    }

    pub fn address(&self) -> Address {
        self.inner.address
    }

    pub fn public_key(&self) -> Option<PublicKey> {
        let public_key = self.inner.public_key.lock().unwrap();
        public_key.clone()
    }

    pub fn status(&self) -> ContactStatus {
        let status = self.inner.status.lock().unwrap();
        *status
    }

    pub fn profile(&self) -> Option<ContactProfile> {
        let profile = self.inner.profile.lock().unwrap();
        profile.clone()
    }

    pub fn get_name(&self) -> Option<String> {
        self.profile().map(|p| p.name)
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Relaxed)
    }

    pub async fn accept(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .command_tx
            .send(HandleCommand::Accept { tx })
            .await
            .map_err(|_| "Connection is broken".to_string())?;
        rx.await?;
        Ok(())
    }

    pub async fn reject(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .command_tx
            .send(HandleCommand::Reject { tx })
            .await
            .map_err(|_| "Handle is broken".to_string())?;
        rx.await?;
        Ok(())
    }

    pub async fn send_chat_packet(&self, packet: ChatPacket) -> Result<(), Error> {
        self.inner
            .command_tx
            .send(HandleCommand::SendChatPacket(packet))
            .await
            .map_err(|_| "Handle is broken".into())
    }

    pub async fn recv_chat_packet(&self) -> Result<ChatPacket, Error> {
        self.inner
            .chat_packet_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Handle is broken".into())
    }

    pub async fn send_call_packet(&self, packet: CallPacket) -> Result<(), Error> {
        self.inner
            .command_tx
            .send(HandleCommand::SendCallPacket(packet))
            .await
            .map_err(|_| "Handle is broken".into())
    }

    pub async fn recv_call_packet(&self) -> Result<CallPacket, Error> {
        self.inner
            .call_packet_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or("Handle is broken".into())
    }

    pub(super) async fn set_connection(&self, connection: Connection) -> Result<(), Error> {
        self.inner
            .command_tx
            .send(HandleCommand::SetConnection(connection))
            .await?;
        Ok(())
    }
}

struct ContactHandleInner {
    address: Address,
    public_key: Arc<Mutex<Option<PublicKey>>>,
    status: Arc<Mutex<ContactStatus>>,
    connected: Arc<AtomicBool>,
    profile: Arc<Mutex<Option<ContactProfile>>>,
    command_tx: mpsc::Sender<HandleCommand>,
    chat_packet_rx: TokioMutex<mpsc::Receiver<ChatPacket>>,
    call_packet_rx: TokioMutex<mpsc::Receiver<CallPacket>>,
    main_task: tokio::task::JoinHandle<()>,
}

impl Drop for ContactHandleInner {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

enum HandleCommand {
    Accept { tx: oneshot::Sender<()> },
    Reject { tx: oneshot::Sender<()> },
    SetConnection(Connection),
    SendChatPacket(ChatPacket),
    SendCallPacket(CallPacket),
}

struct ContactHandleTask {
    transport: Arc<TokioRwLock<Option<Arc<Transport>>>>,
    connection: Option<Connection>,
    address: Address,
    public_key: Arc<Mutex<Option<PublicKey>>>,
    status: Arc<Mutex<ContactStatus>>,
    connected: Arc<AtomicBool>,
    profile: Arc<Mutex<Option<ContactProfile>>>,
    own_profile: ContactProfile,
    own_address: Address,
    listener: Arc<dyn ContactListener>,
    command_rx: mpsc::Receiver<HandleCommand>,
    chat_packet_tx: mpsc::Sender<ChatPacket>,
    call_packet_tx: mpsc::Sender<CallPacket>,
}

impl ContactHandleTask {
    const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

    pub async fn run(mut self) {
        loop {
            let current_status = *self.status.lock().unwrap();
            tracing::debug!(?current_status, "Enter contact status state");
            match current_status {
                ContactStatus::PendingIncoming => self.pending_incoming_loop().await,
                ContactStatus::PendingOutgoing => self.pending_outgoing_loop().await,
                ContactStatus::RejectedIncoming => self.rejected_incoming_loop().await,
                ContactStatus::RejectedOutgoing => self.rejected_outgoing_loop().await,
                ContactStatus::Accepted => self.accepted_loop().await,
            }
        }
    }

    async fn pending_incoming_loop(&mut self) {
        if !self.accept_connection().await {
            return;
        }
        let connection_mut = self
            .connection
            .as_mut()
            .expect("Unexpected connection state");
        loop {
            tokio::select! {
                command = self.command_rx.recv() => match command {
                    Some(v) => match v {
                        HandleCommand::Accept { tx } => {
                            let packet = Packet::Contact(ContactPacket::Accept(ContactAcceptPacket {
                                profile: self.own_profile.clone(),
                            }));
                            let bytes = bincode::serialize(&packet).unwrap();
                            tracing::debug!("Sending accept packet");
                            if let Err(err) = connection_mut.send(bytes).await {
                                tracing::error!(?err, "Failed to send accept packet");
                            }
                            *self.status.lock().unwrap() = ContactStatus::Accepted;
                            // if let Err(err) = self.event_tx.try_send(ContactEvent::Accepted { address: self.address }) {
                            //     tracing::error!(?err, "Failed to send accepted event");
                            // }
                            if let Err(err) = tx.send(()) {
                                tracing::error!(?err, "Failed to send accept completion");
                            }
                            return;
                        }
                        HandleCommand::Reject { tx } => {
                            let packet = Packet::Contact(ContactPacket::Reject(ContactRejectPacket {}));
                            let bytes = bincode::serialize(&packet).unwrap();
                            tracing::debug!("Sending reject packet");
                            if let Err(err) = connection_mut.send(bytes).await {
                                tracing::error!(?err, "Failed to send reject packet");
                            }
                            *self.status.lock().unwrap() = ContactStatus::RejectedIncoming;
                            self.listener.on_contact_rejected(self.address).await;
                            if let Err(err) = tx.send(()) {
                                tracing::error!(?err, "Failed to send reject completion");
                            }
                            return;
                        }
                        HandleCommand::SetConnection(connection) => {
                            if self.own_address.to_string() < connection.peer_address().to_string() {
                                tracing::debug!("Discard incoming connection");
                                continue;
                            }
                            tracing::debug!("Replace connection");
                            *connection_mut = connection;
                            continue;
                        }
                        _ => {
                            tracing::debug!("Ignoring command");
                        }
                    }
                    None => {
                        tracing::debug!("Command channel closed");
                        return;
                    }
                },
                packet = connection_mut.recv() => match packet {
                    Ok(packet) => {
                        match bincode::deserialize::<Packet>(&packet) {
                            Ok(Packet::Contact(ContactPacket::Request(ContactRequestPacket { profile }))) => {
                                tracing::debug!("Received contact request from {:?}", self.address);
                                *self.profile.lock().unwrap() = Some(profile.clone());
                                self.listener.on_contact_incoming(self.address, profile).await;
                            }
                            Ok(Packet::Contact(ContactPacket::Reject(ContactRejectPacket { }))) => {
                                tracing::debug!("Received contact reject packet");
                                *self.status.lock().unwrap() = ContactStatus::RejectedIncoming;
                                self.listener.on_contact_rejected(self.address).await;
                                return;
                            }
                            Ok(packet) => {
                                tracing::warn!(?packet, "Unexpected packet in pending incoming state");
                            }
                            Err(err) => {
                                tracing::error!(?err, "Failed to parse packet");
                            }
                        }
                    }
                    Err(_) => {
                        self.close_connection().await;
                        return;
                    }
                },
            }
        }
    }

    async fn pending_outgoing_loop(&mut self) {
        if !self.establish_connection().await {
            return;
        }
        let connection_mut = self
            .connection
            .as_mut()
            .expect("Unexpected connection state");
        let packet = Packet::Contact(ContactPacket::Request(ContactRequestPacket {
            profile: self.own_profile.clone(),
        }));
        let bytes = bincode::serialize(&packet).unwrap();
        tracing::debug!("Sending contact request packet");
        if let Err(err) = connection_mut.send(bytes).await {
            tracing::error!(?err, "Failed to send contact request packet");
            self.close_connection().await;
            return;
        } else {
            // if let Err(err) = self.event_tx.try_send(ContactEvent::OutgoingRequest {
            //     address: self.address,
            // }) {
            //     tracing::error!(?err, "Failed to send outgoing request event");
            // }
        }
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let command = match command {
                        Some(v) => v,
                        None => return,
                    };
                    match command {
                        HandleCommand::SetConnection(connection) => {
                            if self.own_address.to_string() < connection.peer_address().to_string() {
                                tracing::debug!("Discard incoming connection");
                                continue;
                            }
                            tracing::debug!("Replace connection");
                            *connection_mut = connection;
                            continue;
                        }
                        HandleCommand::Reject { tx } => {
                            let packet = Packet::Contact(ContactPacket::Reject(ContactRejectPacket {}));
                            let bytes = bincode::serialize(&packet).unwrap();
                            tracing::debug!("Sending reject packet");
                            if let Err(err) = connection_mut.send(bytes).await {
                                tracing::error!(?err, "Failed to send reject packet");
                            }
                            *self.status.lock().unwrap() = ContactStatus::RejectedOutgoing;
                            // if let Err(err) = self.event_tx.try_send(ContactEvent::Rejected { address: self.address }) {
                            //     tracing::error!(?err, "Failed to send rejected event");
                            // }
                            if let Err(err) = tx.send(()) {
                                tracing::error!(?err, "Failed to send reject completion");
                            }
                            return;
                        }
                        _ => {
                            tracing::debug!("Ignoring command");
                        }
                    }
                },
                packet = connection_mut.recv() => match packet {
                    Ok(packet) => {
                        match bincode::deserialize::<Packet>(&packet) {
                            Ok(Packet::Contact(ContactPacket::Accept(ContactAcceptPacket { profile }))) => {
                                tracing::debug!("Received contact accept packet");
                                *self.profile.lock().unwrap() = Some(profile.clone());
                                *self.status.lock().unwrap() = ContactStatus::Accepted;
                                self.listener.on_contact_accepted(self.address, profile).await;
                                return;
                            }
                            Ok(Packet::Contact(ContactPacket::Reject(ContactRejectPacket { }))) => {
                                tracing::debug!("Received contact reject packet");
                                *self.status.lock().unwrap() = ContactStatus::RejectedOutgoing;
                                self.listener.on_contact_rejected(self.address).await;
                                return;
                            }
                            Ok(packet) => {
                                tracing::warn!(?packet, "Unexpected packet in pending outgoing state");
                            }
                            Err(err) => {
                                tracing::error!(?err, "Failed to parse packet");
                            }
                        }
                    }
                    Err(_) => {
                        self.close_connection().await;
                        return;
                    }
                },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // Send contact request periodically
                    let packet = Packet::Contact(ContactPacket::Request(ContactRequestPacket {
                        profile: self.own_profile.clone(),
                    }));
                    let bytes = bincode::serialize(&packet).unwrap();
                    tracing::debug!("Sending contact request packet");
                    if let Err(err) = connection_mut.send(bytes).await {
                        tracing::error!(?err, "Failed to send contact request packet");
                        self.close_connection().await;
                        return;
                    } else {
                        // if let Err(err) = self.event_tx.try_send(ContactEvent::OutgoingRequest {
                        //     address: self.address,
                        // }) {
                        //     tracing::error!(?err, "Failed to send outgoing request event");
                        // }
                    }
                }
            }
        }
    }

    async fn rejected_incoming_loop(&mut self) {
        self.close_connection().await;
        loop {
            let command = match self.command_rx.recv().await {
                Some(v) => v,
                None => return,
            };
            match command {
                HandleCommand::SetConnection(connection) => {
                    let packet = Packet::Contact(ContactPacket::Reject(ContactRejectPacket {}));
                    let bytes = bincode::serialize(&packet).unwrap();
                    tracing::debug!("Sending contact reject packet");
                    if let Err(err) = connection.send(bytes).await {
                        tracing::warn!(err, "Failed to send contact reject packet");
                    }
                    tracing::debug!("Drop connection");
                    drop(connection);
                }
                _ => {
                    tracing::debug!("Ignoring command");
                }
            }
        }
    }

    async fn rejected_outgoing_loop(&mut self) {
        self.close_connection().await;
        loop {
            let command = match self.command_rx.recv().await {
                Some(v) => v,
                None => return,
            };
            match command {
                HandleCommand::SetConnection(connection) => {
                    tracing::debug!("Drop connection");
                    drop(connection);
                }
                _ => {
                    tracing::debug!("Ignoring command");
                }
            }
        }
    }

    async fn accepted_loop(&mut self) {
        if !self.establish_connection().await {
            return;
        }
        let connection_mut = self
            .connection
            .as_mut()
            .expect("Unexpected connection state");
        loop {
            tokio::select! {
                v = self.command_rx.recv() => {
                    let command = match v {
                        Some(v) => v,
                        None => return,
                    };
                    match command {
                        HandleCommand::SendChatPacket(chat_packet) => {
                            let packet = Packet::Chat(chat_packet);
                            let bytes = bincode::serialize(&packet).unwrap();
                            tracing::debug!("Send packet");
                            if let Err(err) = connection_mut.send(bytes).await {
                                tracing::error!(?err, "Failed to send packet");
                            }
                        }
                        HandleCommand::SendCallPacket(call_packet) => {
                            let packet = Packet::Call(call_packet);
                            let bytes = bincode::serialize(&packet).unwrap();
                            tracing::debug!("Send packet");
                            if let Err(err) = connection_mut.send(bytes).await {
                                tracing::error!(?err, "Failed to send packet");
                            }
                        }
                        HandleCommand::SetConnection(connection) => {
                            if self.own_address.to_string() < connection.peer_address().to_string() {
                                tracing::debug!("Discard incoming connection");
                                continue;
                            }
                            tracing::debug!("Replace connection");
                            *connection_mut = connection;
                            continue;
                        }
                        _ => {
                            tracing::debug!("Ignoring command");
                        }
                    }
                },
                packet = connection_mut.recv() => match packet {
                    Ok(packet) => {
                        match bincode::deserialize::<Packet>(&packet) {
                            Ok(Packet::Contact(ContactPacket::Request(ContactRequestPacket { profile }))) => {
                                tracing::debug!("Received contact request packet");
                                *self.profile.lock().unwrap() = Some(profile);
                                let packet = Packet::Contact(ContactPacket::Accept(ContactAcceptPacket {
                                    profile: self.own_profile.clone(),
                                }));
                                let bytes = bincode::serialize(&packet).unwrap();
                                tracing::debug!("Sending contact accept packet");
                                if let Err(err) = connection_mut.send(bytes).await {
                                    tracing::error!(?err, "Failed to send contact accept packet");
                                }
                            }
                            Ok(Packet::Contact(ContactPacket::Accept(ContactAcceptPacket { profile }))) => {
                                tracing::debug!("Received contact accept packet");
                                *self.profile.lock().unwrap() = Some(profile);
                            }
                            Ok(Packet::Chat(chat_packet)) => {
                                if let Err(err) = self.chat_packet_tx.try_send(chat_packet) {
                                    tracing::warn!(?err, "Received chat packet is lost");
                                }
                            }
                            Ok(Packet::Call(call_packet)) => {
                                if let Err(err) = self.call_packet_tx.try_send(call_packet) {
                                    tracing::warn!(?err, "Received call packet is lost");
                                }
                            }
                            Ok(packet) => {
                                tracing::warn!(?packet, "Unexpected packet in accepted state");
                            }
                            Err(err) => {
                                tracing::error!(?err, "Failed to parse packet");
                            }
                        }
                    }
                    Err(_) => {
                        self.close_connection().await;
                        return;
                    }
                },
            }
        }
    }

    async fn establish_connection(&mut self) -> bool {
        if self.connection.is_some() {
            return true;
        }
        let outgoing_connection = async {
            let transport = self.transport.read().await.clone();
            let transport = match transport {
                Some(v) => v,
                None => return std::future::pending().await,
            };
            match transport.connect(self.address).await {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(err, "Failed to connect to peer");
                    std::future::pending().await
                }
            }
        };
        let incoming_connection = async {
            while let Some(v) = self.command_rx.recv().await {
                match v {
                    HandleCommand::SetConnection(connection) => {
                        return connection;
                    }
                    _ => {
                        tracing::debug!("Ignoring command");
                    }
                }
            }
            return std::future::pending().await;
        };
        tracing::debug!("Trying to connect to peer");
        tokio::select! {
            v = outgoing_connection => {
                tracing::debug!("Connected to peer");
                self.set_connection(v).await;
                true
            }
            v = incoming_connection => {
                tracing::debug!("Connection accepted from peer");
                self.set_connection(v).await;
                true
            }
            _ = tokio::time::sleep(Self::CONNECTION_TIMEOUT) => {
                tracing::debug!("Connection timeout");
                false
            }
        }
    }

    async fn accept_connection(&mut self) -> bool {
        if self.connection.is_some() {
            return true;
        }
        let incoming_connection = async {
            while let Some(v) = self.command_rx.recv().await {
                match v {
                    HandleCommand::SetConnection(connection) => {
                        return connection;
                    }
                    _ => {
                        tracing::debug!("Ignoring command");
                    }
                }
            }

            return std::future::pending().await;
        };
        tokio::select! {
            v = incoming_connection => {
                tracing::debug!("Connection accepted from peer");
                self.set_connection(v).await;
                true
            }
            _ = tokio::time::sleep(Self::CONNECTION_TIMEOUT) => {
                tracing::debug!("Connection timeout");
                false
            }
        }
    }

    async fn set_connection(&mut self, connection: Connection) {
        {
            let mut public_key = self.public_key.lock().unwrap();
            *public_key = Some(connection.peer_public_key().clone());
        }
        self.connection = Some(connection);
        self.connected.store(true, Ordering::SeqCst);
        tracing::info!("Connection established");
        self.listener.on_contact_connected(self.address).await;
    }

    async fn close_connection(&mut self) {
        if let Some(connection) = self.connection.take() {
            drop(connection);
            self.connected.store(false, Ordering::SeqCst);
            tracing::info!("Connection closed");
            self.listener.on_contact_disconnected(self.address).await;
        }
    }
}
