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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peek_init() {
        let mut buf = vec![INIT_TYPE];
        buf.extend_from_slice(&42u64.to_be_bytes());
        buf.extend_from_slice(&[0u8; 100]);

        match peek_header(&buf).unwrap() {
            PacketHeader::Init { initiator_connection_id } => {
                assert_eq!(initiator_connection_id, 42);
            }
            _ => panic!("expected Init"),
        }
    }

    #[test]
    fn peek_init_ack() {
        let mut buf = vec![INIT_ACK_TYPE];
        buf.extend_from_slice(&1u64.to_be_bytes());
        buf.extend_from_slice(&2u64.to_be_bytes());

        match peek_header(&buf).unwrap() {
            PacketHeader::InitAck { responder_connection_id, initiator_connection_id } => {
                assert_eq!(responder_connection_id, 1);
                assert_eq!(initiator_connection_id, 2);
            }
            _ => panic!("expected InitAck"),
        }
    }

    #[test]
    fn peek_data_all_epochs() {
        for epoch in 0..=3u8 {
            let mut buf = vec![DATA_TYPE_BASE + epoch];
            buf.extend_from_slice(&99u64.to_be_bytes());

            match peek_header(&buf).unwrap() {
                PacketHeader::Data { epoch: e, receiver_connection_id } => {
                    assert_eq!(e, epoch);
                    assert_eq!(receiver_connection_id, 99);
                }
                _ => panic!("expected Data"),
            }
        }
    }

    #[test]
    fn peek_too_short() {
        assert_eq!(peek_header(&[]), Err(PacketError::UnexpectedEnd));
        assert_eq!(peek_header(&[INIT_TYPE, 0, 0]), Err(PacketError::UnexpectedEnd));
    }

    #[test]
    fn peek_invalid_type() {
        assert_eq!(peek_header(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0]), Err(PacketError::InvalidType(0x03)));
    }

    #[test]
    fn peek_invalid_type_above_data() {
        assert_eq!(peek_header(&[0x14, 0, 0, 0, 0, 0, 0, 0, 0]), Err(PacketError::InvalidType(0x14)));
    }

    #[test]
    fn peek_data_too_short() {
        assert_eq!(
            peek_header(&[DATA_TYPE_BASE, 0, 0]),
            Err(PacketError::UnexpectedEnd)
        );
    }

    #[test]
    fn peek_init_ack_too_short() {
        let mut buf = vec![INIT_ACK_TYPE];
        buf.extend_from_slice(&1u64.to_be_bytes());
        assert_eq!(peek_header(&buf), Err(PacketError::UnexpectedEnd));
    }

    #[test]
    fn parse_data() {
        let mut buf = vec![DATA_TYPE_BASE + 1];
        buf.extend_from_slice(&10u64.to_be_bytes());
        buf.extend_from_slice(&77u64.to_be_bytes());
        buf.extend_from_slice(b"encrypted_payload");

        let pkt = parse_data_packet(&buf).unwrap();
        assert_eq!(pkt.epoch, 1);
        assert_eq!(pkt.receiver_connection_id, 10);
        assert_eq!(pkt.counter, 77);
        assert_eq!(pkt.payload, b"encrypted_payload");
    }

    #[test]
    fn parse_data_too_short() {
        let buf = [DATA_TYPE_BASE, 0, 0, 0];
        assert!(matches!(parse_data_packet(&buf), Err(PacketError::UnexpectedEnd)));
    }

    #[test]
    fn parse_data_wrong_type_below() {
        let buf = [INIT_TYPE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(parse_data_packet(&buf), Err(PacketError::InvalidType(INIT_TYPE))));
    }

    #[test]
    fn parse_data_wrong_type_above() {
        let buf = [0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(parse_data_packet(&buf), Err(PacketError::InvalidType(0x14))));
    }

    #[test]
    fn encode_decode_data_header() {
        for epoch in 0..=3u8 {
            let mut buf = [0u8; 17];
            let n = encode_data_header(&mut buf, epoch, 123, 456);
            assert_eq!(n, 17);

            let pkt = parse_data_packet(&buf).unwrap();
            assert_eq!(pkt.epoch, epoch);
            assert_eq!(pkt.receiver_connection_id, 123);
            assert_eq!(pkt.counter, 456);
        }
    }

    #[test]
    fn init_roundtrip() {
        use crate::crypto::KemPrivateKey;
        let eph = KemPrivateKey::generate();
        let pk = eph.public_key();

        let mut buf = [0u8; INIT_SIZE];
        let n = encode_init(&mut buf, 42, &pk);
        assert_eq!(n, INIT_SIZE);

        let pkt = parse_init(&buf).unwrap();
        assert_eq!(pkt.initiator_connection_id, 42);
        assert_eq!(pkt.kem_public_key.to_bytes(), pk.to_bytes());
    }

    #[test]
    fn init_ack_roundtrip() {
        use crate::crypto::KemPrivateKey;
        let init_eph = KemPrivateKey::generate();
        let init_pk = init_eph.public_key();
        let resp_eph = KemPrivateKey::generate();
        let (ct, _) = resp_eph.encapsulate(&init_pk).unwrap();

        let mut buf = [0u8; INIT_ACK_SIZE];
        let n = encode_init_ack(&mut buf, 99, 42, &ct);
        assert_eq!(n, INIT_ACK_SIZE);

        let pkt = parse_init_ack(&buf).unwrap();
        assert_eq!(pkt.responder_connection_id, 99);
        assert_eq!(pkt.initiator_connection_id, 42);
        assert_eq!(pkt.kem_ciphertext.to_bytes(), ct.to_bytes());
    }

    #[test]
    fn init_too_short() {
        let buf = [INIT_TYPE; 10];
        assert!(matches!(parse_init(&buf), Err(PacketError::UnexpectedEnd)));
    }

    #[test]
    fn init_ack_too_short() {
        let buf = [INIT_ACK_TYPE; 20];
        assert!(matches!(parse_init_ack(&buf), Err(PacketError::UnexpectedEnd)));
    }
}
