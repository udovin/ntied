use std::time::{Duration, Instant};

use super::*;
use crate::wire::{Ack, AckRange, Frame, Ping, Pong, WindowUpdate};

fn ping(id: u32) -> Frame {
    Frame::Ping(Ping { ping_id: id })
}

fn pong(id: u32) -> Frame {
    Frame::Pong(Pong { ping_id: id })
}

fn window_update(stream_id: u32) -> Frame {
    Frame::WindowUpdate(WindowUpdate {
        stream_id,
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
    let recv = RecvAckState::new();
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
