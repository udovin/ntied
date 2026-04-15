use super::manager::*;
use super::message::*;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// ChannelManager tests
// ---------------------------------------------------------------------------

fn now() -> Instant {
    Instant::now()
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

#[test]
fn send_and_emit() {
    let mut mgr = ChannelManager::new(65536, true);
    let msg_id = mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    assert_eq!(msg_id, 0);

    let mut out = [0u8; 100];
    let (ch, msg, off, len, fin) = mgr.emit(&mut out, now()).unwrap();
    assert_eq!((ch, msg, off, len), (0, 0, 0, 5));
    assert!(fin);

    assert!(mgr.emit(&mut out, now()).is_none());
}

#[test]
fn recv_and_poll() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.recv(0, 0, 0, b"hello", true).unwrap();

    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"hello");

    assert!(mgr.poll(0).is_none());
}

#[test]
fn recv_fragmented() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.recv(0, 0, 0, b"hel", false).unwrap();
    assert!(mgr.poll(0).is_none());

    mgr.recv(0, 0, 3, b"lo", true).unwrap();
    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"hello");
}

#[test]
fn recv_out_of_order() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.recv(0, 0, 5, b"world", true).unwrap();
    mgr.recv(0, 0, 0, b"hello", false).unwrap();

    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"helloworld");
}

#[test]
fn multiple_messages() {
    let mut mgr = ChannelManager::new(65536, true);
    let id0 = mgr.send(0, b"first".to_vec(), far_future()).unwrap();
    let id1 = mgr.send(0, b"second".to_vec(), far_future()).unwrap();
    assert_ne!(id0, id1);

    let mut out = [0u8; 100];
    let mut emitted = Vec::new();
    while let Some(frag) = mgr.emit(&mut out, now()) {
        emitted.push(frag);
    }
    assert_eq!(emitted.len(), 2);
}

#[test]
fn loss_retransmit() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.send(0, b"ABCDEFGHIJ".to_vec(), far_future()).unwrap();

    let mut out = [0u8; 5];
    let (_, msg_id, off1, _, _) = mgr.emit(&mut out, now()).unwrap();
    let (_, _, _, _, fin) = mgr.emit(&mut out, now()).unwrap();
    assert!(fin);
    assert!(mgr.emit(&mut out, now()).is_none());

    mgr.loss(0, msg_id, off1, 5);
    let (_, _, off, len, _) = mgr.emit(&mut out, now()).unwrap();
    assert_eq!((off, len), (0, 5));
    assert_eq!(&out, b"ABCDE");
}

#[test]
fn close_channel() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    mgr.recv(0, 99, 0, b"incoming", false).unwrap();

    assert!(mgr.close(0));
    assert!(!mgr.channels.contains_key(&0));

    assert!(!mgr.close(0));
}

#[test]
fn close_reuse_rejected() {
    let mut mgr = ChannelManager::new(65536, true);
    // Create local channel 0 (even, local_base=0).
    mgr.send(0, b"hi".to_vec(), far_future()).unwrap();
    mgr.close(0);

    // Channel 0 was removed but local_next_id is 2 → IdReused.
    assert_eq!(
        mgr.send(0, b"hi".to_vec(), far_future()),
        Err(ChannelError::IdReused)
    );
}

#[test]
fn recv_too_large_evicts() {
    // Buffer can hold 10 bytes total.
    let mut mgr = ChannelManager::new(10, true);
    // First message: 6 bytes.
    mgr.recv(0, 0, 0, b"aaaaaa", false).unwrap();
    // Second message: 6 bytes. Total would be 12 > 10. Evicts first.
    mgr.recv(0, 1, 0, b"bbbbbb", false).unwrap();
    assert!(!mgr.channels[&0].recv.contains_key(&0));
    assert!(mgr.channels[&0].recv.contains_key(&1));
}

#[test]
fn readable_channels() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.recv(0, 0, 0, b"msg", true).unwrap();
    mgr.send(1, b"out".to_vec(), far_future()).unwrap();

    let readable: Vec<u64> = mgr.readable_channels().collect();
    assert!(readable.contains(&0));
    assert!(!readable.contains(&1));
}

#[test]
fn emit_empty_buf() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    assert!(mgr.emit(&mut [], now()).is_none());
}

#[test]
fn loss_unknown_channel() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.loss(99, 0, 0, 5); // no panic
}

#[test]
fn loss_unknown_message() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    mgr.loss(0, 99, 0, 5); // no panic
}

#[test]
fn has_pending() {
    let mut mgr = ChannelManager::new(65536, true);
    assert!(!mgr.has_pending());

    mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    assert!(mgr.has_pending());

    let mut out = [0u8; 100];
    mgr.emit(&mut out, now());
    assert!(!mgr.has_pending());
}

#[test]
fn recv_creates_channel() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.recv(5, 0, 0, b"data", true).unwrap();
    assert!(mgr.channels.contains_key(&5));
}

#[test]
fn multiple_channels() {
    let mut mgr = ChannelManager::new(65536, true);
    mgr.send(0, b"ch0".to_vec(), far_future()).unwrap();
    mgr.send(1, b"ch1".to_vec(), far_future()).unwrap();

    let mut out = [0u8; 100];
    let mut channel_ids = Vec::new();
    while let Some((ch, _, _, _, _)) = mgr.emit(&mut out, now()) {
        channel_ids.push(ch);
    }
    assert_eq!(channel_ids.len(), 2);
    assert!(channel_ids.contains(&0));
    assert!(channel_ids.contains(&1));
}

#[test]
fn manager_roundtrip() {
    let mut sender = ChannelManager::new(65536, true);
    let mut receiver = ChannelManager::new(65536, true);

    sender
        .send(0, b"hello world!".to_vec(), far_future())
        .unwrap();

    let mut buf = [0u8; 5];
    while let Some((ch, msg, off, len, fin)) = sender.emit(&mut buf, now()) {
        receiver.recv(ch, msg, off, &buf[..len], fin).unwrap();
    }

    let msg = receiver.poll(0).unwrap();
    assert_eq!(msg, b"hello world!");
}

#[test]
fn eviction_oldest_dropped() {
    // Buffer holds 8 bytes. 3 messages of 3 bytes each = 9 > 8.
    let mut mgr = ChannelManager::new(8, true);

    mgr.recv(0, 10, 0, b"aaa", false).unwrap();
    mgr.recv(0, 20, 0, b"bbb", false).unwrap();
    assert_eq!(mgr.channels[&0].recv.len(), 2);

    // 3rd message needs 3 bytes. Total would be 9 > 8. Evicts oldest (10).
    mgr.recv(0, 30, 0, b"ccc", false).unwrap();
    assert!(!mgr.channels[&0].recv.contains_key(&10));
    assert!(mgr.channels[&0].recv.contains_key(&20));
    assert!(mgr.channels[&0].recv.contains_key(&30));
}

#[test]
fn eviction_does_not_affect_existing_message() {
    let mut mgr = ChannelManager::new(65536, true);

    mgr.recv(0, 10, 0, b"aaa", false).unwrap();
    mgr.recv(0, 20, 0, b"bbb", false).unwrap();

    // Writing to existing message_id=10 completes it.
    mgr.recv(0, 10, 3, b"ddd", true).unwrap();
    // Both still in recv (10 completed but not polled, 20 incomplete).
    assert_eq!(mgr.channels[&0].recv.len(), 2);
    let msg = mgr.poll(0).unwrap();
    assert_eq!(&msg[..6], b"aaaddd");
    // Now 10 removed by poll.
    assert_eq!(mgr.channels[&0].recv.len(), 1);
}

#[test]
fn eviction_current_message_can_be_dropped() {
    // Buffer holds 5 bytes. One message of 10 bytes exceeds limit.
    let mut mgr = ChannelManager::new(5, true);
    // First fragment grows assembler to 10 bytes (offset=5, data=5 -> resize to 10).
    mgr.recv(0, 0, 5, b"world", false).unwrap();
    // Assembler is 10 bytes > max 5 -> evicted (including this message).
    assert!(mgr.channels[&0].recv.is_empty());
}

#[test]
fn completed_evicted_when_full() {
    // Buffer holds 10 bytes.
    let mut mgr = ChannelManager::new(10, true);

    // Complete a 6-byte message (stays in completed, counts in budget).
    mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
    assert!(mgr.poll(0).is_none() == false); // read it? no, leave it
    // Oops, poll consumed it. Let's redo without polling.

    let mut mgr = ChannelManager::new(10, true);
    mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
    // Don't poll -- completed has 6 bytes in budget.

    // New message: 6 bytes. Total would be 12 > 10.
    // No assemblers to evict -> evicts oldest completed.
    mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();

    // First completed message was dropped.
    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"bbbbbb");
    assert!(mgr.poll(0).is_none());
}

#[test]
fn poll_frees_budget() {
    let mut mgr = ChannelManager::new(10, true);
    mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();

    // Poll frees budget.
    let _ = mgr.poll(0).unwrap();

    // Now 6 more bytes fit without eviction.
    mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();
    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"bbbbbb");
}

#[test]
fn completed_evicted_before_poll() {
    let mut mgr = ChannelManager::new(10, true);

    mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
    // Don't poll -- msg 0 in completed + recv.

    // New 6-byte msg exceeds budget -> evicts msg 0 from recv AND completed.
    mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();

    // Msg 0 was evicted. Only msg 1 available.
    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"bbbbbb");
    assert!(mgr.poll(0).is_none());
}

#[test]
fn send_eviction_empties_map() {
    // Buffer holds 3 bytes. Message is 5 bytes -- eviction empties map, still inserts.
    let mut mgr = ChannelManager::new(3, true);
    mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
    // Message exceeds limit but nothing to evict -> inserted anyway.
    assert_eq!(mgr.channels[&0].send.len(), 1);
}

#[test]
fn ttl_expiration() {
    let mut mgr = ChannelManager::new(65536, true);
    let past = Instant::now(); // deadline in the past
    mgr.send(0, b"expired".to_vec(), past).unwrap();

    // Sleep to ensure now > deadline.
    std::thread::sleep(Duration::from_millis(1));

    let mut out = [0u8; 100];
    // emit expires the message -- returns None.
    assert!(mgr.emit(&mut out, Instant::now()).is_none());
    assert!(!mgr.has_pending());
}

#[test]
fn would_evict_check() {
    let mut mgr = ChannelManager::new(10, true);
    mgr.send(0, b"aaaa".to_vec(), far_future()).unwrap();
    mgr.send(0, b"bbbb".to_vec(), far_future()).unwrap();
    // 8 bytes used. 4 more would be 12 > 10 -> eviction.
    assert!(mgr.would_evict(0, 4));
    // 2 more would be 10 = 10 -> no eviction.
    assert!(!mgr.would_evict(0, 2));
    // Unknown channel -> no eviction (will be created).
    assert!(!mgr.would_evict(99, 100));
}

#[test]
fn send_eviction() {
    // Buffer holds 10 bytes.
    let mut mgr = ChannelManager::new(10, true);
    mgr.send(0, b"aaaa".to_vec(), far_future()).unwrap(); // 4 bytes
    mgr.send(0, b"bbbb".to_vec(), far_future()).unwrap(); // 4 bytes, total 8
    // 3rd message: 4 bytes, total would be 12 > 10. Evicts oldest.
    mgr.send(0, b"cccc".to_vec(), far_future()).unwrap();
    assert!(!mgr.channels[&0].send.contains_key(&0)); // msg 0 evicted
    assert!(mgr.channels[&0].send.contains_key(&1));
    assert!(mgr.channels[&0].send.contains_key(&2));
}

// ---------------------------------------------------------------------------
// MessageAssembler / MessageFragmenter tests
// ---------------------------------------------------------------------------

#[test]
fn assembler_in_order() {
    let mut a = MessageAssembler::new(1024);
    assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
    assert_eq!(a.write(5, b"world", true).unwrap(), 5);
    assert!(a.is_complete());
    assert_eq!(a.take(), b"helloworld");
}

#[test]
fn assembler_out_of_order() {
    let mut a = MessageAssembler::new(1024);
    assert_eq!(a.write(5, b"world", true).unwrap(), 5);
    assert!(!a.is_complete());
    assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
    assert!(a.is_complete());
    assert_eq!(a.take(), b"helloworld");
}

#[test]
fn assembler_duplicate() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hello", true).unwrap();
    assert_eq!(a.write(0, b"hello", true).unwrap(), 0);
}

#[test]
fn assembler_overlap() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"helloworld", true).unwrap();
    assert_eq!(a.write(3, b"loworl", false).unwrap(), 0);
}

#[test]
fn assembler_too_large() {
    let mut a = MessageAssembler::new(5);
    assert_eq!(
        a.write(0, b"toolarge!", true),
        Err(AssemblerError::TooLarge)
    );
}

#[test]
fn assembler_too_large_no_fin() {
    let mut a = MessageAssembler::new(5);
    assert_eq!(
        a.write(0, b"toolarge!", false),
        Err(AssemblerError::TooLarge)
    );
}

#[test]
fn assembler_fin_mismatch() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hello", true).unwrap();
    assert_eq!(
        a.write(0, b"helloworld", true),
        Err(AssemblerError::FinalSizeMismatch)
    );
}

#[test]
fn assembler_data_past_fin() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hello", true).unwrap(); // fin_off = 5
    assert_eq!(
        a.write(3, b"loworld", false),
        Err(AssemblerError::FinalSizeMismatch)
    );
}

#[test]
fn assembler_empty_write() {
    let mut a = MessageAssembler::new(1024);
    assert_eq!(a.write(0, b"", false).unwrap(), 0);
}

#[test]
fn assembler_empty_fin() {
    let mut a = MessageAssembler::new(1024);
    assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
    assert_eq!(a.write(5, b"", true).unwrap(), 0);
    assert!(a.is_complete());
    assert_eq!(a.take(), b"hello");
}

#[test]
fn assembler_bridges_ranges() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"AB", false).unwrap();
    a.write(5, b"FG", false).unwrap();
    assert_eq!(a.write(1, b"BCDEF", false).unwrap(), 3);
    assert_eq!(a.received.len(), 1);
}

#[test]
fn assembler_non_adjacent_prev() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"AB", false).unwrap();
    a.write(5, b"FG", false).unwrap();
    assert_eq!(a.received.len(), 2);
}

#[test]
fn assembler_fin_off() {
    let mut a = MessageAssembler::new(1024);
    assert_eq!(a.fin_off(), None);
    a.write(0, b"hi", true).unwrap();
    assert_eq!(a.fin_off(), Some(2));
}

#[test]
fn assembler_take_incomplete_with_fin() {
    let mut a = MessageAssembler::new(1024);
    a.write(5, b"world", true).unwrap();
    assert!(!a.is_complete());
    let data = a.take();
    assert_eq!(data.len(), 10); // truncated to fin_off
}

#[test]
fn assembler_incomplete_partial_from_zero() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hel", false).unwrap();
    a.write(5, b"world", true).unwrap(); // fin_off = 10
    // received = {0:3, 5:10}. First range starts at 0 but ends at 3 < 10.
    assert!(!a.is_complete());
}

#[test]
fn assembler_take_without_fin() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"partial", false).unwrap();
    assert!(!a.is_complete());
    let data = a.take();
    assert_eq!(data.len(), 7); // no truncation, raw buffer size
}

#[test]
fn assembler_reset() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hello", true).unwrap();
    a.reset();
    assert!(!a.is_complete());
    assert_eq!(a.fin_off(), None);
}

#[test]
fn assembler_grows_buffer() {
    let mut a = MessageAssembler::new(1024);
    // First fragment at offset 100 -- buffer grows to 105.
    a.write(100, b"hello", false).unwrap();
    assert!(a.data.len() >= 105);
    // Then fill start.
    a.write(0, &vec![0u8; 100], false).unwrap();
    a.write(105, b"!", true).unwrap();
    assert!(a.is_complete());
}

#[test]
fn fragmenter_basic() {
    let mut f = MessageFragmenter::new(b"helloworld".to_vec());
    assert_eq!(f.len(), 10);

    let mut out = [0u8; 5];
    let (off, n, fin) = f.emit(&mut out).unwrap();
    assert_eq!((off, n), (0, 5));
    assert!(!fin);
    assert_eq!(&out, b"hello");

    let (off, n, fin) = f.emit(&mut out).unwrap();
    assert_eq!((off, n), (5, 5));
    assert!(fin);
    assert_eq!(&out, b"world");

    assert!(f.emit(&mut out).is_none());
    assert!(f.is_done());
}

#[test]
fn fragmenter_small_chunks() {
    let mut f = MessageFragmenter::new(b"abcdefgh".to_vec());
    let mut out = [0u8; 3];
    let mut fragments = Vec::new();
    while let Some((off, n, fin)) = f.emit(&mut out) {
        fragments.push((off, n, fin));
    }
    assert_eq!(fragments.len(), 3);
    assert!(!fragments[0].2); // not fin
    assert!(!fragments[1].2); // not fin
    assert!(fragments[2].2);  // fin on last
}

#[test]
fn fragmenter_retransmit() {
    let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
    let mut tmp = [0u8; 5];
    while f.emit(&mut tmp).is_some() {}
    assert!(f.is_done());

    f.loss(0, 5);
    assert!(!f.is_done());

    let mut out = [0u8; 5];
    let (off, n, fin) = f.emit(&mut out).unwrap();
    assert_eq!((off, n), (0, 5));
    assert!(!fin); // not last -- [5..10) already sent
    assert_eq!(&out, b"ABCDE");
    assert!(f.is_done());
}

#[test]
fn fragmenter_retransmit_last_carries_fin() {
    let mut f = MessageFragmenter::new(b"ABCDE".to_vec());
    let mut tmp = [0u8; 5];
    f.emit(&mut tmp); // emit all
    assert!(f.is_done());

    f.loss(0, 5);
    let (_, _, fin) = f.emit(&mut tmp).unwrap();
    assert!(fin); // retransmit of entire message carries fin
}

#[test]
fn fragmenter_partial_retransmit() {
    let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
    let mut tmp = [0u8; 10];
    while f.emit(&mut tmp).is_some() {}

    f.loss(0, 10);

    let mut small = [0u8; 3];
    let (off, n, _) = f.emit(&mut small).unwrap();
    assert_eq!((off, n), (0, 3));

    let mut rest = [0u8; 10];
    let (off, n, fin) = f.emit(&mut rest).unwrap();
    assert_eq!((off, n), (3, 7));
    assert!(fin);
}

#[test]
fn fragmenter_loss_bridges() {
    let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
    let mut tmp = [0u8; 10];
    while f.emit(&mut tmp).is_some() {}

    f.loss(0, 3);
    f.loss(5, 3);
    f.loss(2, 5);

    let mut out = [0u8; 10];
    let (off, n, _) = f.emit(&mut out).unwrap();
    assert_eq!((off, n), (0, 8));
}

#[test]
fn fragmenter_loss_past_end() {
    let mut f = MessageFragmenter::new(b"hello".to_vec());
    let mut tmp = [0u8; 5];
    while f.emit(&mut tmp).is_some() {}
    f.loss(10, 5);
    assert!(f.is_done());
}

#[test]
fn fragmenter_loss_clamped() {
    let mut f = MessageFragmenter::new(b"hello".to_vec());
    let mut tmp = [0u8; 5];
    while f.emit(&mut tmp).is_some() {}
    f.loss(3, 100);

    let mut out = [0u8; 10];
    let (off, n, fin) = f.emit(&mut out).unwrap();
    assert_eq!((off, n), (3, 2));
    assert!(fin);
}

#[test]
fn fragmenter_loss_zero_len() {
    let mut f = MessageFragmenter::new(b"hello".to_vec());
    let mut tmp = [0u8; 5];
    while f.emit(&mut tmp).is_some() {}
    f.loss(0, 0);
    assert!(f.is_done());
}

#[test]
fn fragmenter_emit_empty_buf() {
    let mut f = MessageFragmenter::new(b"hello".to_vec());
    assert!(f.emit(&mut []).is_none());
}

#[test]
fn message_roundtrip() {
    let msg = b"The quick brown fox jumps over the lazy dog".to_vec();
    let mut frag = MessageFragmenter::new(msg.clone());
    let mut asm = MessageAssembler::new(1024);

    let mut buf = [0u8; 10];
    while let Some((off, n, fin)) = frag.emit(&mut buf) {
        asm.write(off, &buf[..n], fin).unwrap();
    }
    assert!(asm.is_complete());
    assert_eq!(asm.take(), msg);
}
