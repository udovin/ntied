use super::manager::*;
use super::reliable::*;
use crate::v2::wire::{StreamClose, StreamData, StreamOpen, StreamReset, StreamType, WindowUpdate};

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

#[test]
fn manager_open_initiator_ids() {
    let mut mgr = StreamManager::new(true);
    let (id1, open1) = mgr.open(100);
    let (id2, _) = mgr.open(200);

    assert_eq!(id1, 1);
    assert_eq!(id2, 3);
    assert_eq!(open1.stream_type, StreamType::ReliableOrdered);
    assert_eq!(open1.purpose, 100);
    assert_eq!(mgr.stream_count(), 2);
}

#[test]
fn manager_open_responder_ids() {
    let mut mgr = StreamManager::new(false);
    let (id1, _) = mgr.open(0);
    let (id2, _) = mgr.open(0);

    assert_eq!(id1, 2);
    assert_eq!(id2, 4);
}

#[test]
fn manager_accept_remote_stream() {
    let mut mgr = StreamManager::new(true);

    let accepted = mgr.on_stream_open(StreamOpen {
        stream_id: 2,
        stream_type: StreamType::ReliableOrdered,
        purpose: 42,
    });
    assert!(accepted);
    assert_eq!(mgr.pending_accept_count(), 1);

    let (id, purpose) = mgr.accept().unwrap();
    assert_eq!(id, 2);
    assert_eq!(purpose, 42);
    assert_eq!(mgr.pending_accept_count(), 0);
}

#[test]
fn manager_reject_duplicate_stream_open() {
    let mut mgr = StreamManager::new(true);

    mgr.on_stream_open(StreamOpen {
        stream_id: 2,
        stream_type: StreamType::ReliableOrdered,
        purpose: 1,
    });

    let dup = mgr.on_stream_open(StreamOpen {
        stream_id: 2,
        stream_type: StreamType::ReliableOrdered,
        purpose: 1,
    });
    assert!(!dup);
    assert_eq!(mgr.stream_count(), 1);
}

#[test]
fn manager_accept_empty() {
    let mut mgr = StreamManager::new(true);
    assert!(mgr.accept().is_none());
}

#[test]
fn manager_write_read_roundtrip() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, b"hello").unwrap();

    let frame = mgr.poll_stream_data(1200).unwrap();
    assert_eq!(frame.stream_id, id);
    assert_eq!(frame.data, b"hello");

    mgr.on_stream_data(StreamData {
        stream_id: id,
        offset: 0,
        fin: false,
        data: b"world".to_vec(),
    });

    let data = mgr.read(id).unwrap().unwrap();
    assert_eq!(data, b"world");
}

#[test]
fn manager_write_unknown_stream() {
    let mut mgr = StreamManager::new(true);
    assert_eq!(mgr.write(999, b"x"), Err(StreamError::UnknownStream));
}

#[test]
fn manager_read_unknown_stream() {
    let mut mgr = StreamManager::new(true);
    assert_eq!(mgr.read(999), Err(StreamError::UnknownStream));
}

#[test]
fn manager_close_stream() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);
    mgr.write(id, b"last").unwrap();

    let close_frame = mgr.close(id).unwrap();
    assert_eq!(close_frame.stream_id, id);

    assert_eq!(mgr.write(id, b"more"), Err(StreamError::StreamClosed));

    let frame = mgr.poll_stream_data(1200).unwrap();
    assert!(frame.fin);
    assert_eq!(frame.data, b"last");
}

#[test]
fn manager_close_unknown_stream() {
    let mut mgr = StreamManager::new(true);
    assert_eq!(mgr.close(999), Err(StreamError::UnknownStream));
}

#[test]
fn manager_on_stream_reset() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_stream_reset(&StreamReset {
        stream_id: id,
        error_code: 1,
    });

    assert_eq!(mgr.write(id, b"x"), Err(StreamError::StreamReset));
    assert_eq!(mgr.read(id), Err(StreamError::StreamReset));
}

#[test]
fn manager_on_stream_close_remote() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_stream_close(&StreamClose { stream_id: id });

    assert_eq!(mgr.write(id, b"x"), Err(StreamError::StreamClosed));
}

#[test]
fn manager_on_window_update() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, &[0u8; 100_000]).unwrap();

    let f1 = mgr.poll_stream_data(100_000).unwrap();
    assert_eq!(f1.data.len(), DEFAULT_STREAM_WINDOW as usize);

    assert!(mgr.poll_stream_data(100_000).is_none());

    mgr.on_window_update(&WindowUpdate {
        stream_id: id,
        max_offset: DEFAULT_STREAM_WINDOW + 10000,
    });

    let f2 = mgr.poll_stream_data(100_000).unwrap();
    assert_eq!(f2.data.len(), 10000);
}

#[test]
fn manager_poll_no_data() {
    let mut mgr = StreamManager::new(true);
    assert!(mgr.poll_stream_data(1200).is_none());

    let (_, _) = mgr.open(0);
    assert!(!mgr.has_pending_data());
    assert!(mgr.poll_stream_data(1200).is_none());
}

#[test]
fn manager_has_pending_data() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    assert!(!mgr.has_pending_data());
    mgr.write(id, b"x").unwrap();
    assert!(mgr.has_pending_data());

    mgr.poll_stream_data(1200);
    assert!(!mgr.has_pending_data());
}

#[test]
fn manager_is_stream_finished() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    assert!(!mgr.is_stream_finished(id));

    mgr.on_stream_data(StreamData {
        stream_id: id,
        offset: 0,
        fin: true,
        data: b"end".to_vec(),
    });

    assert!(!mgr.is_stream_finished(id));
    mgr.read(id).unwrap();
    assert!(mgr.is_stream_finished(id));
}

#[test]
fn manager_data_to_unknown_stream_ignored() {
    let mut mgr = StreamManager::new(true);

    mgr.on_stream_data(StreamData {
        stream_id: 999,
        offset: 0,
        fin: false,
        data: b"ghost".to_vec(),
    });

    assert_eq!(mgr.stream_count(), 0);
}

#[test]
fn manager_data_to_reset_stream_ignored() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_stream_reset(&StreamReset {
        stream_id: id,
        error_code: 0,
    });

    mgr.on_stream_data(StreamData {
        stream_id: id,
        offset: 0,
        fin: false,
        data: b"late".to_vec(),
    });
}

#[test]
fn manager_write_fin() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, b"done").unwrap();
    mgr.write_fin(id).unwrap();

    let frame = mgr.poll_stream_data(1200).unwrap();
    assert_eq!(frame.data, b"done");
    assert!(frame.fin);
}

#[test]
fn manager_poll_skips_reset_stream() {
    let mut mgr = StreamManager::new(true);
    let (id, _) = mgr.open(0);
    mgr.write(id, b"data").unwrap();

    mgr.on_stream_reset(&StreamReset {
        stream_id: id,
        error_code: 0,
    });

    assert!(mgr.poll_stream_data(1200).is_none());
    assert!(!mgr.has_pending_data());
}

#[test]
fn stream_error_display() {
    assert_eq!(StreamError::UnknownStream.to_string(), "unknown stream");
    assert_eq!(StreamError::StreamClosed.to_string(), "stream closed");
    assert_eq!(StreamError::StreamReset.to_string(), "stream reset");
}
