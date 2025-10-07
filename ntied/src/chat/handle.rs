use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use ntied_transport::Address;
use rand::Rng as _;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_sqlite::Value;
use uuid::Uuid;

use crate::contact::ContactHandle;
use crate::models::{ColumnIndex, Contact, DateTime, Message, MessageKind};
use crate::packet::{
    ChatConflictPacket, ChatMessageAckPacket, ChatMessageKind, ChatMessagePacket, ChatPacket,
};
use crate::storage::Storage;

use super::ChatListener;

#[derive(Clone)]
pub struct ChatHandle {
    inner: Arc<ChatHandleInner>,
}

impl ChatHandle {
    const MAX_PACKETS: usize = 4;

    pub fn new(
        contact_handle: ContactHandle,
        contact: Contact,
        storage: Arc<TokioMutex<Storage>>,
        listener: Arc<dyn ChatListener>,
    ) -> Self {
        let contact = Arc::new(Mutex::new(contact));
        let (command_tx, command_rx) = mpsc::channel(Self::MAX_PACKETS);
        let (recv_tx, recv_rx) = mpsc::channel(Self::MAX_PACKETS);
        let recv_rx = TokioMutex::new(recv_rx);
        let main_task = tokio::spawn(Self::main_loop(
            contact_handle.clone(),
            contact.clone(),
            storage.clone(),
            command_rx,
            recv_tx,
            listener.clone(),
        ));
        Self {
            inner: Arc::new(ChatHandleInner {
                contact_handle,
                contact,
                storage,
                command_tx,
                recv_rx,
                main_task,
            }),
        }
    }

    pub fn address(&self) -> Address {
        let contact = self.inner.contact.lock().unwrap();
        contact.address
    }

    pub fn contact(&self) -> Contact {
        let contact = self.inner.contact.lock().unwrap();
        contact.clone()
    }

    pub fn contact_handle(&self) -> &ContactHandle {
        &self.inner.contact_handle
    }

    pub async fn send_message(&self, kind: MessageKind) -> Result<Message, anyhow::Error> {
        let contact_id = self.inner.contact.lock().unwrap().id;
        let message_id = Uuid::now_v7();
        let message = Message {
            id: 0,
            contact_id,
            message_id,
            log_id: None,
            incoming: false,
            kind,
            create_time: DateTime::now(),
            receive_time: None,
            read_time: None,
        };
        let message = Self::create_message(&self.inner.storage.as_ref(), message).await?;
        self.inner
            .command_tx
            .send(HandleCommand::SendMessage(message.clone()))
            .await
            .map_err(|_| anyhow::Error::msg("Handle is broken"))?;
        Ok(message)
    }

    pub async fn recv_message(&self) -> Result<Message, anyhow::Error> {
        Ok(self
            .inner
            .recv_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(anyhow!("Handle is broken"))?)
    }

    /// Load chat history for this contact.
    pub async fn load_history(&self, limit: usize) -> Result<Vec<Message>, anyhow::Error> {
        let contact_id = self.inner.contact.lock().unwrap().id;
        let columns = Message::columns();
        let query = format!(
            "SELECT {} FROM \"message\" \
             WHERE \"contact_id\" = ?1 \
             ORDER BY CASE WHEN \"log_id\" IS NULL THEN 0 ELSE 1 END, \"log_id\" DESC, \"id\" DESC \
             LIMIT ?2",
            Self::format_columns(columns)
        );
        let mut values = Vec::<tokio_sqlite::Value>::new();
        values.push(tokio_sqlite::Value::Integer(contact_id));
        values.push(tokio_sqlite::Value::Integer(limit as i64));
        let mut storage = self.inner.storage.lock().await;
        let conn = storage.connection().await;
        let mut rows = conn.query(query, values).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await {
            let row = row?;
            let values = row.into_values();
            let message = Message::from_values(values, columns)?;
            result.push(message);
        }
        result.reverse();
        Ok(result)
    }

    async fn main_loop(
        contact_handle: ContactHandle,
        contact: Arc<Mutex<Contact>>,
        storage: Arc<TokioMutex<Storage>>,
        mut command_rx: mpsc::Receiver<HandleCommand>,
        recv_tx: mpsc::Sender<Message>,
        listener: Arc<dyn ChatListener>,
    ) {
        let contact_id = contact.lock().unwrap().id;
        let contact_address = contact_handle.address();
        let mut pending_messages = VecDeque::<Uuid>::new();
        let mut pending_message_ack = None::<Uuid>;
        let mut head_log_id = Self::get_head_log_id(storage.as_ref(), contact_id)
            .await
            .unwrap();
        let mut next_tick = Self::next_tick();
        // Restore pending outgoing messages (incoming = 0, log_id IS NULL)
        match Self::get_pending_message_ids(storage.as_ref(), contact_id).await {
            Ok(ids) => {
                for id in ids {
                    pending_messages.push_back(id);
                }
            }
            Err(err) => {
                tracing::error!(?err, "Failed to restore pending messages");
            }
        }
        loop {
            tracing::trace!("Wait for chat update");
            tokio::select! {
                command = command_rx.recv() => {
                    let command = match command {
                        Some(v) => v,
                        None => return,
                    };
                    match command {
                        HandleCommand::SendMessage(message) => {
                            tracing::debug!("Registering new pending message");
                            if pending_message_ack.is_none() && pending_messages.is_empty() {
                                pending_message_ack = Some(message.message_id);
                                let log_id = head_log_id.unwrap_or(0) + 1;
                                let kind = match message.kind {
                                    MessageKind::Text(text) => ChatMessageKind::Text(text),
                                };
                                let packet = ChatMessagePacket {
                                    message_id: message.message_id,
                                    log_id,
                                    kind,
                                };
                                if let Err(err) = contact_handle.send_chat_packet(ChatPacket::Message(packet)).await {
                                    tracing::warn!(?err, "Failed to send chat packet");
                                }
                                continue;
                            }
                            pending_messages.push_back(message.message_id);
                        }
                    }
                }
                packet = contact_handle.recv_chat_packet() => {
                    let packet = match packet {
                        Ok(v) => v,
                        Err(err) => {
                            tracing::error!(?err, "Failed to receive chat packet");
                            continue;
                        }
                    };
                    match packet {
                        ChatPacket::Message(message_packet) => {
                            tracing::debug!("Received new message");
                            match Self::get_message(storage.as_ref(), message_packet.message_id).await {
                                Ok(Some(_)) => {
                                    tracing::debug!("Sending message ack");
                                    let packet = ChatMessageAckPacket {
                                        message_id: message_packet.message_id,
                                        log_id: message_packet.log_id,
                                    };
                                    if let Err(err) = contact_handle.send_chat_packet(ChatPacket::MessageAck(packet)).await {
                                        tracing::warn!(?err, "Failed to send chat packet");
                                    }
                                    continue;
                                },
                                Ok(None) => {}
                                Err(err) => {
                                    tracing::error!(?err, message_id = ?message_packet.message_id, "Failed to get message");
                                    continue;
                                }
                            };
                            tracing::trace!(message_id = ?message_packet.message_id, "Check message log_id");
                            if pending_message_ack.is_some() || !head_log_id.map(|v| v + 1 == message_packet.log_id).unwrap_or(true) {
                                tracing::debug!(log_id = message_packet.log_id, "Rejecting message with incorrect log_id");
                                let packet = ChatConflictPacket {
                                    message_id: message_packet.message_id,
                                };
                                if let Err(err) = contact_handle.send_chat_packet(ChatPacket::Conflict(packet)).await {
                                    tracing::warn!(?err, "Failed to send chat packet");
                                }
                                continue;
                            }
                            let kind = match message_packet.kind {
                                ChatMessageKind::Text(text) => MessageKind::Text(text),
                            };
                            let message = Message {
                                id: 0,
                                message_id: message_packet.message_id,
                                log_id: Some(message_packet.log_id),
                                contact_id: contact.lock().unwrap().id,
                                incoming: true,
                                kind,
                                create_time: DateTime::now(),
                                receive_time: Some(DateTime::now()),
                                read_time: None,
                            };
                            tracing::trace!(message_id = ?message.message_id, "Save message in storage");
                            let message = match Self::create_message(storage.as_ref(), message).await {
                                Ok(v) => v,
                                Err(err) => {
                                    tracing::error!(?err, "Failed to create message");
                                    continue;
                                }
                            };
                            tracing::trace!(log_id = ?message.log_id, "Update chat head");
                            head_log_id = message.log_id;
                            assert!(head_log_id.is_some());
                            listener.on_incoming_message(contact_address, message.clone()).await;
                            tracing::debug!("Sending message ack");
                            let packet = ChatMessageAckPacket {
                                message_id: message.message_id,
                                log_id: message_packet.log_id,
                            };
                            if let Err(err) = recv_tx.try_send(message.clone()) {
                                tracing::error!(?err, "Failed to notify recv message");
                            }
                            if let Err(err) = contact_handle.send_chat_packet(ChatPacket::MessageAck(packet)).await {
                                tracing::warn!(?err, "Failed to send chat packet");
                            }
                        }
                        ChatPacket::MessageAck(message_ack_packet) => {
                            tracing::debug!("Received message ack");
                            match pending_message_ack {
                                Some(message_id) => {
                                    tracing::trace!(?message_id, "Check message_id for ack");
                                    if message_id != message_ack_packet.message_id {
                                        tracing::debug!(message_id = ?message_ack_packet.message_id, "Rejected message ack");
                                        continue;
                                    }
                                    tracing::trace!(?message_id, "Fetch message content");
                                    let message = match Self::get_message(storage.as_ref(), message_ack_packet.message_id).await {
                                        Ok(Some(v)) => v,
                                        Ok(None) => {
                                            tracing::debug!(?message_id, "Message not found");
                                            pending_message_ack.take();
                                            continue;
                                        },
                                        Err(err) => {
                                            tracing::error!(?err, ?message_id, "Failed to get message");
                                            continue;
                                        }
                                    };
                                    tracing::trace!(?message_id, "Check message already confirmed");
                                    if message.log_id == Some(message_ack_packet.log_id) {
                                        tracing::debug!("Message already confirmed");
                                        continue;
                                    }
                                    tracing::trace!(?message_id, "Check message log_id");
                                    if !head_log_id.map(|v| v + 1 == message_ack_packet.log_id).unwrap_or(true) {
                                        tracing::warn!(
                                            log_id = message_ack_packet.log_id,
                                            "Rejecting message ack with incorrect log_id",
                                        );
                                        continue;
                                    }
                                    let mut new_message = message.clone();
                                    new_message.log_id = Some(message_ack_packet.log_id);
                                    new_message.receive_time = Some(DateTime::now());
                                    tracing::trace!(?message_id, "Update message status");
                                    let new_message = match Self::update_message(storage.as_ref(), new_message).await {
                                        Ok(v) => v,
                                        Err(err) => {
                                            tracing::error!(?err, "Failed to update message");
                                            continue;
                                        }
                                    };
                                    tracing::trace!(log_id = new_message.log_id, "Update chat head");
                                    pending_message_ack.take();
                                    head_log_id = new_message.log_id;
                                    assert!(head_log_id.is_some());
                                    listener.on_outgoing_message(contact_address, new_message).await;
                                }
                                None => {
                                    tracing::debug!(
                                        message_id = ?message_ack_packet.message_id,
                                        "Ignored message ack for non sent message",
                                    );
                                }
                            }
                        }
                        ChatPacket::Conflict(conflict_packet) => {
                            tracing::debug!("Received conflict");
                            match pending_message_ack {
                                Some(message_id) => {
                                    tracing::trace!(?message_id, "Check message_id for conflict");
                                    if message_id != conflict_packet.message_id {
                                        tracing::debug!(message_id = ?conflict_packet.message_id, "Rejected message conflict");
                                        continue;
                                    }
                                    tracing::debug!(?message_id, "Accepted message conflict");
                                    pending_messages.push_front(message_id);
                                    pending_message_ack.take();
                                }
                                None => {
                                    tracing::debug!(
                                        message_id = ?conflict_packet.message_id,
                                        "Ignored message conflict for non sent message",
                                    );
                                }
                            }
                        }
                    }
                }
                _ = sleep_until(next_tick) => {
                    next_tick = Self::next_tick();
                    let message_id = if let Some(v) = pending_message_ack {
                        v
                    } else if let Some(v) = pending_messages.pop_front() {
                        pending_message_ack = Some(v);
                        v
                    } else {
                        continue;
                    };
                    let message = match Self::get_message(storage.as_ref(), message_id).await {
                        Ok(Some(v)) => v,
                        Ok(None) => {
                            tracing::debug!(?message_id, "Message not found");
                            // Ignore message.
                            pending_message_ack.take();
                            continue;
                        }
                        Err(err) => {
                            tracing::error!(?err, ?message_id, "Failed to get message");
                            continue;
                        }
                    };
                    let log_id = head_log_id.unwrap_or(0) + 1;
                    let kind = match message.kind {
                        MessageKind::Text(text) => ChatMessageKind::Text(text),
                    };
                    let packet = ChatMessagePacket {
                        message_id,
                        log_id,
                        kind,
                    };
                    if let Err(err) = contact_handle.send_chat_packet(ChatPacket::Message(packet)).await {
                        tracing::warn!(?err, "Failed to send chat packet");
                    }
                }
            }
        }
    }

    async fn get_head_log_id(
        storage: &TokioMutex<Storage>,
        contact_id: i64,
    ) -> Result<Option<u64>, anyhow::Error> {
        let query = "SELECT MAX(\"log_id\") FROM \"message\" WHERE \"log_id\" IS NOT NULL AND \"contact_id\" = ?1";
        let mut values = Vec::<Value>::new();
        values.push(contact_id.into());
        let mut storage = storage.lock().await;
        let connection = storage.connection().await;
        match connection.query_row(query, values).await? {
            Some(row) => {
                let values = row.into_values();
                assert_eq!(values.len(), 1);
                match values.first().unwrap() {
                    Value::Integer(i) if *i >= 0 => return Ok(Some(*i as u64)),
                    Value::Null => return Ok(None),
                    v => return Err(anyhow!("Failed to parse log_id from value: {v:?}")),
                }
            }
            None => Ok(None),
        }
    }

    async fn get_pending_message_ids(
        storage: &TokioMutex<Storage>,
        contact_id: i64,
    ) -> Result<Vec<Uuid>, anyhow::Error> {
        let query = "SELECT \"message_id\" FROM \"message\" \
                     WHERE \"contact_id\" = ?1 AND \"incoming\" = 0 AND \"log_id\" IS NULL \
                     ORDER BY \"id\" ASC";
        let mut storage = storage.lock().await;
        let conn = storage.connection().await;
        let mut rows = conn.query(query, vec![Value::Integer(contact_id)]).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await {
            let row = row?;
            if let Some(Value::Text(s)) = row.into_values().into_iter().next() {
                if let Ok(uuid) = Uuid::parse_str(&s) {
                    result.push(uuid);
                }
            }
        }
        Ok(result)
    }

    async fn get_message(
        storage: &TokioMutex<Storage>,
        message_id: Uuid,
    ) -> Result<Option<Message>, anyhow::Error> {
        let columns = Message::columns();
        let query = format!(
            "SELECT {} FROM \"message\" WHERE \"message_id\" = ?1 LIMIT 1",
            Self::format_columns(columns)
        );
        let mut values = Vec::<Value>::new();
        values.push(message_id.to_string().into());
        let mut storage = storage.lock().await;
        let connection = storage.connection().await;
        match connection.query_row(query, values).await? {
            Some(row) => {
                let values = row.into_values();
                let message = Message::from_values(values, columns)?;
                Ok(Some(message))
            }
            None => Ok(None),
        }
    }

    async fn create_message(
        storage: &TokioMutex<Storage>,
        mut message: Message,
    ) -> Result<Message, anyhow::Error> {
        let columns = Self::columns_without_id(Message::columns(), "id");
        let values = message.values(&columns);
        let query = format!(
            "INSERT INTO \"message\" ({}) VALUES ({})",
            Self::format_columns(&columns),
            Self::format_values(&values),
        );
        let mut storage = storage.lock().await;
        let connection = storage.connection().await;
        let status = connection.execute(query, values).await?;
        message.id = status
            .last_insert_id()
            .ok_or(anyhow!("Cannot retrieve contact id"))?;
        Ok(message)
    }

    async fn update_message(
        storage: &TokioMutex<Storage>,
        message: Message,
    ) -> Result<Message, anyhow::Error> {
        let query =
            "UPDATE \"message\" SET \"log_id\" = ?1, \"receive_time\" = ?2 WHERE \"id\" = ?3";
        let mut storage = storage.lock().await;
        let connection = storage.connection().await;
        let mut values = Vec::<Value>::new();
        values.push(message.log_id.map(|v| v as i64).into());
        values.push(message.receive_time.map(|v| v.0.timestamp_micros()).into());
        values.push(message.id.into());
        let status = connection.execute(query, values).await?;
        if status.rows_affected() != 1 {
            return Err(anyhow!("Cannot delete contact"));
        }
        Ok(message)
    }

    fn columns_without_id(columns: &ColumnIndex, id_name: &str) -> ColumnIndex {
        let mut result = ColumnIndex::builder();
        for name in columns.columns() {
            if name != id_name {
                result.add(name);
            }
        }
        result.build()
    }

    fn format_columns(columns: &ColumnIndex) -> String {
        let mut result = String::new();
        for column in columns.columns() {
            if !result.is_empty() {
                result.push_str(", ");
            }
            result.push('"');
            result.push_str(&column);
            result.push('"');
        }
        result
    }

    fn format_values(values: &[Value]) -> String {
        let mut result = String::new();
        for i in 0..values.len() {
            if !result.is_empty() {
                result.push_str(", ");
            }
            result.push('?');
            result.push_str(&(i + 1).to_string());
        }
        result
    }

    fn next_tick() -> Instant {
        Instant::now() + Duration::from_millis(rand::thread_rng().gen_range(1000..5000))
    }
}

struct ChatHandleInner {
    contact_handle: ContactHandle,
    contact: Arc<Mutex<Contact>>,
    storage: Arc<TokioMutex<Storage>>,
    command_tx: mpsc::Sender<HandleCommand>,
    recv_rx: TokioMutex<mpsc::Receiver<Message>>,
    main_task: JoinHandle<()>,
}

impl Drop for ChatHandleInner {
    fn drop(&mut self) {
        self.main_task.abort();
    }
}

enum HandleCommand {
    SendMessage(Message),
}
