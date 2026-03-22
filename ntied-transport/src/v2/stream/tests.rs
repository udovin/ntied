use super::reliable::*;
use crate::v2::wire::StreamData;

#[test]
fn send_basic_write_and_poll() {
    let mut s = ReliableSendStream::new(1, 65536);
    s.write(b"hello");

    let frame = s.poll_frame(1200).unwrap();
    assert_eq!(frame.stream_id, 1);
    assert_eq!(frame.offset, 0);
    assert_eq!(frame.data, b"hello");
    assert!(!frame.fin);
    assert_eq!(s.send_offset(), 5);
    assert_eq!(s.pending_len(), 0);
}

#[test]
fn send_respects_max_data() {
    let mut s = ReliableSendStream::new(1, 65536);
    s.write(&[0u8; 100]);

    let frame = s.poll_frame(30).unwrap();
    assert_eq!(frame.data.len(), 30);
    assert_eq!(frame.offset, 0);
    assert_eq!(s.send_offset(), 30);
    assert_eq!(s.pending_len(), 70);

    let frame = s.poll_frame(30).unwrap();
    assert_eq!(frame.offset, 30);
    assert_eq!(frame.data.len(), 30);
}

#[test]
fn send_respects_flow_control() {
    let mut s = ReliableSendStream::new(1, 10);
    s.write(&[0u8; 50]);

    let frame = s.poll_frame(1200).unwrap();
    assert_eq!(frame.data.len(), 10);
    assert_eq!(s.send_window(), 0);
    assert!(!s.can_send());

    assert!(s.poll_frame(1200).is_none());

    s.on_window_update(30);
    assert_eq!(s.send_window(), 20);
    assert!(s.can_send());

    let frame = s.poll_frame(1200).unwrap();
    assert_eq!(frame.data.len(), 20);
}

#[test]
fn send_window_update_ignores_smaller() {
    let mut s = ReliableSendStream::new(1, 100);
    s.on_window_update(50);
    assert_eq!(s.send_window(), 100);
}

#[test]
fn send_fin_with_data() {
    let mut s = ReliableSendStream::new(1, 65536);
    s.write(b"bye");
    s.write_fin();

    let frame = s.poll_frame(1200).unwrap();
    assert_eq!(frame.data, b"bye");
    assert!(frame.fin);
    assert!(s.is_fin_sent());
    assert!(!s.can_send());
    assert!(s.poll_frame(1200).is_none());
}

#[test]
fn send_fin_empty() {
    let mut s = ReliableSendStream::new(1, 65536);
    s.write_fin();
    assert!(s.can_send());

    let frame = s.poll_frame(1200).unwrap();
    assert!(frame.data.is_empty());
    assert!(frame.fin);
    assert_eq!(frame.offset, 0);
    assert!(s.is_fin_sent());
}

#[test]
fn send_fin_deferred_until_pending_drained() {
    let mut s = ReliableSendStream::new(1, 65536);
    s.write(&[0u8; 100]);
    s.write_fin();

    let f1 = s.poll_frame(60).unwrap();
    assert!(!f1.fin);
    assert!(!s.is_fin_sent());

    let f2 = s.poll_frame(60).unwrap();
    assert!(f2.fin);
    assert_eq!(f2.data.len(), 40);
    assert!(s.is_fin_sent());
}

#[test]
fn send_empty_pending_no_fin_returns_none() {
    let mut s = ReliableSendStream::new(1, 65536);
    assert!(!s.can_send());
    assert!(s.poll_frame(1200).is_none());
}

#[test]
fn recv_in_order() {
    let mut r = ReliableRecvStream::new(1);

    assert_eq!(r.on_data(0, b"hello".to_vec(), false), RecvResult::Received);
    assert_eq!(
        r.on_data(5, b" world".to_vec(), false),
        RecvResult::Received
    );

    let data = r.read().unwrap();
    assert_eq!(data, b"hello world");
    assert_eq!(r.read_offset(), 11);
}

#[test]
fn recv_out_of_order() {
    let mut r = ReliableRecvStream::new(1);

    assert_eq!(r.on_data(5, b"world".to_vec(), false), RecvResult::Received);
    assert!(r.read().is_none());

    assert_eq!(r.on_data(0, b"hello".to_vec(), false), RecvResult::Received);
    let data = r.read().unwrap();
    assert_eq!(data, b"helloworld");
}

#[test]
fn recv_duplicate_fully_consumed() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(0, b"hello".to_vec(), false);
    r.read();

    assert_eq!(
        r.on_data(0, b"hello".to_vec(), false),
        RecvResult::Duplicate
    );
}

#[test]
fn recv_duplicate_in_buffer() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(0, b"hello".to_vec(), false);
    assert_eq!(r.on_data(0, b"hello".to_vec(), false), RecvResult::Received);

    let data = r.read().unwrap();
    assert_eq!(data, b"hello");
}

#[test]
fn recv_partial_overlap_trimmed() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(0, b"hello".to_vec(), false);
    r.read();
    assert_eq!(r.read_offset(), 5);

    assert_eq!(
        r.on_data(3, b"lo world".to_vec(), false),
        RecvResult::Received
    );
    let data = r.read().unwrap();
    assert_eq!(data, b" world");
    assert_eq!(r.read_offset(), 11);
}

#[test]
fn recv_overlap_in_buffer_resolved_on_read() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(5, b"67890".to_vec(), false);
    r.on_data(0, b"0123456789".to_vec(), false);

    let data = r.read().unwrap();
    assert_eq!(data, b"0123456789");
    assert_eq!(r.read_offset(), 10);
}

#[test]
fn recv_fully_overlapping_entry_skipped_on_read() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(2, b"cd".to_vec(), false);
    r.on_data(0, b"abcdef".to_vec(), false);

    let data = r.read().unwrap();
    assert_eq!(data.len(), 6);
    assert_eq!(&data[..6], b"abcdef");
    assert_eq!(r.read_offset(), 6);
}

#[test]
fn recv_fin() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(0, b"hello".to_vec(), true);
    assert!(!r.is_finished());

    r.read();
    assert!(r.is_finished());
}

#[test]
fn recv_fin_out_of_order() {
    let mut r = ReliableRecvStream::new(1);

    r.on_data(5, b"world".to_vec(), true);
    assert!(!r.is_finished());

    r.on_data(0, b"hello".to_vec(), false);
    r.read();
    assert!(r.is_finished());
}

#[test]
fn recv_empty_read() {
    let mut r = ReliableRecvStream::new(1);
    assert!(r.read().is_none());
    assert!(!r.is_finished());
}

#[test]
fn recv_fin_empty_data() {
    let mut r = ReliableRecvStream::new(1);
    r.on_data(0, vec![], true);
    assert!(r.is_finished());
}

#[test]
fn send_stream_id() {
    let s = ReliableSendStream::new(42, 0);
    assert_eq!(s.stream_id(), 42);
}

#[test]
fn recv_stream_id() {
    let r = ReliableRecvStream::new(42);
    assert_eq!(r.stream_id(), 42);
}
