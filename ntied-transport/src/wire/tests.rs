use super::frame::*;
use super::packet::*;
use crate::crypto::KemPrivateKey;

// -- Frame --

fn collect_frames(buf: &[u8]) -> Vec<Frame<'_>> {
    decode_frames(buf).collect::<Result<Vec<_>, _>>().unwrap()
}

fn expect_err(buf: &[u8], expected: FrameError) {
    let err = decode_frames(buf)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_err();
    assert_eq!(err, expected);
}

#[test]
fn stream_roundtrip() {
    let mut buf = [0u8; 128];
    let h = encode_stream_header(&mut buf, 42, 100, 5, true);
    buf[h..h + 5].copy_from_slice(b"hello");
    let frames = collect_frames(&buf[..h + 5]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], Frame::Stream { stream_id: 42, offset: 100, fin: true, data: b"hello" });
}

#[test]
fn stream_no_fin() {
    let mut buf = [0u8; 128];
    let h = encode_stream_header(&mut buf, 1, 0, 3, false);
    buf[h..h + 3].copy_from_slice(b"abc");
    let frames = collect_frames(&buf[..h + 3]);
    match &frames[0] {
        Frame::Stream { fin, .. } => assert!(!fin),
        _ => panic!("expected Stream"),
    }
}

#[test]
fn channel_roundtrip() {
    let mut buf = [0u8; 128];
    let h = encode_channel_header(&mut buf, 10, 20, 500, 4, false);
    buf[h..h + 4].copy_from_slice(b"data");
    let frames = collect_frames(&buf[..h + 4]);
    assert_eq!(frames[0], Frame::Channel { channel_id: 10, message_id: 20, offset: 500, fin: false, data: b"data" });
}

#[test]
fn auth_roundtrip() {
    let mut buf = [0u8; 128];
    let h = encode_auth_header(&mut buf, 0, 6, true);
    buf[h..h + 6].copy_from_slice(b"secret");
    let frames = collect_frames(&buf[..h + 6]);
    assert_eq!(frames[0], Frame::Auth { offset: 0, fin: true, data: b"secret" });
}

#[test]
fn ack_roundtrip() {
    let mut buf = [0u8; 128];
    let ranges = vec![(0u64, 5u64), (3, 10)];
    let n = encode_ack(&mut buf, 99, 1234, &ranges);
    let frames = collect_frames(&buf[..n]);
    match &frames[0] {
        Frame::Ack { largest, delay, ranges } => {
            assert_eq!(*largest, 99);
            assert_eq!(*delay, 1234);
            assert_eq!(ranges.len(), 32);
        }
        _ => panic!("expected Ack"),
    }
}

#[test]
fn ping_pong_roundtrip() {
    let mut buf = [0u8; 10];
    let n = encode_ping(&mut buf, 7);
    assert_eq!(collect_frames(&buf[..n])[0], Frame::Ping { id: 7 });

    let n = encode_pong(&mut buf, 7);
    assert_eq!(collect_frames(&buf[..n])[0], Frame::Pong { id: 7 });
}

#[test]
fn auth_complete_roundtrip() {
    let mut buf = [0u8; 1];
    let n = encode_auth_complete(&mut buf);
    assert_eq!(collect_frames(&buf[..n])[0], Frame::AuthComplete);
}

#[test]
fn connection_close_roundtrip() {
    let mut buf = [0u8; 128];
    let n = encode_connection_close(&mut buf, 42, b"bye");
    assert_eq!(collect_frames(&buf[..n])[0], Frame::ConnectionClose { error_code: 42, reason: b"bye" });
}

#[test]
fn window_update_roundtrip() {
    let mut buf = [0u8; 17];
    let n = encode_window_update(&mut buf, 5, 65536);
    assert_eq!(collect_frames(&buf[..n])[0], Frame::WindowUpdate { stream_id: 5, max_offset: 65536 });
}

#[test]
fn channel_fin_roundtrip() {
    let mut buf = [0u8; 17];
    let n = encode_channel_fin(&mut buf, 42, 7);
    assert_eq!(
        collect_frames(&buf[..n])[0],
        Frame::ChannelFin { channel_id: 42, last_message_id: 7 }
    );
}

#[test]
fn max_channels_roundtrip() {
    let mut buf = [0u8; 9];
    let n = encode_max_channels(&mut buf, 256);
    assert_eq!(collect_frames(&buf[..n])[0], Frame::MaxChannels { count: 256 });
}

#[test]
fn channel_with_fin() {
    let mut buf = [0u8; 128];
    let h = encode_channel_header(&mut buf, 1, 2, 0, 3, true);
    buf[h..h + 3].copy_from_slice(b"fin");
    match &collect_frames(&buf[..h + 3])[0] {
        Frame::Channel { fin, .. } => assert!(*fin),
        _ => panic!("expected Channel"),
    }
}

#[test]
fn auth_no_fin() {
    let mut buf = [0u8; 128];
    let h = encode_auth_header(&mut buf, 0, 4, false);
    buf[h..h + 4].copy_from_slice(b"data");
    match &collect_frames(&buf[..h + 4])[0] {
        Frame::Auth { fin, .. } => assert!(!fin),
        _ => panic!("expected Auth"),
    }
}

#[test]
fn multiple_frames() {
    let mut buf = [0u8; 256];
    let mut pos = 0;
    pos += encode_ping(&mut buf[pos..], 1);
    pos += encode_pong(&mut buf[pos..], 1);
    let h = encode_stream_header(&mut buf[pos..], 0, 0, 5, true);
    buf[pos + h..pos + h + 5].copy_from_slice(b"hello");
    pos += h + 5;
    let frames = collect_frames(&buf[..pos]);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], Frame::Ping { id: 1 });
    assert_eq!(frames[1], Frame::Pong { id: 1 });
    assert!(matches!(&frames[2], Frame::Stream { fin: true, data: b"hello", .. }));
}

#[test]
fn padding_skipped() {
    let mut buf = [0u8; 16];
    buf[0] = PADDING;
    buf[1] = PADDING;
    let n = encode_ping(&mut buf[2..], 99);
    let frames = collect_frames(&buf[..2 + n]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], Frame::Ping { id: 99 });
}

#[test]
fn empty_payload() {
    assert!(collect_frames(&[]).is_empty());
}

#[test]
fn invalid_type() { expect_err(&[0x1F, 0, 0, 0, 0], FrameError::InvalidType(0x1F)); }
#[test]
fn truncated_ping() { expect_err(&[PING, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_ack() { expect_err(&[ACK, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_stream() { expect_err(&[STREAM_BASE, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_channel() { expect_err(&[CHANNEL_BASE, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_auth() { expect_err(&[AUTH_BASE, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_connection_close() { expect_err(&[CONNECTION_CLOSE, 0, 0], FrameError::UnexpectedEnd); }
#[test]
fn truncated_window_update() { expect_err(&[WINDOW_UPDATE, 0, 0], FrameError::UnexpectedEnd); }

#[test]
fn truncated_stream_at_len() {
    let mut buf = vec![STREAM_BASE];
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&0u64.to_be_bytes());
    expect_err(&buf, FrameError::UnexpectedEnd);
}

#[test]
fn truncated_stream_at_data() {
    let mut buf = vec![STREAM_BASE];
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&10u16.to_be_bytes());
    buf.extend_from_slice(&[0u8; 5]);
    expect_err(&buf, FrameError::UnexpectedEnd);
}

#[test]
fn truncated_ack_no_count() {
    let mut buf = vec![ACK];
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
    expect_err(&buf, FrameError::UnexpectedEnd);
}

// -- Packet --

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

#[test]
fn parse_init_wrong_type() {
    let mut buf = [0u8; 2000];
    buf[0] = 0xFF; // wrong type
    assert!(matches!(parse_init(&buf), Err(PacketError::InvalidType(0xFF))));
}

#[test]
fn parse_init_ack_wrong_type() {
    let mut buf = [0u8; 2000];
    buf[0] = 0xFF;
    assert!(matches!(parse_init_ack(&buf), Err(PacketError::InvalidType(0xFF))));
}

#[test]
fn encode_rekey_without_fin() {
    let mut buf = [0u8; 20];
    let n = encode_rekey_header(&mut buf, 0, 5, false);
    assert_eq!(n, REKEY_HEADER_SIZE);
    assert_eq!(buf[0], REKEY_BASE); // no FIN bit
}

#[test]
fn encode_rekey_ack_without_fin() {
    let mut buf = [0u8; 20];
    let n = encode_rekey_ack_header(&mut buf, 0, 5, false);
    assert_eq!(n, REKEY_ACK_HEADER_SIZE);
    assert_eq!(buf[0], REKEY_ACK_BASE); // no FIN bit
}

#[test]
fn channel_open_roundtrip() {
    let mut buf = [0u8; 20];
    let n = encode_channel_open(&mut buf, 42);
    let frames = collect_frames(&buf[..n]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], Frame::ChannelOpen { channel_id: 42 });
}
