use std::time::{Duration, Instant};

use super::ack::*;
use super::core::TransportCore;
use super::fragment::*;
use super::*;
use crate::crypto::{EncryptionKeys, KemPrivateKey, PrivateKey, compute_transcript_hash};
use crate::session::{Role, Session};
use crate::wire::packet::MAX_PACKET_PAYLOAD;
use crate::wire::{Ack, AckRange, Frame, Ping, Pong, StreamData, WindowUpdate};

// ── helpers ──

fn ping(id: u32) -> Frame {
    Frame::Ping(Ping { ping_id: id })
}

fn pong(id: u32) -> Frame {
    Frame::Pong(Pong { ping_id: id })
}

fn window_update(channel_id: u32) -> Frame {
    Frame::WindowUpdate(WindowUpdate {
        channel_id,
        max_offset: 0,
    })
}

fn ack_single(largest: u64) -> Ack {
    Ack {
        largest_ack: largest,
        ack_delay: 0,
        ranges: vec![AckRange { gap: 0, length: 1 }],
    }
}

fn ack_range(largest: u64, length: u64) -> Ack {
    Ack {
        largest_ack: largest,
        ack_delay: 0,
        ranges: vec![AckRange { gap: 0, length }],
    }
}

struct TestPair {
    initiator: PeerConnection,
    responder: PeerConnection,
}

fn make_test_pair() -> TestPair {
    let init_identity = PrivateKey::generate();
    let resp_identity = PrivateKey::generate();

    let init_eph = KemPrivateKey::generate();
    let resp_eph = KemPrivateKey::generate();
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

// ── fragment collector ──

#[test]
fn fragment_collector_basic() {
    let mut collector = FragmentCollector::new();

    let res = collector.add_fragment(0, 3, b"hello ");
    assert!(res.is_none());

    let res = collector.add_fragment(1, 3, b"world");
    assert!(res.is_none());

    let res = collector.add_fragment(2, 3, b"!");
    assert_eq!(res.unwrap(), b"hello world!");
}

#[test]
fn fragment_collector_out_of_order() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(2, 3, b"!");
    collector.add_fragment(0, 3, b"hello ");
    let res = collector.add_fragment(1, 3, b"world");

    assert_eq!(res.unwrap(), b"hello world!");
}

#[test]
fn fragment_collector_duplicate_fragment() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(0, 2, b"a");
    collector.add_fragment(0, 2, b"a");
    let res = collector.add_fragment(1, 2, b"b");

    assert_eq!(res.unwrap(), b"ab");
}

#[test]
fn fragment_collector_total_changed() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(0, 3, b"a");
    let res = collector.add_fragment(0, 2, b"hello ");
    assert!(res.is_none());
    let res = collector.add_fragment(1, 2, b"world");

    assert_eq!(res.unwrap(), b"hello world");
}

#[test]
fn fragment_collector_invalid_index() {
    let mut collector = FragmentCollector::new();
    let res = collector.add_fragment(2, 2, b"out of bounds");
    assert!(res.is_none());
}

// ── ack tracker ──

#[test]
fn recv_accept_duplicate_below_floor() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    assert_eq!(recv.receive(0, now), RecvResult::Accepted);
    assert_eq!(recv.receive(5, now), RecvResult::Accepted);
    assert_eq!(recv.receive(5, now), RecvResult::Duplicate);
    assert_eq!(recv.receive(0, now), RecvResult::Duplicate);
    assert_eq!(recv.largest(), Some(5));

    recv.advance_floor(3);
    assert_eq!(recv.receive(2, now), RecvResult::BelowFloor);
    assert_eq!(recv.receive(3, now), RecvResult::Accepted);
}

#[test]
fn recv_range_merging() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    recv.receive(1, now);
    recv.receive(3, now);
    recv.receive(2, now);

    let ack = recv.generate_ack(now).unwrap();
    assert_eq!(ack.largest_ack, 3);
    assert_eq!(ack.ranges.len(), 1);
    assert_eq!(ack.ranges[0], AckRange { gap: 0, length: 3 });
}

#[test]
fn recv_generate_ack_with_gaps() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    for c in [1, 2, 3, 5, 6, 8, 9, 10] {
        recv.receive(c, now);
    }

    let ack = recv.generate_ack(now).unwrap();
    assert_eq!(ack.largest_ack, 10);
    assert_eq!(ack.ranges.len(), 3);
    assert_eq!(ack.ranges[0], AckRange { gap: 0, length: 3 });
    assert_eq!(ack.ranges[1], AckRange { gap: 1, length: 2 });
    assert_eq!(ack.ranges[2], AckRange { gap: 1, length: 3 });
}

#[test]
fn recv_generate_ack_empty() {
    let mut recv = RecvAckState::new();
    assert!(recv.generate_ack(Instant::now()).is_none());
}

#[test]
fn recv_advance_floor_trims_ranges() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    for c in 1..=10 {
        recv.receive(c, now);
    }

    recv.advance_floor(6);
    assert_eq!(recv.floor(), 6);
    assert_eq!(recv.receive(5, now), RecvResult::BelowFloor);
    assert_eq!(recv.receive(6, now), RecvResult::Duplicate);
    assert_eq!(recv.receive(11, now), RecvResult::Accepted);

    let ack = recv.generate_ack(now).unwrap();
    assert_eq!(ack.largest_ack, 11);
}

#[test]
fn recv_advance_floor_partial_trim() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    for c in [1, 2, 3, 7, 8, 9] {
        recv.receive(c, now);
    }

    recv.advance_floor(5);

    assert_eq!(recv.receive(3, now), RecvResult::BelowFloor);
    assert_eq!(recv.receive(7, now), RecvResult::Duplicate);
    assert_eq!(recv.receive(5, now), RecvResult::Accepted);
}

#[test]
fn decode_ack_ranges_roundtrip() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    for c in [1, 2, 3, 5, 6, 8, 9, 10] {
        recv.receive(c, now);
    }

    let ack = recv.generate_ack(now).unwrap();
    let decoded = decode_ack_ranges(&ack);

    assert_eq!(decoded, vec![(8, 10), (5, 6), (1, 3)]);
}

#[test]
fn decode_ack_ranges_single() {
    let ack = ack_range(5, 3);
    let decoded = decode_ack_ranges(&ack);
    assert_eq!(decoded, vec![(3, 5)]);
}

#[test]
fn decode_ack_ranges_zero_length_stops() {
    let ack = Ack {
        largest_ack: 10,
        ack_delay: 0,
        ranges: vec![
            AckRange { gap: 0, length: 0 },
            AckRange { gap: 0, length: 3 },
        ],
    };
    let decoded = decode_ack_ranges(&ack);
    assert!(decoded.is_empty());
}

#[test]
fn send_filters_non_ack_eliciting() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![pong(1)], now);
    assert_eq!(send.in_flight_count(), 0);

    send.on_packet_sent(1, vec![window_update(1)], now);
    assert_eq!(send.in_flight_count(), 0);

    send.on_packet_sent(2, vec![ping(1)], now);
    assert_eq!(send.in_flight_count(), 1);

    send.on_packet_sent(3, vec![pong(2), ping(2), ping(3)], now);
    assert_eq!(send.in_flight_count(), 2);
}

#[test]
fn send_ack_removes_entries() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);
    send.on_packet_sent(1, vec![ping(1)], now);
    send.on_packet_sent(2, vec![ping(2)], now);

    let lost = send.on_ack_received(&ack_range(1, 2), now);
    assert!(lost.is_empty());
    assert_eq!(send.in_flight_count(), 1);
    assert_eq!(send.largest_acked(), Some(1));
}

#[test]
fn send_loss_by_gap() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    for i in 0..5 {
        send.on_packet_sent(i, vec![ping(i as u32)], now);
    }

    let ack = ack_range(4, 4);
    let lost = send.on_ack_received(&ack, now);

    assert_eq!(lost.len(), 1);
    assert!(matches!(lost[0], Frame::Ping(Ping { ping_id: 0 })));
    assert_eq!(send.in_flight_count(), 0);
}

#[test]
fn send_no_false_gap_loss_when_largest_small() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);
    send.on_packet_sent(1, vec![ping(1)], now);

    let lost = send.on_ack_received(&ack_single(1), now);
    assert!(lost.is_empty());
    assert_eq!(send.in_flight_count(), 1);
}

#[test]
fn send_loss_by_timeout() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);
    send.on_packet_sent(1, vec![ping(1)], now);

    let lost = send.on_ack_received(&ack_single(1), now);
    assert!(lost.is_empty());

    let after_rto = now + INITIAL_RTO + Duration::from_millis(1);
    let lost = send.on_ack_received(&ack_single(1), after_rto);
    assert_eq!(lost.len(), 1);
    assert!(matches!(lost[0], Frame::Ping(Ping { ping_id: 0 })));
}

#[test]
fn send_rtt_measurement() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);

    let later = now + Duration::from_millis(100);
    send.on_ack_received(&ack_single(0), later);

    assert_eq!(send.srtt(), Some(Duration::from_millis(100)));
    assert_eq!(send.rto(), Duration::from_millis(300));
}

#[test]
fn send_rtt_with_ack_delay() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);

    let ack = Ack {
        largest_ack: 0,
        ack_delay: 20_000,
        ranges: vec![AckRange { gap: 0, length: 1 }],
    };

    let later = now + Duration::from_millis(100);
    send.on_ack_received(&ack, later);

    assert_eq!(send.srtt(), Some(Duration::from_millis(80)));
}

#[test]
fn send_rtt_converges() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);
    let t1 = now + Duration::from_millis(100);
    send.on_ack_received(&ack_single(0), t1);

    send.on_packet_sent(1, vec![ping(1)], t1);
    let t2 = t1 + Duration::from_millis(100);
    send.on_ack_received(&ack_single(1), t2);

    let srtt = send.srtt().unwrap();
    assert!(srtt >= Duration::from_millis(90) && srtt <= Duration::from_millis(110));
}

#[test]
fn send_loss_returns_all_frames_from_packet() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0), ping(1), pong(2)], now);
    for i in 1..=4 {
        send.on_packet_sent(i, vec![ping(10 + i as u32)], now);
    }

    let lost = send.on_ack_received(&ack_range(4, 4), now);

    assert_eq!(lost.len(), 2);
    assert!(matches!(lost[0], Frame::Ping(Ping { ping_id: 0 })));
    assert!(matches!(lost[1], Frame::Ping(Ping { ping_id: 1 })));
}

#[test]
fn send_empty_in_flight_on_ack() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    let lost = send.on_ack_received(&ack_single(5), now);
    assert!(lost.is_empty());
    assert_eq!(send.in_flight_count(), 0);
}

#[test]
fn recv_merge_next_only() {
    let mut recv = RecvAckState::new();
    let now = Instant::now();

    recv.receive(5, now);
    recv.receive(6, now);
    recv.receive(4, now);

    let ack = recv.generate_ack(now).unwrap();
    assert_eq!(ack.ranges.len(), 1);
    assert_eq!(ack.ranges[0], AckRange { gap: 0, length: 3 });
}

#[test]
fn send_rtt_decreases() {
    let mut send = SendAckState::new();
    let now = Instant::now();

    send.on_packet_sent(0, vec![ping(0)], now);
    let t1 = now + Duration::from_millis(200);
    send.on_ack_received(&ack_single(0), t1);
    assert_eq!(send.srtt(), Some(Duration::from_millis(200)));

    send.on_packet_sent(1, vec![ping(1)], t1);
    let t2 = t1 + Duration::from_millis(50);
    send.on_ack_received(&ack_single(1), t2);

    let srtt = send.srtt().unwrap();
    assert!(srtt < Duration::from_millis(200));
    assert!(srtt > Duration::from_millis(50));
}

#[test]
fn decode_ack_ranges_cursor_underflow() {
    let ack = Ack {
        largest_ack: 2,
        ack_delay: 0,
        ranges: vec![
            AckRange { gap: 0, length: 1 },
            AckRange { gap: 10, length: 1 },
        ],
    };
    let decoded = decode_ack_ranges(&ack);
    assert_eq!(decoded, vec![(2, 2)]);
}

// ── peer connection ──

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
fn connection_ids() {
    let pair = make_test_pair();

    assert_eq!(pair.initiator.local_connection_id(), 100);
    assert_eq!(pair.initiator.remote_connection_id(), 200);
    assert_eq!(pair.responder.local_connection_id(), 200);
    assert_eq!(pair.responder.remote_connection_id(), 100);
}

#[test]
fn stream_data_exchange() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let stream_id = pair.initiator.open_stream(42);
    pair.initiator
        .write(stream_id, b"hello from initiator")
        .unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (accepted_id, purpose) = pair.responder.accept_stream().unwrap();
    assert_eq!(purpose, 42);

    let data = pair.responder.read(accepted_id).unwrap().unwrap();
    assert_eq!(data, b"hello from initiator");
}

#[test]
fn bidirectional_stream() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid_i = pair.initiator.open_stream(1);
    pair.initiator.write(sid_i, b"ping").unwrap();
    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (sid_r, _) = pair.responder.accept_stream().unwrap();
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

    let s1 = pair.initiator.open_stream(10);
    let s2 = pair.initiator.open_stream(20);

    pair.initiator.write(s1, b"stream-one").unwrap();
    pair.initiator.write(s2, b"stream-two").unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (a1, p1) = pair.responder.accept_stream().unwrap();
    let (a2, p2) = pair.responder.accept_stream().unwrap();

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

    let sid = pair.initiator.open_stream(1);
    pair.initiator.write(sid, b"last").unwrap();
    pair.initiator.close_channel(sid).unwrap();

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (rsid, _) = pair.responder.accept_stream().unwrap();
    let data = pair.responder.read(rsid).unwrap().unwrap();
    assert_eq!(data, b"last");
    assert!(pair.responder.is_channel_finished(rsid));
}

#[test]
fn responder_opens_stream() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.responder.open_stream(99);
    pair.responder.write(sid, b"from responder").unwrap();

    deliver(&mut pair.responder, &mut pair.initiator, now);

    let (accepted, purpose) = pair.initiator.accept_stream().unwrap();
    assert_eq!(purpose, 99);

    let data = pair.initiator.read(accepted).unwrap().unwrap();
    assert_eq!(data, b"from responder");
}

#[test]
fn duplicate_packet_ignored() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_stream(1);
    pair.initiator.write(sid, b"once").unwrap();

    let packets = pair.initiator.poll_packets(now);
    assert!(!packets.is_empty());

    for data in &packets {
        pair.responder.on_data_packet(data.clone(), now);
    }
    for data in &packets {
        pair.responder.on_data_packet(data.clone(), now);
    }

    let (rsid, _) = pair.responder.accept_stream().unwrap();
    let data = pair.responder.read(rsid).unwrap().unwrap();
    assert_eq!(data, b"once");
    assert!(pair.responder.accept_stream().is_none());
}

#[test]
fn ack_round_trip() {
    let mut pair = make_test_pair();
    let now = Instant::now();
    complete_auth(&mut pair, now);

    let sid = pair.initiator.open_stream(1);
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

    let sid = pair.initiator.open_stream(1);

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

    let (rsid, _) = pair.responder.accept_stream().unwrap();

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

    let sid = pair.initiator.open_stream(1);

    let large_data = vec![0xABu8; 8000];
    pair.initiator.write(sid, &large_data).unwrap();

    let packets = pair.initiator.poll_packets(now);
    assert!(packets.len() > 1);

    for data in packets {
        pair.responder.on_data_packet(data, now);
    }

    let (rsid, _) = pair.responder.accept_stream().unwrap();
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

    let sid = pair.initiator.open_stream(1);
    assert!(pair.initiator.has_pending());

    pair.initiator.write(sid, b"x").unwrap();
    assert!(pair.initiator.has_pending());

    deliver(&mut pair.initiator, &mut pair.responder, now);
}

#[test]
fn write_before_established_queues() {
    let mut pair = make_test_pair();
    let now = Instant::now();

    let sid = pair.initiator.open_stream(1);
    pair.initiator.write(sid, b"early").unwrap();

    complete_auth(&mut pair, now);

    deliver(&mut pair.initiator, &mut pair.responder, now);

    let (rsid, _) = pair.responder.accept_stream().unwrap();
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

    let sid = pair.initiator.open_stream(1);
    pair.initiator.write(sid, b"hello").unwrap();

    let packets = pair.initiator.poll_packets(now);
    let mut all_unhandled = Vec::new();
    for data in packets {
        let unhandled = pair.responder.on_data_packet(data, now);
        all_unhandled.extend(unhandled);
    }

    assert!(all_unhandled.is_empty());
    let (rsid, _) = pair.responder.accept_stream().unwrap();
    assert!(pair.responder.read(rsid).unwrap().is_some());
}

// ── TransportCore tests ──

struct CorePair {
    sender: TransportCore,
    receiver: TransportCore,
}

fn make_core_pair() -> CorePair {
    let init_identity = PrivateKey::generate();
    let resp_identity = PrivateKey::generate();

    let init_eph = KemPrivateKey::generate();
    let resp_eph = KemPrivateKey::generate();
    let init_pk = init_eph.public_key();
    let (ct, resp_ss) = resp_eph.encapsulate(&init_pk).unwrap();
    let init_ss = init_eph.decapsulate(&ct).unwrap();

    let keys_s = EncryptionKeys::new(&init_ss, &init_pk, &ct);
    let keys_r = EncryptionKeys::new(&resp_ss, &init_pk, &ct);
    let th = compute_transcript_hash(&init_pk, &ct);

    let session_s = Session::new(Role::Initiator, 1, keys_s, th);
    let session_r = Session::new(Role::Responder, 1, keys_r, th);

    let auth_s = build_auth_payload(&init_identity, &th);
    let auth_r = build_auth_payload(&resp_identity, &th);

    CorePair {
        sender: TransportCore::new(session_s, 100, 200, auth_s),
        receiver: TransportCore::new(session_r, 200, 100, auth_r),
    }
}

fn complete_core_auth(pair: &mut CorePair, now: Instant) {
    for _ in 0..10 {
        if pair.sender.is_established() && pair.receiver.is_established() {
            // Drain remaining packets: flush + deliver any acks
            for _ in 0..3 {
                pair.sender.flush(now);
                for pkt in pair.sender.send_packets() {
                    pair.receiver.receive_packet(pkt, now);
                }
                pair.receiver.flush(now);
                for pkt in pair.receiver.send_packets() {
                    pair.sender.receive_packet(pkt, now);
                }
            }
            // Final drain
            let _ = pair.sender.send_packets();
            let _ = pair.receiver.send_packets();
            return;
        }
        // sender → receiver
        pair.sender.flush(now);
        for pkt in pair.sender.send_packets() {
            pair.receiver.receive_packet(pkt, now);
        }
        // receiver → sender
        pair.receiver.flush(now);
        for pkt in pair.receiver.send_packets() {
            pair.sender.receive_packet(pkt, now);
        }
    }
    panic!("core auth did not complete within 10 rounds");
}

fn make_large_frame(size: usize) -> Frame {
    Frame::StreamData(StreamData {
        channel_id: 1,
        offset: 0,
        fin: false,
        data: vec![0xAB; size],
    })
}

#[test]
fn core_queue_frame_auto_builds_on_mtu() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    // Queue small frames — should not produce packets yet
    pair.sender.queue_frame(ping(1));
    assert!(pair.sender.send_packets().is_empty(), "small frame should not trigger auto-build");

    // Queue a large frame that exceeds MTU — should auto-build
    pair.sender.queue_frame(make_large_frame(MAX_PACKET_PAYLOAD));
    let packets = pair.sender.send_packets();
    assert!(!packets.is_empty(), "exceeding MTU should trigger auto-build");
}

#[test]
fn core_flush_sends_pending_with_ack() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    // Send a packet from sender to receiver so receiver has something to ack
    pair.sender.queue_frame(ping(42));
    pair.sender.flush(now);
    let packets = pair.sender.send_packets();
    assert_eq!(packets.len(), 1);

    // Receiver processes it
    let passthrough = pair.receiver.receive_packet(packets.into_iter().next().unwrap(), now);
    assert!(passthrough.is_some());

    // Receiver flushes — should produce ack + pong
    pair.receiver.flush(now);
    let packets = pair.receiver.send_packets();
    assert!(!packets.is_empty(), "flush should produce ack + pong");
}

#[test]
fn core_flush_empty_produces_nothing() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    // First flush may still have a pending ack from auth — drain it
    pair.sender.flush(now);
    let _ = pair.sender.send_packets();

    // Second flush with truly nothing pending
    pair.sender.flush(now);
    let packets = pair.sender.send_packets();
    assert!(packets.is_empty(), "flush with no pending data should produce nothing");
}

#[test]
fn core_receive_packet_urgent_flush() {
    let mut pair = make_core_pair();
    let now = Instant::now();

    // Before auth: sender has auth fragments pending.
    // Flush and send to receiver.
    pair.sender.flush(now);
    let packets = pair.sender.send_packets();
    assert!(!packets.is_empty(), "sender should have auth frames");

    // Receiver processes auth — should generate AuthComplete internally
    // and flush it immediately (urgent).
    for pkt in packets {
        pair.receiver.receive_packet(pkt, now);
    }

    // receive_packet should have auto-flushed the AuthComplete
    let packets = pair.receiver.send_packets();
    assert!(!packets.is_empty(), "auth response should be auto-flushed as urgent");
}

#[test]
fn core_send_packets_drains() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    pair.sender.queue_frame(ping(1));
    pair.sender.flush(now);

    let packets = pair.sender.send_packets();
    assert!(!packets.is_empty());

    // Second call should return empty — already drained
    let packets2 = pair.sender.send_packets();
    assert!(packets2.is_empty(), "send_packets should drain");
}

#[test]
fn core_counter_not_burned_on_decrypt_failure() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    // Send a real packet
    pair.sender.queue_frame(ping(1));
    pair.sender.flush(now);
    let mut packets = pair.sender.send_packets();
    assert_eq!(packets.len(), 1);
    let real_packet = packets.remove(0);
    let counter = real_packet.counter;

    // Send a fake packet with the same counter but garbage payload
    let fake_packet = crate::wire::packet::Data {
        epoch: real_packet.epoch,
        receiver_connection_id: real_packet.receiver_connection_id,
        counter,
        encrypted_payload: vec![0xFF; 100],
    };

    // Fake packet should be rejected (decryption fails)
    let result = pair.receiver.receive_packet(fake_packet, now);
    assert!(result.is_none(), "fake packet should be rejected");

    // Real packet with same counter should still be accepted
    let result = pair.receiver.receive_packet(real_packet, now);
    assert!(result.is_some(), "real packet should not be rejected after fake with same counter");
}

#[test]
fn core_high_throughput_auto_sends() {
    let mut pair = make_core_pair();
    let now = Instant::now();
    complete_core_auth(&mut pair, now);

    // Queue many frames to exceed MTU multiple times
    for i in 0..20 {
        pair.sender.queue_frame(make_large_frame(200));
    }

    // Should have auto-built packets as MTU was reached
    let packets = pair.sender.send_packets();
    assert!(packets.len() > 1, "high throughput should produce multiple auto-built packets");

    // Flush remainder
    pair.sender.flush(now);
    let remaining = pair.sender.send_packets();
    // May or may not have remaining depending on exact sizes

    // All packets should be decryptable
    let total = packets.len() + remaining.len();
    let mut received = 0;
    for pkt in packets.into_iter().chain(remaining.into_iter()) {
        if pair.receiver.receive_packet(pkt, now).is_some() {
            received += 1;
        }
    }
    assert_eq!(received, total, "all packets should decrypt successfully");
}
