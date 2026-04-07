use super::codec::{CodecError, Reader, Writer};
use crate::crypto::{KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE, KemCiphertext, KemPublicKey};

pub const INITIAL_MTU: usize = 1350;
pub const DATA_HEADER_SIZE: usize = 17;
pub const AEAD_TAG_SIZE: usize = 16;
pub const PACKET_OVERHEAD: usize = DATA_HEADER_SIZE + AEAD_TAG_SIZE;
pub const MAX_PACKET_PAYLOAD: usize = INITIAL_MTU - PACKET_OVERHEAD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    UnexpectedEnd,
    InvalidPacketType(u8),
}

impl From<CodecError> for PacketError {
    fn from(err: CodecError) -> Self {
        match err {
            CodecError::UnexpectedEnd => Self::UnexpectedEnd,
        }
    }
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of packet"),
            Self::InvalidPacketType(t) => write!(f, "invalid packet type: 0x{t:02X}"),
        }
    }
}

impl std::error::Error for PacketError {}

pub struct Handshake {
    pub initiator_connection_id: u64,
    pub kem_public_key: KemPublicKey,
}

pub struct HandshakeAck {
    pub responder_connection_id: u64,
    pub initiator_connection_id: u64,
    pub kem_ciphertext: KemCiphertext,
}

#[derive(Clone)]
pub struct Data {
    pub epoch: u8,
    pub receiver_connection_id: u64,
    pub counter: u64,
    pub encrypted_payload: Vec<u8>,
}

pub enum Packet {
    Handshake(Handshake),
    HandshakeAck(HandshakeAck),
    Data(Data),
}

impl Packet {
    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        let mut reader = Reader::new(buf);
        let packet_type = reader.read_u8()?;
        match packet_type {
            Handshake::TYPE => Ok(Self::Handshake(Handshake::decode(&mut reader)?)),
            HandshakeAck::TYPE => Ok(Self::HandshakeAck(HandshakeAck::decode(&mut reader)?)),
            t if t >= Data::TYPE_START => {
                let epoch = t - Data::TYPE_START;
                Ok(Self::Data(Data::decode_with_epoch(epoch, &mut reader)?))
            }
            t => Err(PacketError::InvalidPacketType(t)),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Handshake(p) => p.encode(),
            Self::HandshakeAck(p) => p.encode(),
            Self::Data(p) => p.encode(),
        }
    }
}

impl Handshake {
    pub const TYPE: u8 = 0x01;
    pub const PACKET_SIZE: usize = 1 + 8 + KEM_PUBLIC_KEY_SIZE;

    pub fn decode(reader: &mut Reader) -> Result<Self, PacketError> {
        let initiator_connection_id = reader.read_u64()?;
        let kem_public_key = KemPublicKey::from_bytes(&reader.read_array()?);
        Ok(Self {
            initiator_connection_id,
            kem_public_key,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(Self::PACKET_SIZE);
        w.write_u8(Self::TYPE);
        w.write_u64(self.initiator_connection_id);
        w.write_bytes(&self.kem_public_key.to_bytes());
        w.into_vec()
    }
}

impl HandshakeAck {
    pub const TYPE: u8 = 0x02;
    pub const PACKET_SIZE: usize = 1 + 8 + 8 + KEM_CIPHERTEXT_SIZE;

    pub fn decode(reader: &mut Reader) -> Result<Self, PacketError> {
        let responder_connection_id = reader.read_u64()?;
        let initiator_connection_id = reader.read_u64()?;
        let kem_ciphertext = KemCiphertext::from_bytes(&reader.read_array()?);
        Ok(Self {
            responder_connection_id,
            initiator_connection_id,
            kem_ciphertext,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(Self::PACKET_SIZE);
        w.write_u8(Self::TYPE);
        w.write_u64(self.responder_connection_id);
        w.write_u64(self.initiator_connection_id);
        w.write_bytes(&self.kem_ciphertext.to_bytes());
        w.into_vec()
    }
}

impl Data {
    const TYPE_START: u8 = 0x10;

    pub fn decode_with_epoch(epoch: u8, reader: &mut Reader) -> Result<Self, PacketError> {
        let receiver_connection_id = reader.read_u64()?;
        let counter = reader.read_u64()?;
        let encrypted_payload = reader.remaining().to_vec();
        Ok(Self {
            epoch,
            receiver_connection_id,
            counter,
            encrypted_payload,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(DATA_HEADER_SIZE + self.encrypted_payload.len());
        w.write_u8(self.epoch + Self::TYPE_START);
        w.write_u64(self.receiver_connection_id);
        w.write_u64(self.counter);
        w.write_bytes(&self.encrypted_payload);
        w.into_vec()
    }

    pub fn aad(&self) -> [u8; DATA_HEADER_SIZE] {
        let mut buf = [0u8; DATA_HEADER_SIZE];
        buf[0] = self.epoch + Self::TYPE_START;
        buf[1..9].copy_from_slice(&self.receiver_connection_id.to_be_bytes());
        buf[9..17].copy_from_slice(&self.counter.to_be_bytes());
        buf
    }
}
