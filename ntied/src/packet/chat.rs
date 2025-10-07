use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ChatPacket {
    Message(ChatMessagePacket),
    MessageAck(ChatMessageAckPacket),
    Conflict(ChatConflictPacket),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessagePacket {
    pub message_id: Uuid,
    pub log_id: u64,
    pub kind: ChatMessageKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ChatMessageKind {
    Text(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessageAckPacket {
    pub message_id: Uuid,
    pub log_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatConflictPacket {
    pub message_id: Uuid,
}
