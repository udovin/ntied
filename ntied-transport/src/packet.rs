use ntied_crypto::{Error, SharedSecret};

use crate::Address;
use crate::byteio::{Reader, Writer};

pub enum Packet {
    Handshake(HandshakePacket),
    HandshakeAck(HandshakeAckPacket),
    Encrypted(EncryptedPacket),
}

impl Packet {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        match self {
            Self::Handshake(packet) => {
                writer.write_u8(1);
                packet.serialize_to(&mut writer);
            }
            Self::HandshakeAck(packet) => {
                writer.write_u8(2);
                packet.serialize_to(&mut writer);
            }
            Self::Encrypted(v) => {
                writer.write_u8(v.epoch.as_u8() + EncryptionEpoch::RESERVED);
                writer.write_u32(v.target_id);
                writer.write_bytes(&v.payload);
                writer.write_array(&v.nonce);
            }
        }
        bytes
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(bytes);
        let packet_type = reader.read_u8()?;
        match packet_type {
            1 => {
                let packet = HandshakePacket::deserialize_from(&mut reader)?;
                Ok(Self::Handshake(packet))
            }
            2 => {
                let packet = HandshakeAckPacket::deserialize_from(&mut reader)?;
                Ok(Self::HandshakeAck(packet))
            }
            _ => {
                if packet_type < EncryptionEpoch::RESERVED {
                    return Err("Incorrect packet type".into());
                }
                let epoch = EncryptionEpoch::from_u8(packet_type - EncryptionEpoch::RESERVED)?;
                let target_id = reader.read_u32()?;
                let payload = reader.read_bytes()?;
                let nonce = reader.read_array()?;
                Ok(Packet::Encrypted(EncryptedPacket {
                    target_id,
                    epoch,
                    payload,
                    nonce,
                }))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EncryptionEpoch(u8);

impl EncryptionEpoch {
    const RESERVED: u8 = 128;

    pub fn new(epoch: u8) -> Self {
        assert!(
            epoch <= u8::MAX - Self::RESERVED,
            "Epoch value exceeds maximum allowed"
        );
        Self(epoch)
    }

    pub fn from_u8(epoch: u8) -> Result<Self, Error> {
        if epoch > u8::MAX - Self::RESERVED {
            Err("Epoch value exceeds maximum allowed".into())
        } else {
            Ok(Self(epoch))
        }
    }

    pub fn as_u8(&self) -> u8 {
        self.0
    }

    pub fn next(&self) -> Self {
        let epoch = self.0 + 1;
        if epoch > u8::MAX - Self::RESERVED {
            Self(1)
        } else {
            Self(epoch)
        }
    }
}

impl Default for EncryptionEpoch {
    fn default() -> Self {
        Self::new(0)
    }
}

pub struct EncryptedPacket {
    pub target_id: u32,
    pub epoch: EncryptionEpoch,
    pub payload: Vec<u8>,
    pub nonce: [u8; 12],
}

impl EncryptedPacket {
    pub fn encrypt(
        target_id: u32,
        message: DecryptedPacket,
        epoch: EncryptionEpoch,
        shared_secret: &SharedSecret,
        nonce: [u8; 12],
    ) -> Result<Self, Error> {
        let decrypted_payload = message.serialize();
        let payload = shared_secret.encrypt_nonce(&nonce, &decrypted_payload)?;
        Ok(Self {
            target_id,
            epoch,
            payload,
            nonce,
        })
    }

    pub fn decrypt(&self, shared_secret: &SharedSecret) -> Result<DecryptedPacket, Error> {
        let decrypted_payload = shared_secret.decrypt_nonce(&self.nonce, &self.payload)?;
        let message = DecryptedPacket::deserialize(&decrypted_payload)?;
        Ok(message)
    }
}

pub enum DecryptedPacket {
    Heartbeat(HeartbeatPacket),
    HeartbeatAck(HeartbeatPacket),
    Data(DataPacket),
    Rotate(RotatePacket),
    RotateAck(RotatePacket),
}

impl DecryptedPacket {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes);
        match self {
            Self::Heartbeat(_) => {
                writer.write_u8(1);
            }
            Self::HeartbeatAck(_) => {
                writer.write_u8(2);
            }
            Self::Data(packet) => {
                writer.write_u8(3);
                packet.serialize_to(&mut writer);
            }
            Self::Rotate(packet) => {
                writer.write_u8(4);
                packet.serialize_to(&mut writer);
            }
            Self::RotateAck(packet) => {
                writer.write_u8(5);
                packet.serialize_to(&mut writer);
            }
        }
        bytes
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::new(bytes);
        match reader.read_u8()? {
            1 => Ok(Self::Heartbeat(HeartbeatPacket {})),
            2 => Ok(Self::HeartbeatAck(HeartbeatPacket {})),
            3 => {
                let packet = DataPacket::deserialize_from(&mut reader)?;
                Ok(Self::Data(packet))
            }
            4 => {
                let packet = RotatePacket::deserialize_from(&mut reader)?;
                Ok(Self::Rotate(packet))
            }
            5 => {
                let packet = RotatePacket::deserialize_from(&mut reader)?;
                Ok(Self::RotateAck(packet))
            }
            _ => Err("Unknown message type".into()),
        }
    }
}

pub struct HandshakePacket {
    pub source_id: u32,
    pub peer_address: Address,
    pub address: Address,
    pub public_key: Vec<u8>,
    pub ephemeral_public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl HandshakePacket {
    pub fn serialize_to(&self, writer: &mut Writer<'_>) {
        writer.write_u32(self.source_id);
        writer.write_array(self.peer_address.as_bytes());
        writer.write_array(self.address.as_bytes());
        writer.write_bytes(&self.public_key);
        writer.write_bytes(&self.ephemeral_public_key);
        writer.write_bytes(&self.signature);
    }

    pub fn deserialize_from(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let source_id = reader.read_u32()?;
        let peer_address = Address::from_bytes(reader.read_array()?);
        let address = Address::from_bytes(reader.read_array()?);
        let public_key = reader.read_bytes()?;
        let ephemeral_public_key = reader.read_bytes()?;
        let signature = reader.read_bytes()?;
        Ok(Self {
            source_id,
            public_key,
            address,
            peer_address,
            ephemeral_public_key,
            signature,
        })
    }
}

pub struct HandshakeAckPacket {
    pub target_id: u32,
    pub source_id: u32,
    pub peer_address: Address,
    pub address: Address,
    pub public_key: Vec<u8>,
    pub ephemeral_public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl HandshakeAckPacket {
    pub fn serialize_to(&self, writer: &mut Writer<'_>) {
        writer.write_u32(self.target_id);
        writer.write_u32(self.source_id);
        writer.write_array(self.peer_address.as_bytes());
        writer.write_array(self.address.as_bytes());
        writer.write_bytes(&self.public_key);
        writer.write_bytes(&self.ephemeral_public_key);
        writer.write_bytes(&self.signature);
    }

    pub fn deserialize_from(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let target_id = reader.read_u32()?;
        let source_id = reader.read_u32()?;
        let peer_address = Address::from_bytes(reader.read_array()?);
        let address = Address::from_bytes(reader.read_array()?);
        let public_key = reader.read_bytes()?;
        let ephemeral_public_key = reader.read_bytes()?;
        let signature = reader.read_bytes()?;
        Ok(Self {
            target_id,
            source_id,
            public_key,
            address,
            peer_address,
            ephemeral_public_key,
            signature,
        })
    }
}

pub struct RotatePacket {
    pub ephemeral_public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl RotatePacket {
    pub fn serialize_to(&self, writer: &mut Writer<'_>) {
        writer.write_bytes(&self.ephemeral_public_key);
        writer.write_bytes(&self.signature);
    }

    pub fn deserialize_from(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let ephemeral_public_key = reader.read_bytes()?;
        let signature = reader.read_bytes()?;
        Ok(Self {
            ephemeral_public_key,
            signature,
        })
    }
}

pub struct HeartbeatPacket {}

pub struct DataPacket {
    pub data: Vec<u8>,
}

impl DataPacket {
    pub fn serialize_to(&self, writer: &mut Writer<'_>) {
        writer.write_bytes(&self.data);
    }

    pub fn deserialize_from(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let data = reader.read_bytes()?;
        Ok(Self { data })
    }
}
