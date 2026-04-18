use super::manager::*;
use super::message::*;

// ---------------------------------------------------------------------------
// ChannelManager tests
// ---------------------------------------------------------------------------

#[test]
fn send_and_emit() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    let msg_id = mgr.send(0, b"hello".to_vec()).unwrap();
    assert_eq!(msg_id, 0);

    let mut out = [0u8; 100];
    let (ch, msg, off, len, fin) = mgr.emit(&mut out).unwrap();
    assert_eq!((ch, msg, off, len), (0, 0, 0, 5));
    assert!(fin);

    assert!(mgr.emit(&mut out).is_none());
}

#[test]
fn recv_and_poll() {
    // is_initiator=false → channel 0 is peer-parity, recv auto-creates it.
    let mut mgr = ChannelManager::new(65536, false, 256);
    mgr.recv(0, 0, 0, b"hello", true).unwrap();

    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"hello");

    assert!(mgr.poll(0).is_none());
}

#[test]
fn recv_fragmented() {
    let mut mgr = ChannelManager::new(65536, false, 256);
    mgr.recv(0, 0, 0, b"hel", false).unwrap();
    assert!(mgr.poll(0).is_none());

    mgr.recv(0, 0, 3, b"lo", true).unwrap();
    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"hello");
}

#[test]
fn recv_out_of_order() {
    let mut mgr = ChannelManager::new(65536, false, 256);
    mgr.recv(0, 0, 5, b"world", true).unwrap();
    mgr.recv(0, 0, 0, b"hello", false).unwrap();

    let msg = mgr.poll(0).unwrap();
    assert_eq!(msg, b"helloworld");
}

#[test]
fn multiple_messages() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    let id0 = mgr.send(0, b"first".to_vec()).unwrap();
    let id1 = mgr.send(0, b"second".to_vec()).unwrap();
    assert_ne!(id0, id1);

    let mut out = [0u8; 100];
    let mut emitted = Vec::new();
    while let Some(frag) = mgr.emit(&mut out) {
        emitted.push(frag);
    }
    assert_eq!(emitted.len(), 2);
}

#[test]
fn loss_retransmit() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"ABCDEFGHIJ".to_vec()).unwrap();

    let mut out = [0u8; 5];
    let (_, msg_id, off1, _, _) = mgr.emit(&mut out).unwrap();
    let (_, _, _, _, fin) = mgr.emit(&mut out).unwrap();
    assert!(fin);
    assert!(mgr.emit(&mut out).is_none());

    mgr.loss(0, msg_id, off1, 5);
    let (_, _, off, len, _) = mgr.emit(&mut out).unwrap();
    assert_eq!((off, len), (0, 5));
    assert_eq!(&out, b"ABCDE");
}

#[test]
fn close_send_marks_fin() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"hello".to_vec()).unwrap();
    // close_send sets send_fin but channel stays in map until both sides done.
    assert!(mgr.close_send(0));
    assert!(mgr.channels.contains_key(&0));
    // Idempotent: second close_send returns false.
    assert!(!mgr.close_send(0));
}

#[test]
fn close_send_unknown_channel() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    assert!(!mgr.close_send(99));
}

#[test]
fn recv_too_large_evicts() {
    // Buffer can hold 10 bytes total.
    let mut mgr = ChannelManager::new(10, false, 256);
    // First message: 6 bytes.
    mgr.recv(0, 0, 0, b"aaaaaa", false).unwrap();
    // Second message: 6 bytes. Total would be 12 > 10. Evicts first.
    mgr.recv(0, 1, 0, b"bbbbbb", false).unwrap();
    assert!(!mgr.channels[&0].recv.contains_key(&0));
    assert!(mgr.channels[&0].recv.contains_key(&1));
}

#[test]
fn readable_channels() {
    // is_initiator=false → 0 is peer (recv auto-creates), 1 is local (send creates).
    let mut mgr = ChannelManager::new(65536, false, 256);
    mgr.recv(0, 0, 0, b"msg", true).unwrap();
    mgr.send(1, b"out".to_vec()).unwrap();

    let readable: Vec<u64> = mgr.readable_channels().collect();
    assert!(readable.contains(&0));
    assert!(!readable.contains(&1));
}

#[test]
fn emit_empty_buf() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"hello".to_vec()).unwrap();
    assert!(mgr.emit(&mut []).is_none());
}

#[test]
fn loss_unknown_channel() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.loss(99, 0, 0, 5); // no panic
}

#[test]
fn loss_unknown_message() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"hello".to_vec()).unwrap();
    mgr.loss(0, 99, 0, 5); // no panic
}

#[test]
fn has_pending() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    assert!(!mgr.has_pending());

    mgr.send(0, b"hello".to_vec()).unwrap();
    assert!(mgr.has_pending());

    let mut out = [0u8; 100];
    mgr.emit(&mut out);
    assert!(!mgr.has_pending());
}

#[test]
fn recv_creates_channel() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(5, 0, 0, b"data", true).unwrap();
    assert!(mgr.channels.contains_key(&5));
}

#[test]
fn multiple_channels() {
    // Both 0 and 2 are local-parity for is_initiator=true.
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"ch0".to_vec()).unwrap();
    mgr.send(2, b"ch2".to_vec()).unwrap();

    let mut out = [0u8; 100];
    let mut channel_ids = Vec::new();
    while let Some((ch, _, _, _, _)) = mgr.emit(&mut out) {
        channel_ids.push(ch);
    }
    assert_eq!(channel_ids.len(), 2);
    assert!(channel_ids.contains(&0));
    assert!(channel_ids.contains(&2));
}

#[test]
fn emit_round_robin_across_channels() {
    // Each channel has a multi-fragment message. Emits with small buf should
    // alternate channels rather than draining one before the next.
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"AAAAAAAAAA".to_vec()).unwrap();
    mgr.send(2, b"BBBBBBBBBB".to_vec()).unwrap();
    mgr.send(4, b"CCCCCCCCCC".to_vec()).unwrap();

    let mut out = [0u8; 3];
    let mut order = Vec::new();
    for _ in 0..3 {
        let (ch, _, _, _, _) = mgr.emit(&mut out).unwrap();
        order.push(ch);
    }
    // Cursor starts at 0: range(0..) yields 0, 2, 4.
    assert_eq!(order, vec![0, 2, 4]);

    // Next round: cursor=5, wraps to start, yields 0, 2, 4 again.
    let mut order2 = Vec::new();
    for _ in 0..3 {
        let (ch, _, _, _, _) = mgr.emit(&mut out).unwrap();
        order2.push(ch);
    }
    assert_eq!(order2, vec![0, 2, 4]);
}

#[test]
fn manager_roundtrip() {
    // Receiver must be the responder so channel 0 is peer-parity on its side.
    let mut sender = ChannelManager::new(65536, true, 256);
    let mut receiver = ChannelManager::new(65536, false, 256);

    sender
        .send(0, b"hello world!".to_vec())
        .unwrap();

    let mut buf = [0u8; 5];
    while let Some((ch, msg, off, len, fin)) = sender.emit(&mut buf) {
        receiver.recv(ch, msg, off, &buf[..len], fin).unwrap();
    }

    let msg = receiver.poll(0).unwrap();
    assert_eq!(msg, b"hello world!");
}

#[test]
fn eviction_oldest_dropped() {
    // Buffer holds 8 bytes. 3 messages of 3 bytes each = 9 > 8.
    let mut mgr = ChannelManager::new(8, false, 256);

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
    let mut mgr = ChannelManager::new(65536, false, 256);

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
    let mut mgr = ChannelManager::new(5, false, 256);
    // Fragment with offset=5, data=5 would resize assembler to 10 > max 5.
    // Assembler's internal bound rejects the write, message is dropped.
    mgr.recv(0, 0, 5, b"world", false).unwrap();
    assert!(mgr.channels[&0].recv.is_empty());
}

#[test]
fn completed_evicted_when_full() {
    // Buffer holds 10 bytes.
    let mut mgr = ChannelManager::new(10, false, 256);
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
    let mut mgr = ChannelManager::new(10, false, 256);
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
    let mut mgr = ChannelManager::new(10, false, 256);

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
    let mut mgr = ChannelManager::new(3, true, 256);
    mgr.send(0, b"hello".to_vec()).unwrap();
    // Message exceeds limit but nothing to evict -> inserted anyway.
    assert_eq!(mgr.channels[&0].send.len(), 1);
}


#[test]
fn would_evict_check() {
    let mut mgr = ChannelManager::new(10, true, 256);
    mgr.send(0, b"aaaa".to_vec()).unwrap();
    mgr.send(0, b"bbbb".to_vec()).unwrap();
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
    let mut mgr = ChannelManager::new(10, true, 256);
    mgr.send(0, b"aaaa".to_vec()).unwrap(); // 4 bytes
    mgr.send(0, b"bbbb".to_vec()).unwrap(); // 4 bytes, total 8
    // 3rd message: 4 bytes, total would be 12 > 10. Evicts oldest.
    mgr.send(0, b"cccc".to_vec()).unwrap();
    assert!(!mgr.channels[&0].send.contains_key(&0)); // msg 0 evicted
    assert!(mgr.channels[&0].send.contains_key(&1));
    assert!(mgr.channels[&0].send.contains_key(&2));
}

#[test]
fn would_evict_as_backpressure_signal() {
    // App pattern: check would_evict before submitting — skip if network slow.
    let mut mgr = ChannelManager::new(10, true, 256);
    mgr.send(0, b"aaaa".to_vec()).unwrap();
    mgr.send(0, b"bbbb".to_vec()).unwrap();
    // Now buf at 8/10. 4 more bytes would evict.
    assert!(mgr.would_evict(0, 4));
    // App skips this send — no message inserted, no eviction.
    assert_eq!(mgr.channels[&0].send.len(), 2);
    assert!(mgr.channels[&0].send.contains_key(&0));
    // 2 bytes still fit without eviction.
    assert!(!mgr.would_evict(0, 2));
    mgr.send(0, b"cc".to_vec()).unwrap();
    assert_eq!(mgr.channels[&0].send.len(), 3);
}

// ---------------------------------------------------------------------------
// Parity, gap-fill, limits, updated, ack_close
// ---------------------------------------------------------------------------

#[test]
fn peer_gap_fill() {
    // is_initiator=true → local even, peer odd.
    let mut mgr = ChannelManager::new(65536, true, 256);
    // Receive on peer channel 5 → gap-fill 1, 3, 5.
    mgr.recv(5, 0, 0, b"data", true).unwrap();
    assert!(mgr.channels.contains_key(&1));
    assert!(mgr.channels.contains_key(&3));
    assert!(mgr.channels.contains_key(&5));
}

#[test]
fn peer_gap_fill_marks_updated() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(5, 0, 0, b"data", true).unwrap();
    let updated = mgr.drain_updated();
    assert!(updated.contains(&1));
    assert!(updated.contains(&3));
    assert!(updated.contains(&5));
}

#[test]
fn local_gap_fill_queues_channel_open() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    // Send on local channel 4 → gap-fill 0, 2, 4.
    mgr.send(4, b"data".to_vec()).unwrap();
    assert!(mgr.channels.contains_key(&0));
    assert!(mgr.channels.contains_key(&2));
    assert!(mgr.channels.contains_key(&4));
    // All gap-filled locals should have ChannelOpen queued.
    let opens = mgr.drain_pending_opens();
    assert!(opens.contains(&0));
    assert!(opens.contains(&2));
    assert!(opens.contains(&4));
}

#[test]
fn too_many_peer_channels() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    // 256 peer channels (odd 1..511).
    mgr.recv(511, 0, 0, b"x", true).unwrap();
    // 257th → TooManyChannels.
    assert_eq!(
        mgr.recv(513, 0, 0, b"x", true),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn too_many_local_channels() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(510, b"x".to_vec()).unwrap();
    assert_eq!(
        mgr.send(512, b"x".to_vec()),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn peer_id_reuse_rejected_after_full_close() {
    // Both sides finish: try_cleanup removes channel, peer_next_id stays.
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(1, 0, 0, b"data", true).unwrap();
    // Drain peer's message and signal our fin.
    let _ = mgr.poll(1).unwrap();
    mgr.on_peer_fin(1, 1); // peer says: last_message_id=1 (sent msg 0)
    mgr.close_send(1);     // we say: no more sends from us
    // Both sides finished → channel removed.
    assert!(!mgr.channels.contains_key(&1));
    // Reuse of 1 → IdReused.
    assert_eq!(
        mgr.recv(1, 0, 0, b"reuse", true),
        Err(ChannelError::IdReused)
    );
}

#[test]
fn on_peer_fin_marks_recv_finished() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(1, 0, 0, b"data", true).unwrap();
    let _ = mgr.poll(1).unwrap();
    // Peer says: last_message_id=1 (only msg 0 was sent).
    mgr.on_peer_fin(1, 1);
    // Channel still alive — our send side hasn't fin'd.
    assert!(mgr.channels.contains_key(&1));
    // Updated set should include 1.
    assert!(mgr.drain_updated().contains(&1));
}

#[test]
fn recv_rejects_local_parity_when_missing() {
    // Regression: peer must not be able to fabricate local-parity channels.
    // With is_initiator=true, channel 0 is local-parity. Peer sending recv
    // for it without us having opened it should fail.
    let mut mgr = ChannelManager::new(65536, true, 256);
    assert_eq!(
        mgr.recv(0, 0, 0, b"bogus", true),
        Err(ChannelError::UnknownChannel)
    );
    // No local channel was created.
    assert!(!mgr.channels.contains_key(&0));
}

#[test]
fn recv_accepts_local_parity_after_local_open() {
    // Once we open a local channel, peer can legitimately send data on it.
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"hi".to_vec()).unwrap();
    mgr.recv(0, 99, 0, b"from peer", true).unwrap();
}

#[test]
fn on_peer_open_rejects_local_parity() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    // Peer-base=1, so channel 0 is local. Peer sending ChannelOpen for it → reject.
    assert_eq!(
        mgr.on_peer_open(0),
        Err(ChannelError::UnknownChannel)
    );
}

#[test]
fn cleanup_of_peer_channel_grants_credit() {
    // Regression: cleaning up a peer channel must grant additional credit
    // for peer to open more.  Mirrors auto-cleanup in streams.
    let mut mgr = ChannelManager::new(65536, true, 2);
    mgr.recv(1, 0, 0, b"a", true).unwrap();
    mgr.recv(3, 0, 0, b"b", true).unwrap();
    // Both peer slots used (cumulative=2 at advertised=2).
    // 5 is rejected.
    assert_eq!(
        mgr.recv(5, 0, 0, b"c", true),
        Err(ChannelError::TooManyChannels)
    );

    // Both sides finish channel 1: we drain msg, peer fin'd at 1, we close_send.
    let _ = mgr.poll(1).unwrap();
    mgr.on_peer_fin(1, 1);
    mgr.close_send(1);
    assert!(!mgr.channels.contains_key(&1));
    // Cleanup bumped advertised → peer can open 5 now.
    mgr.recv(5, 0, 0, b"c", true).unwrap();
    assert!(mgr.channels.contains_key(&5));
}

#[test]
fn on_peer_open_existing_channel_is_noop() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(1, 0, 0, b"data", true).unwrap();
    // ChannelOpen for already-existing channel — no-op.
    mgr.on_peer_open(1).unwrap();
}

#[test]
fn on_peer_open_new_channel() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.on_peer_open(1).unwrap();
    assert!(mgr.channels.contains_key(&1));
}

#[test]
fn on_peer_open_too_many() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.on_peer_open(511).unwrap(); // 256 peer channels.
    assert_eq!(
        mgr.on_peer_open(513),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn close_send_queues_pending_fin() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"x".to_vec()).unwrap();
    mgr.close_send(0);
    let fins = mgr.drain_pending_fins();
    // last_message_id = next_message_id at close_send time = 1 (msg 0 already sent).
    assert_eq!(fins, vec![(0, 1)]);
}

#[test]
fn requeue_open_and_fin() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.requeue_open(42);
    mgr.requeue_fin(99, 5);
    assert_eq!(mgr.drain_pending_opens(), vec![42]);
    assert_eq!(mgr.drain_pending_fins(), vec![(99, 5)]);
}

#[test]
fn ack_unknown_channel_noop() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.ack(99, 0, 0, 5); // no crash
}

#[test]
fn ack_unknown_message_noop() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"x".to_vec()).unwrap();
    mgr.ack(0, 99, 0, 5); // unknown message_id — no crash
}

#[test]
fn ack_not_done_yet() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"ABCDEFGHIJ".to_vec()).unwrap();

    // Emit only half.
    let mut out = [0u8; 5];
    let (_, _, off, len, _) = mgr.emit(&mut out).unwrap();
    // Ack the first half — frag not done yet, shouldn't remove.
    mgr.ack(0, 0, off, len);
    assert!(mgr.channels[&0].send.contains_key(&0));
}

#[test]
fn ack_done_but_has_retransmits() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"ABCDE".to_vec()).unwrap();

    // Emit all.
    let mut out = [0u8; 100];
    let (_, _, off, len, _) = mgr.emit(&mut out).unwrap();

    // Mark first part as lost → creates retransmit.
    mgr.loss(0, 0, 0, 3);

    // ACK the emitted range — removes it from retransmits if overlapping,
    // but retransmit [0..3) was re-added by loss. ACK [0..5) covers it.
    mgr.ack(0, 0, off, len);
    // is_done() = true now (offset past end, retransmits cleared by ack).
    assert!(!mgr.channels[&0].send.contains_key(&0));
}

#[test]
fn ack_partial_removes_retransmit_overlap() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.send(0, b"ABCDEFGHIJ".to_vec()).unwrap();

    // Emit in two chunks.
    let mut out = [0u8; 5];
    let (_, _, off1, len1, _) = mgr.emit(&mut out).unwrap();
    let (_, _, off2, len2, _) = mgr.emit(&mut out).unwrap();

    // Both lost.
    mgr.loss(0, 0, off1, len1);
    mgr.loss(0, 0, off2, len2);

    // ACK only second chunk — first retransmit remains.
    mgr.ack(0, 0, off2, len2);
    assert!(mgr.channels[&0].send.contains_key(&0)); // still has retransmit [0..5)

    // Retransmit first chunk.
    let mut out2 = [0u8; 100];
    let result = mgr.emit(&mut out2);
    assert!(result.is_some());
    let (_, _, roff, rlen, _) = result.unwrap();
    assert_eq!((roff, rlen), (0, 5));

    // ACK the retransmit.
    mgr.ack(0, 0, roff, rlen);
    // Now is_done and no retransmits → cleaned up.
    assert!(!mgr.channels[&0].send.contains_key(&0));
}

#[test]
fn on_peer_fin_nonexistent_noop() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.on_peer_fin(99, 5); // no crash
}

// -- MAX_CHANNELS flow control / half-close ---------------------------------

#[test]
fn cumulative_credit_caps_open() {
    // With max_channels=2, can open 2 cumulative; 3rd is rejected even
    // after cleanup (no MaxChannels update yet).
    let mut mgr = ChannelManager::new(65536, true, 2);
    mgr.send(0, b"a".to_vec()).unwrap();
    mgr.send(2, b"b".to_vec()).unwrap();
    assert_eq!(
        mgr.send(4, b"c".to_vec()),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn max_channels_update_grants_credit() {
    let mut mgr = ChannelManager::new(65536, true, 2);
    mgr.send(0, b"a".to_vec()).unwrap();
    mgr.send(2, b"b".to_vec()).unwrap();
    assert_eq!(
        mgr.send(4, b"c".to_vec()),
        Err(ChannelError::TooManyChannels)
    );
    mgr.update_send_max_channels(4);
    mgr.send(4, b"c".to_vec()).unwrap();
}

#[test]
fn cleanup_of_peer_channel_advances_advertised() {
    let mut mgr = ChannelManager::new(65536, true, 2);
    mgr.recv(1, 0, 0, b"hi", true).unwrap();
    let _ = mgr.poll(1).unwrap();
    mgr.on_peer_fin(1, 1);
    mgr.close_send(1);
    // advertised: 2 → 3.  Threshold = max/2 = 1. Δ=1 → drain triggers.
    assert_eq!(mgr.drain_max_channels_update(), Some(3));
    // Already drained — second call returns None.
    assert!(mgr.drain_max_channels_update().is_none());
}

#[test]
fn requeue_max_channels_forces_resend() {
    let mut mgr = ChannelManager::new(65536, true, 2);
    mgr.recv(1, 0, 0, b"hi", true).unwrap();
    let _ = mgr.poll(1).unwrap();
    mgr.on_peer_fin(1, 1);
    mgr.close_send(1);
    let _ = mgr.drain_max_channels_update().unwrap();
    assert!(mgr.drain_max_channels_update().is_none());
    mgr.requeue_max_channels_update();
    assert_eq!(mgr.drain_max_channels_update(), Some(3));
}

#[test]
fn half_close_drains_in_flight_then_cleans_up() {
    // Both sides exchange a message and then half-close in any order.
    // Channel removed once everything drained from both sides.
    let mut mgr = ChannelManager::new(65536, true, 256);
    mgr.recv(1, 0, 0, b"from peer", true).unwrap();
    mgr.send(1, b"reply".to_vec()).unwrap();
    // Emit our send.
    let mut out = [0u8; 100];
    let (_, _, off, len, _) = mgr.emit(&mut out).unwrap();
    mgr.ack(1, 0, off, len);
    // Both sides half-close.
    mgr.on_peer_fin(1, 1);
    mgr.close_send(1);
    // Drain peer's data.
    let _ = mgr.poll(1).unwrap();
    // Channel removed.
    assert!(!mgr.channels.contains_key(&1));
}

#[test]
fn on_peer_fin_prunes_above_boundary() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    // Receive partial message id=2 (not complete).
    mgr.recv(1, 2, 0, b"part", false).unwrap();
    // Peer says they sent only msgs [0, 1] (last_message_id=2).
    mgr.on_peer_fin(1, 2);
    // Assembler for msg 2 should be pruned (peer won't send it).
    assert!(!mgr.channels[&1].recv.contains_key(&2));
}

#[test]
fn close_send_nonexistent_returns_false() {
    let mut mgr = ChannelManager::new(65536, true, 256);
    assert!(!mgr.close_send(99));
}

#[test]
fn recv_assembler_error_propagated() {
    let mut mgr = ChannelManager::new(65536, false, 256);
    // First recv sets fin_off = 5.
    mgr.recv(0, 0, 0, b"hello", true).unwrap();
    // Second recv on same message with different fin_off → FinalSizeMismatch.
    let result = mgr.recv(0, 0, 0, b"helloworld", true);
    assert!(matches!(result, Err(ChannelError::AssemblerError(_))));
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

// Helper: drain all retransmits via emit() and return the (start, end) ranges.
fn drain_retransmits(f: &mut MessageFragmenter) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    while f.has_retransmits() {
        let (off, n, _) = f.emit(&mut buf).unwrap();
        out.push((off, off + n as u64));
    }
    out
}

fn fragmenter_emitted(data: &[u8]) -> MessageFragmenter {
    let mut f = MessageFragmenter::new(data.to_vec());
    let mut tmp = [0u8; 1024];
    while f.emit(&mut tmp).is_some() {}
    f
}

#[test]
fn fragmenter_ack_zero_len_noop() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 5);
    f.ack(2, 0);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 5)]);
}

#[test]
fn fragmenter_ack_no_retransmits_noop() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.ack(0, 5); // empty retransmits — early return
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_range_before_retransmits() {
    // ACK ends at/before the earliest retransmit (range(..ack_end) empty).
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(5, 5); // [5, 10)
    f.ack(0, 3);
    assert_eq!(drain_retransmits(&mut f), vec![(5, 10)]);
}

#[test]
fn fragmenter_ack_range_after_retransmits() {
    // ACK begins past the only retransmit's end (filter `re > offset` false).
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 3); // [0, 3)
    f.ack(5, 5);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_touches_retransmit_end() {
    // re == offset boundary case (no overlap).
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 3); // [0, 3)
    f.ack(3, 3);  // [3, 6) — touches but no overlap
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_exact_match_removes() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(2, 5); // [2, 7)
    f.ack(2, 5);  // exact match — fully removed
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_contains_retransmit() {
    // ACK fully contains retransmit (rs >= offset, re <= ack_end).
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(3, 3); // [3, 6)
    f.ack(0, 10); // [0, 10) — contains [3, 6)
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_leaves_prefix() {
    // rs < offset, re <= ack_end — first inner branch true, second false.
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 5); // [0, 5)
    f.ack(3, 3);  // [3, 6) — leaves [0, 3)
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_leaves_suffix() {
    // rs >= offset, re > ack_end — first inner branch false, second true.
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(3, 5); // [3, 8)
    f.ack(2, 4);  // [2, 6) — leaves [6, 8)
    assert_eq!(drain_retransmits(&mut f), vec![(6, 8)]);
}

#[test]
fn fragmenter_ack_splits_retransmit() {
    // rs < offset && re > ack_end — both inner branches true.
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 10); // [0, 10)
    f.ack(3, 4);   // [3, 7) — splits into [0, 3) and [7, 10)
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3), (7, 10)]);
}

#[test]
fn fragmenter_ack_multiple_retransmits() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJKLMNOPQRST");
    f.loss(0, 3);  // [0, 3)
    f.loss(5, 3);  // [5, 8)
    f.loss(10, 3); // [10, 13)
    // ACK [2, 12) — overlaps all three.
    // [0,3): rs=0<offset, re=3<=ack_end → leaves [0, 2)
    // [5,8): rs>=offset, re<=ack_end → removed
    // [10,13): rs>=offset, re>ack_end → leaves [12, 13)
    f.ack(2, 10);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 2), (12, 13)]);
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
