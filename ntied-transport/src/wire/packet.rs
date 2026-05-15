use crate::crypto::{KemCiphertext, KemPublicKey, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE};

/// Packet header for routing without full decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketHeader {
    Init {
        initiator_connection_id: u64,
    },
    InitAck {
        responder_connection_id: u64,
        initiator_connection_id: u64,
    },
    Data {
        epoch: u8,
        receiver_connection_id: u64,
    },
}

pub const INIT_TYPE: u8 = 0x01;
pub const INIT_ACK_TYPE: u8 = 0x02;
pub const DATA_TYPE_BASE: u8 = 0x10;
/// 2-bit epoch: values 0..3, packet types 0x10..0x13.
pub const EPOCH_MASK: u8 = 0x03;
pub const DATA_HEADER_SIZE: usize = 1 + 8 + 8; // type + connection_id + counter

/// Init: [type:1] [initiator_id:8] [kem_pk:1216]
pub const INIT_SIZE: usize = 1 + 8 + KEM_PUBLIC_KEY_SIZE;

/// InitAck: [type:1] [responder_id:8] [initiator_id:8] [kem_ct:1120]
pub const INIT_ACK_SIZE: usize = 1 + 8 + 8 + KEM_CIPHERTEXT_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    UnexpectedEnd,
    InvalidType(u8),
}

/// Peek the packet header for routing.  Does not consume the buffer.
pub fn peek_header(buf: &[u8]) -> Result<PacketHeader, PacketError> {
    if buf.is_empty() {
        return Err(PacketError::UnexpectedEnd);
    }

    let packet_type = buf[0];
    match packet_type {
        INIT_TYPE => {
            if buf.len() < 9 {
                return Err(PacketError::UnexpectedEnd);
            }
            let id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
            Ok(PacketHeader::Init {
                initiator_connection_id: id,
            })
        }
        INIT_ACK_TYPE => {
            if buf.len() < 17 {
                return Err(PacketError::UnexpectedEnd);
            }
            let resp_id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
            let init_id = u64::from_be_bytes(buf[9..17].try_into().unwrap());
            Ok(PacketHeader::InitAck {
                responder_connection_id: resp_id,
                initiator_connection_id: init_id,
            })
        }
        t if t >= DATA_TYPE_BASE && t <= DATA_TYPE_BASE + EPOCH_MASK => {
            if buf.len() < 9 {
                return Err(PacketError::UnexpectedEnd);
            }
            let epoch = t - DATA_TYPE_BASE;
            let id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
            Ok(PacketHeader::Data {
                epoch,
                receiver_connection_id: id,
            })
        }
        t => Err(PacketError::InvalidType(t)),
    }
}

/// Parsed Init packet.
pub struct InitPacket {
    pub initiator_connection_id: u64,
    pub kem_public_key: KemPublicKey,
}

/// Parse a full Init packet.
pub fn parse_init(buf: &[u8]) -> Result<InitPacket, PacketError> {
    if buf.len() < INIT_SIZE {
        return Err(PacketError::UnexpectedEnd);
    }
    if buf[0] != INIT_TYPE {
        return Err(PacketError::InvalidType(buf[0]));
    }
    let initiator_connection_id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
    let kem_public_key =
        KemPublicKey::from_bytes(buf[9..9 + KEM_PUBLIC_KEY_SIZE].try_into().unwrap());
    Ok(InitPacket {
        initiator_connection_id,
        kem_public_key,
    })
}

/// Encode an Init packet. Returns bytes written (INIT_SIZE).
pub fn encode_init(out: &mut [u8], initiator_connection_id: u64, kem_pk: &KemPublicKey) -> usize {
    out[0] = INIT_TYPE;
    out[1..9].copy_from_slice(&initiator_connection_id.to_be_bytes());
    out[9..9 + KEM_PUBLIC_KEY_SIZE].copy_from_slice(&kem_pk.to_bytes());
    INIT_SIZE
}

/// Parsed InitAck packet.
pub struct InitAckPacket {
    pub responder_connection_id: u64,
    pub initiator_connection_id: u64,
    pub kem_ciphertext: KemCiphertext,
}

/// Parse a full InitAck packet.
pub fn parse_init_ack(buf: &[u8]) -> Result<InitAckPacket, PacketError> {
    if buf.len() < INIT_ACK_SIZE {
        return Err(PacketError::UnexpectedEnd);
    }
    if buf[0] != INIT_ACK_TYPE {
        return Err(PacketError::InvalidType(buf[0]));
    }
    let responder_connection_id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
    let initiator_connection_id = u64::from_be_bytes(buf[9..17].try_into().unwrap());
    let kem_ciphertext =
        KemCiphertext::from_bytes(buf[17..17 + KEM_CIPHERTEXT_SIZE].try_into().unwrap());
    Ok(InitAckPacket {
        responder_connection_id,
        initiator_connection_id,
        kem_ciphertext,
    })
}

/// Encode an InitAck packet. Returns bytes written (INIT_ACK_SIZE).
pub fn encode_init_ack(
    out: &mut [u8],
    responder_connection_id: u64,
    initiator_connection_id: u64,
    kem_ct: &KemCiphertext,
) -> usize {
    out[0] = INIT_ACK_TYPE;
    out[1..9].copy_from_slice(&responder_connection_id.to_be_bytes());
    out[9..17].copy_from_slice(&initiator_connection_id.to_be_bytes());
    out[17..17 + KEM_CIPHERTEXT_SIZE].copy_from_slice(&kem_ct.to_bytes());
    INIT_ACK_SIZE
}

/// Zero-copy Data packet view.
#[derive(Debug)]
pub struct DataPacket<'a> {
    pub epoch: u8,
    pub receiver_connection_id: u64,
    pub counter: u64,
    pub payload: &'a [u8],
}

/// Parse a Data packet from raw bytes.  Payload is borrowed.
pub fn parse_data_packet(buf: &[u8]) -> Result<DataPacket<'_>, PacketError> {
    if buf.len() < DATA_HEADER_SIZE {
        return Err(PacketError::UnexpectedEnd);
    }
    let packet_type = buf[0];
    if packet_type < DATA_TYPE_BASE || packet_type > DATA_TYPE_BASE + EPOCH_MASK {
        return Err(PacketError::InvalidType(packet_type));
    }

    let epoch = packet_type - DATA_TYPE_BASE;
    let receiver_connection_id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
    let counter = u64::from_be_bytes(buf[9..17].try_into().unwrap());
    let payload = &buf[DATA_HEADER_SIZE..];

    Ok(DataPacket {
        epoch,
        receiver_connection_id,
        counter,
        payload,
    })
}

/// Write a Data packet header.  Returns header length (always DATA_HEADER_SIZE).
/// `epoch` must be 0..3 (2 bits).
pub fn encode_data_header(out: &mut [u8], epoch: u8, receiver_connection_id: u64, counter: u64) -> usize {
    debug_assert!(epoch <= EPOCH_MASK, "epoch {epoch} exceeds 2 bits");
    out[0] = DATA_TYPE_BASE + (epoch & EPOCH_MASK);
    out[1..9].copy_from_slice(&receiver_connection_id.to_be_bytes());
    out[9..17].copy_from_slice(&counter.to_be_bytes());
    DATA_HEADER_SIZE
}
