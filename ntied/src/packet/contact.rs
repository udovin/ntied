use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ContactPacket {
    Request(ContactRequestPacket),
    Accept(ContactAcceptPacket),
    Reject(ContactRejectPacket),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactRequestPacket {
    pub profile: ContactProfile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactAcceptPacket {
    pub profile: ContactProfile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactRejectPacket {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactProfile {
    pub name: String,
}
