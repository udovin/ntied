use std::time::Instant;

use super::*;
use crate::crypto::{EncryptionKeys, EphemeralPrivateKey, PrivateKey, compute_transcript_hash};
use crate::session::{Role, Session};
use crate::wire::Frame;

struct TestPair {
    initiator: PeerConnection,
    responder: PeerConnection,
}

fn make_test_pair() -> TestPair {
    let init_identity = PrivateKey::generate();
    let resp_identity = PrivateKey::generate();

    let init_eph = EphemeralPrivateKey::generate();
    let resp_eph = EphemeralPrivateKey::generate();
    let init_pk = init_eph.public_key();
    let (ct, resp_ss) = resp_eph.encapsulate(&init_pk).unwrap();
    let init_ss = init_eph.decapsulate(&ct).unwrap();

    let keys_i = EncryptionKeys::new(&init_ss, &init_pk, &ct);
    let keys_r = EncryptionKeys::new(&resp_ss, &init_pk, &ct);
    let th = compute_transcript_hash(&init_pk, &ct);

    let session_i = Session::new(Role::Initiator, 1, keys_i, th);
    let session_r = Session::new(Role::Responder, 1, keys_r, th);

    let init_auth = build_auth_payload(&init_identity, &th);
    let resp_auth = build_auth_payload(&resp_identity, &th);

    let initiator = PeerConnection::new(session_i, 100, 200, true, init_auth);
    let responder = PeerConnection::new(session_r, 200, 100, false, resp_auth);

    TestPair {
        initiator,
        responder,
    }
}

fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();
    let sig = identity.sign(transcript_hash);
    let mut payload = Vec::new();
    payload.extend_from_slice(&pk.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());
    payload
}

fn deliver(src: &mut PeerConnection, dst: &mut PeerConnection, now: Instant) -> usize {
    let packets = src.poll_packets(now);
    let count = packets.len();
    for data in packets {
        dst.on_data_packet(data, now);
    }
    count
}

fn complete_auth(pair: &mut TestPair, now: Instant) {
    for _ in 0..10 {
        if pair.initiator.is_established() && pair.responder.is_established() {
            return;
        }
        deliver(&mut pair.initiator, &mut pair.responder, now);
        deliver(&mut pair.responder, &mut pair.initiator, now);
    }
    panic!("auth did not complete within 10 rounds");
}

#[test]
fn auth_completes() {
    let mut pair = make_test_pair();
    let now = Instant::now();

    assert!(!pair.initiator.is_established());
    assert!(!pair.responder.is_established());

    complete_auth(&mut pair, now);

    assert!(pair.initiator.is_established());
    assert!(pair.responder.is_established());
}

#[test]
fn session_ids() {
    let pair = make_test_pair();

    assert_eq!(pair.initiator.local_session_id(), 100);
    assert_eq!(pair.initiator.remote_session_id(), 200);
    assert_eq!(pair.responder.local_session_id(), 200);
    assert_eq!(pair.responder.remote_session_id(), 100);
}

#[test]
fn stream_data_exchange() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let stream_id = pair.initiator.open_channel(42);
    pair.initiator
        .write(stream_id, b"hello from initiator")
        .unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (accepted_id, purpose) = pair.responder.accept_channel().unwrap();
    assert_eq!(purpose, 42);

    let data = pair.responder.read(accepted_id).unwrap().unwrap();
    assert_eq!(data, b"hello from initiator");
}

#[test]
fn bidirectional_stream() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid_i = pair.initiator.open_channel(1);
    pair.initiator.write(sid_i, b"ping").unwrap();
    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (sid_r, _) = pair.responder.accept_channel().unwrap();
    let data = pair.responder.read(sid_r).unwrap().unwrap();
    assert_eq!(data, b"ping");

    pair.responder.write(sid_r, b"pong").unwrap();
    deliver(&mut pair.responder, &mut pair.initiator, now);

    let data = pair.initiator.read(sid_i).unwrap().unwrap();
    assert_eq!(data, b"pong");
}

#[test]
fn multiple_streams() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let s1 = pair.initiator.open_channel(10);
    let s2 = pair.initiator.open_channel(20);

    pair.initiator.write(s1, b"stream-one").unwrap();
    pair.initiator.write(s2, b"stream-two").unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (a1, p1) = pair.responder.accept_channel().unwrap();
    let (a2, p2) = pair.responder.accept_channel().unwrap();

    let mut received = vec![
        (p1, pair.responder.read(a1).unwrap().unwrap()),
        (p2, pair.responder.read(a2).unwrap().unwrap()),
    ];
    received.sort_by_key(|(p, _)| *p);

    assert_eq!(received[0].0, 10);
    assert_eq!(received[0].1, b"stream-one");
    assert_eq!(received[1].0, 20);
    assert_eq!(received[1].1, b"stream-two");
}

#[test]
fn close_channel_sends_fin() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);
    pair.initiator.write(sid, b"last").unwrap();
    pair.initiator.close_channel(sid).unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (rsid, _) = pair.responder.accept_channel().unwrap();
    let data = pair.responder.read(rsid).unwrap().unwrap();
    assert_eq!(data, b"last");
    assert!(pair.responder.is_channel_finished(rsid));
}

#[test]
fn responder_opens_stream() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.responder.open_channel(99);
    pair.responder.write(sid, b"from responder").unwrap();

    deliver(&mut pair.responder, &mut pair.initiator, now);

    let (accepted, purpose) = pair.initiator.accept_channel().unwrap();
    assert_eq!(purpose, 99);

    let data = pair.initiator.read(accepted).unwrap().unwrap();
    assert_eq!(data, b"from responder");
}

#[test]
fn duplicate_packet_ignored() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);
    pair.initiator.write(sid, b"once").unwrap();

    let packets = pair.initiator.poll_packets(now);
    assert!(!packets.is_empty());

    for data in &packets {
        pair.responder.on_data_packet(data.clone(), now);
    }
    for data in &packets {
        pair.responder.on_data_packet(data.clone(), now);
    }

    let (rsid, _) = pair.responder.accept_channel().unwrap();
    let data = pair.responder.read(rsid).unwrap().unwrap();
    assert_eq!(data, b"once");
    assert!(pair.responder.accept_channel().is_none());
}

#[test]
fn ack_round_trip() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);
    pair.initiator.write(sid, b"data").unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);
    deliver(&mut pair.responder, &mut pair.initiator, now);

    assert_eq!(pair.initiator.in_flight_count(), 0);
}

#[test]
fn loss_detection_retransmits() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);

    pair.initiator.write(sid, b"msg-0").unwrap();
    let lost_packets = pair.initiator.poll_packets(now);
    assert!(!lost_packets.is_empty());

    for i in 1u32..5 {
        pair.initiator
            .write(sid, format!("msg-{i}").as_bytes())
            .unwrap();
        deliver(&mut pair.initiator, &mut pair.responder, now);
        deliver(&mut pair.responder, &mut pair.initiator, now);
    }

    let (rsid, _) = pair.responder.accept_channel().unwrap();

    let retransmit_packets = pair.initiator.poll_packets(now);
    for data in retransmit_packets {
        pair.responder.on_data_packet(data, now);
    }

    let mut all_data = Vec::new();
    while let Some(chunk) = pair.responder.read(rsid).unwrap() {
        all_data.extend_from_slice(&chunk);
    }

    assert!(all_data.starts_with(b"msg-0") || all_data.windows(5).any(|w| w == b"msg-0"));
}

#[test]
fn large_data_multiple_packets() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);

    let large_data = vec![0xABu8; 8000];
    pair.initiator.write(sid, &large_data).unwrap();

    let packets = pair.initiator.poll_packets(now);
    assert!(packets.len() > 1);

    for data in packets {
        pair.responder.on_data_packet(data, now);
    }

    let (rsid, _) = pair.responder.accept_channel().unwrap();
    let mut received = Vec::new();
    while let Some(chunk) = pair.responder.read(rsid).unwrap() {
        received.extend_from_slice(&chunk);
    }

    assert_eq!(received.len(), 8000);
    assert!(received.iter().all(|&b| b == 0xAB));
}

#[test]
fn has_pending_reflects_state() {
    let mut pair = make_test_pair();
    let now = Instant::now();

    assert!(pair.initiator.has_pending());

    complete_auth(&mut pair, now);

    let packets = pair.initiator.poll_packets(now);
    for data in packets {
        pair.responder.on_data_packet(data, now);
    }
    let packets = pair.responder.poll_packets(now);
    for data in packets {
        pair.initiator.on_data_packet(data, now);
    }

    let _ = pair.initiator.poll_packets(now);
    let _ = pair.responder.poll_packets(now);

    let sid = pair.initiator.open_channel(1);
    assert!(pair.initiator.has_pending());

    pair.initiator.write(sid, b"x").unwrap();
    assert!(pair.initiator.has_pending());

    deliver(&mut pair.initiator, &mut pair.responder, now);
}

#[test]
fn write_before_established_queues() {
    let mut pair = make_test_pair();
    let now = Instant::now();

    let sid = pair.initiator.open_channel(1);
    pair.initiator.write(sid, b"early").unwrap();

    complete_auth(&mut pair, now);

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (rsid, _) = pair.responder.accept_channel().unwrap();
    let data = pair.responder.read(rsid).unwrap().unwrap();
    assert_eq!(data, b"early");
}

#[test]
fn gateway_frames_returned_as_unhandled() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let dest = crate::crypto::PrivateKey::generate().public_key().peer_id();
    let src = crate::crypto::PrivateKey::generate().public_key().peer_id();
    pair.initiator
        .queue_frame(Frame::GatewayPacket(crate::wire::GatewayPacket {
            dest_peer_id: dest,
            src_peer_id: src,
            inner: vec![0xAA, 0xBB],
        }));

    let packets = pair.initiator.poll_packets(now);
    assert!(!packets.is_empty());

    let mut all_unhandled = Vec::new();
    for data in packets {
        let unhandled = pair.responder.on_data_packet(data, now);
        all_unhandled.extend(unhandled);
    }

    assert_eq!(all_unhandled.len(), 1);
    match &all_unhandled[0] {
        Frame::GatewayPacket(pkt) => {
            assert_eq!(pkt.dest_peer_id, dest);
            assert_eq!(pkt.inner, vec![0xAA, 0xBB]);
        }
        _ => panic!("expected GatewayPacket"),
    }
}

#[test]
fn connection_frames_not_returned_as_unhandled() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_channel(1);
    pair.initiator.write(sid, b"hello").unwrap();

    let packets = pair.initiator.poll_packets(now);
    let mut all_unhandled = Vec::new();
    for data in packets {
        let unhandled = pair.responder.on_data_packet(data, now);
        all_unhandled.extend(unhandled);
    }

    assert!(all_unhandled.is_empty());
    let (rsid, _) = pair.responder.accept_channel().unwrap();
    assert!(pair.responder.read(rsid).unwrap().is_some());
}
