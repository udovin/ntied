use super::*;
use crate::wire::{ChannelClose, ChannelOpen, ChannelReset, ChannelType, StreamData, WindowUpdate};

#[test]
fn send_basic_write_and_poll() {
    let mut s = StreamSender::new(1, 65536);
    s.write(b"hello");

    let frame = s.poll_frame(1200).unwrap();
    assert_eq!(frame.channel_id, 1);
    assert_eq!(frame.offset, 0);
    assert_eq!(frame.data, b"hello");
    assert!(!frame.fin);
    assert_eq!(s.send_offset(), 5);
    assert_eq!(s.pending_len(), 0);
}

#[test]
fn send_respects_max_data() {
    let mut s = StreamSender::new(1, 65536);
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
    let mut s = StreamSender::new(1, 10);
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
    let mut s = StreamSender::new(1, 100);
    s.on_window_update(50);
    assert_eq!(s.send_window(), 100);
}

#[test]
fn send_fin_with_data() {
    let mut s = StreamSender::new(1, 65536);
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
    let mut s = StreamSender::new(1, 65536);
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
    let mut s = StreamSender::new(1, 65536);
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
    let mut s = StreamSender::new(1, 65536);
    assert!(!s.can_send());
    assert!(s.poll_frame(1200).is_none());
}

#[test]
fn recv_in_order() {
    let mut r = StreamReceiver::new(1);

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
    let mut r = StreamReceiver::new(1);

    assert_eq!(r.on_data(5, b"world".to_vec(), false), RecvResult::Received);
    assert!(r.read().is_none());

    assert_eq!(r.on_data(0, b"hello".to_vec(), false), RecvResult::Received);
    let data = r.read().unwrap();
    assert_eq!(data, b"helloworld");
}

#[test]
fn recv_duplicate_fully_consumed() {
    let mut r = StreamReceiver::new(1);

    r.on_data(0, b"hello".to_vec(), false);
    r.read();

    assert_eq!(
        r.on_data(0, b"hello".to_vec(), false),
        RecvResult::Duplicate
    );
}

#[test]
fn recv_duplicate_in_buffer() {
    let mut r = StreamReceiver::new(1);

    r.on_data(0, b"hello".to_vec(), false);
    assert_eq!(r.on_data(0, b"hello".to_vec(), false), RecvResult::Received);

    let data = r.read().unwrap();
    assert_eq!(data, b"hello");
}

#[test]
fn recv_partial_overlap_trimmed() {
    let mut r = StreamReceiver::new(1);

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
    let mut r = StreamReceiver::new(1);

    r.on_data(5, b"67890".to_vec(), false);
    r.on_data(0, b"0123456789".to_vec(), false);

    let data = r.read().unwrap();
    assert_eq!(data, b"0123456789");
    assert_eq!(r.read_offset(), 10);
}

#[test]
fn recv_fully_overlapping_entry_skipped_on_read() {
    let mut r = StreamReceiver::new(1);

    r.on_data(2, b"cd".to_vec(), false);
    r.on_data(0, b"abcdef".to_vec(), false);

    let data = r.read().unwrap();
    assert_eq!(data.len(), 6);
    assert_eq!(&data[..6], b"abcdef");
    assert_eq!(r.read_offset(), 6);
}

#[test]
fn recv_fin() {
    let mut r = StreamReceiver::new(1);

    r.on_data(0, b"hello".to_vec(), true);
    assert!(!r.is_finished());

    r.read();
    assert!(r.is_finished());
}

#[test]
fn recv_fin_out_of_order() {
    let mut r = StreamReceiver::new(1);

    r.on_data(5, b"world".to_vec(), true);
    assert!(!r.is_finished());

    r.on_data(0, b"hello".to_vec(), false);
    r.read();
    assert!(r.is_finished());
}

#[test]
fn recv_empty_read() {
    let mut r = StreamReceiver::new(1);
    assert!(r.read().is_none());
    assert!(!r.is_finished());
}

#[test]
fn recv_fin_empty_data() {
    let mut r = StreamReceiver::new(1);
    r.on_data(0, vec![], true);
    assert!(r.is_finished());
}

#[test]
fn send_channel_id() {
    let s = StreamSender::new(42, 0);
    assert_eq!(s.channel_id(), 42);
}

#[test]
fn recv_channel_id() {
    let r = StreamReceiver::new(42);
    assert_eq!(r.channel_id(), 42);
}

#[test]
fn manager_open_initiator_ids() {
    let mut mgr = ChannelManager::new(true);
    let (id1, open1) = mgr.open(100);
    let (id2, _) = mgr.open(200);

    assert_eq!(id1, 1);
    assert_eq!(id2, 3);
    assert_eq!(open1.channel_type, ChannelType::Stream);
    assert_eq!(open1.purpose, 100);
    assert_eq!(mgr.channel_count(), 2);
}

#[test]
fn manager_open_responder_ids() {
    let mut mgr = ChannelManager::new(false);
    let (id1, _) = mgr.open(0);
    let (id2, _) = mgr.open(0);

    assert_eq!(id1, 2);
    assert_eq!(id2, 4);
}

#[test]
fn manager_accept_remote_channel() {
    let mut mgr = ChannelManager::new(true);

    let accepted = mgr.on_channel_open(ChannelOpen {
        channel_id: 2,
        channel_type: ChannelType::Stream,
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
fn manager_reject_duplicate_channel_open() {
    let mut mgr = ChannelManager::new(true);

    mgr.on_channel_open(ChannelOpen {
        channel_id: 2,
        channel_type: ChannelType::Stream,
        purpose: 1,
    });

    let dup = mgr.on_channel_open(ChannelOpen {
        channel_id: 2,
        channel_type: ChannelType::Stream,
        purpose: 1,
    });
    assert!(!dup);
    assert_eq!(mgr.channel_count(), 1);
}

#[test]
fn manager_accept_empty() {
    let mut mgr = ChannelManager::new(true);
    assert!(mgr.accept().is_none());
}

#[test]
fn manager_write_read_roundtrip() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, b"hello").unwrap();

    let frame = mgr.poll_channel_data(1200).unwrap();
    assert_eq!(frame.channel_id, id);
    assert_eq!(frame.data, b"hello");

    mgr.on_channel_data(StreamData {
        channel_id: id,
        offset: 0,
        fin: false,
        data: b"world".to_vec(),
    });

    let data = mgr.read(id).unwrap().unwrap();
    assert_eq!(data, b"world");
}

#[test]
fn manager_write_unknown_channel() {
    let mut mgr = ChannelManager::new(true);
    assert_eq!(mgr.write(999, b"x"), Err(ChannelError::UnknownChannel));
}

#[test]
fn manager_read_unknown_channel() {
    let mut mgr = ChannelManager::new(true);
    assert_eq!(mgr.read(999), Err(ChannelError::UnknownChannel));
}

#[test]
fn manager_close_channel() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);
    mgr.write(id, b"last").unwrap();

    let close_frame = mgr.close(id).unwrap();
    assert_eq!(close_frame.channel_id, id);

    assert_eq!(mgr.write(id, b"more"), Err(ChannelError::ChannelClosed));

    let frame = mgr.poll_channel_data(1200).unwrap();
    assert!(frame.fin);
    assert_eq!(frame.data, b"last");
}

#[test]
fn manager_close_unknown_channel() {
    let mut mgr = ChannelManager::new(true);
    assert_eq!(mgr.close(999), Err(ChannelError::UnknownChannel));
}

#[test]
fn manager_on_channel_reset() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_channel_reset(&ChannelReset {
        channel_id: id,
        error_code: 1,
    });

    assert_eq!(mgr.write(id, b"x"), Err(ChannelError::ChannelReset));
    assert_eq!(mgr.read(id), Err(ChannelError::ChannelReset));
}

#[test]
fn manager_on_channel_close_remote() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_channel_close(&ChannelClose { channel_id: id });

    assert_eq!(mgr.write(id, b"x"), Err(ChannelError::ChannelClosed));
}

#[test]
fn manager_on_window_update() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, &[0u8; 100_000]).unwrap();

    let f1 = mgr.poll_channel_data(100_000).unwrap();
    assert_eq!(f1.data.len(), DEFAULT_CHANNEL_WINDOW as usize);

    assert!(mgr.poll_channel_data(100_000).is_none());

    mgr.on_window_update(&WindowUpdate {
        channel_id: id,
        max_offset: DEFAULT_CHANNEL_WINDOW + 10000,
    });

    let f2 = mgr.poll_channel_data(100_000).unwrap();
    assert_eq!(f2.data.len(), 10000);
}

#[test]
fn manager_poll_no_data() {
    let mut mgr = ChannelManager::new(true);
    assert!(mgr.poll_channel_data(1200).is_none());

    let (_, _) = mgr.open(0);
    assert!(!mgr.has_pending_data());
    assert!(mgr.poll_channel_data(1200).is_none());
}

#[test]
fn manager_has_pending_data() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    assert!(!mgr.has_pending_data());
    mgr.write(id, b"x").unwrap();
    assert!(mgr.has_pending_data());

    mgr.poll_channel_data(1200);
    assert!(!mgr.has_pending_data());
}

#[test]
fn manager_is_channel_finished() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    assert!(!mgr.is_channel_finished(id));

    mgr.on_channel_data(StreamData {
        channel_id: id,
        offset: 0,
        fin: true,
        data: b"end".to_vec(),
    });

    assert!(!mgr.is_channel_finished(id));
    mgr.read(id).unwrap();
    assert!(mgr.is_channel_finished(id));
}

#[test]
fn manager_data_to_unknown_channel_ignored() {
    let mut mgr = ChannelManager::new(true);

    mgr.on_channel_data(StreamData {
        channel_id: 999,
        offset: 0,
        fin: false,
        data: b"ghost".to_vec(),
    });

    assert_eq!(mgr.channel_count(), 0);
}

#[test]
fn manager_data_to_reset_channel_ignored() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.on_channel_reset(&ChannelReset {
        channel_id: id,
        error_code: 0,
    });

    mgr.on_channel_data(StreamData {
        channel_id: id,
        offset: 0,
        fin: false,
        data: b"late".to_vec(),
    });
}

#[test]
fn manager_write_fin() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);

    mgr.write(id, b"done").unwrap();
    mgr.write_fin(id).unwrap();

    let frame = mgr.poll_channel_data(1200).unwrap();
    assert_eq!(frame.data, b"done");
    assert!(frame.fin);
}

#[test]
fn manager_poll_skips_reset_channel() {
    let mut mgr = ChannelManager::new(true);
    let (id, _) = mgr.open(0);
    mgr.write(id, b"data").unwrap();

    mgr.on_channel_reset(&ChannelReset {
        channel_id: id,
        error_code: 0,
    });

    assert!(mgr.poll_channel_data(1200).is_none());
    assert!(!mgr.has_pending_data());
}

#[test]
fn channel_error_display() {
    assert_eq!(ChannelError::UnknownChannel.to_string(), "unknown channel");
    assert_eq!(ChannelError::ChannelClosed.to_string(), "channel closed");
    assert_eq!(ChannelError::ChannelReset.to_string(), "channel reset");
}
