use serde::{Deserialize, Serialize};

use super::{CallPacket, ChatPacket, ContactPacket};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Packet {
    Contact(ContactPacket),
    Chat(ChatPacket),
    Call(CallPacket),
}
