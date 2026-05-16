use super::manager::*;
use super::message::*;

// Convenience defaults: large buf cap, generous window so flow control
// doesn't trip up unrelated assertions.  Tests that exercise flow control
// pick smaller values explicitly.
const BIG_BUF: u64 = 1 << 20;
const BIG_WND: u64 = 1 << 20;

fn mgr(is_initiator: bool) -> ChannelManager {
    ChannelManager::new(BIG_BUF, BIG_WND, is_initiator, 256)
}

// ---------------------------------------------------------------------------
// ChannelManager: basic send / recv / emit
// ---------------------------------------------------------------------------

#[test]
fn send_and_emit() {
    let mut m = mgr(true);
    let msg_id = m.send(0, b"hello".to_vec(), true).unwrap();
    assert_eq!(msg_id, 0);

    let mut out = [0u8; 100];
    let (ch, msg, off, len, fin) = m.emit(&mut out).unwrap();
    assert_eq!((ch, msg, off, len), (0, 0, 0, 5));
    assert!(fin);

    assert!(m.emit(&mut out).is_none());
}

#[test]
fn recv_and_poll() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"hello", true).unwrap();

    let msg = m.poll(0).unwrap();
    assert_eq!(msg, b"hello");

    assert!(m.poll(0).is_none());
}

#[test]
fn recv_fragmented() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"hel", false).unwrap();
    assert!(m.poll(0).is_none());

    m.recv(0, 0, 3, b"lo", true).unwrap();
    let msg = m.poll(0).unwrap();
    assert_eq!(msg, b"hello");
}

#[test]
fn recv_out_of_order() {
    let mut m = mgr(false);
    m.recv(0, 0, 5, b"world", true).unwrap();
    m.recv(0, 0, 0, b"hello", false).unwrap();

    let msg = m.poll(0).unwrap();
    assert_eq!(msg, b"helloworld");
}

#[test]
fn multiple_messages() {
    let mut m = mgr(true);
    let id0 = m.send(0, b"first".to_vec(), true).unwrap();
    let id1 = m.send(0, b"second".to_vec(), true).unwrap();
    assert_ne!(id0, id1);

    let mut out = [0u8; 100];
    let mut emitted = Vec::new();
    while let Some(frag) = m.emit(&mut out) {
        emitted.push(frag);
    }
    assert_eq!(emitted.len(), 2);
}

#[test]
fn loss_retransmit() {
    let mut m = mgr(true);
    m.send(0, b"ABCDEFGHIJ".to_vec(), true).unwrap();

    let mut out = [0u8; 5];
    let (_, msg_id, off1, _, _) = m.emit(&mut out).unwrap();
    let (_, _, _, _, fin) = m.emit(&mut out).unwrap();
    assert!(fin);
    assert!(m.emit(&mut out).is_none());

    m.loss(0, msg_id, off1, 5);
    let (_, _, off, len, _) = m.emit(&mut out).unwrap();
    assert_eq!((off, len), (0, 5));
    assert_eq!(&out, b"ABCDE");
}

// ---------------------------------------------------------------------------
// Half-close
// ---------------------------------------------------------------------------

#[test]
fn close_send_marks_fin() {
    let mut m = mgr(true);
    m.send(0, b"hello".to_vec(), true).unwrap();
    assert!(m.close_send(0));
    assert!(m.channels.contains_key(&0));
    assert!(!m.close_send(0));
}

#[test]
fn close_send_unknown_channel() {
    let mut m = mgr(true);
    assert!(!m.close_send(99));
}

#[test]
fn close_send_queues_pending_fin() {
    let mut m = mgr(true);
    m.send(0, b"x".to_vec(), true).unwrap();
    m.close_send(0);
    let mut fins = Vec::new();
    m.drain_pending_fins(&mut fins);
    assert_eq!(fins, vec![(0, 1)]);
}

#[test]
fn close_send_nonexistent_returns_false() {
    let mut m = mgr(true);
    assert!(!m.close_send(99));
}

#[test]
fn half_close_drains_in_flight_then_cleans_up() {
    let mut m = mgr(true);
    m.recv(1, 0, 0, b"from peer", true).unwrap();
    m.send(1, b"reply".to_vec(), true).unwrap();
    let mut out = [0u8; 100];
    let (_, _, off, len, _) = m.emit(&mut out).unwrap();
    m.ack(1, 0, off, len);
    m.on_peer_fin(1, 1).unwrap();
    m.close_send(1);
    let _ = m.poll(1).unwrap();
    assert!(!m.channels.contains_key(&1));
}

// ---------------------------------------------------------------------------
// Readable / writable queries
// ---------------------------------------------------------------------------

#[test]
fn readable_channels() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"msg", true).unwrap();
    m.send(1, b"out".to_vec(), true).unwrap();

    let readable: Vec<u64> = m.readable_channels().collect();
    assert!(readable.contains(&0));
    assert!(!readable.contains(&1));
}

#[test]
fn emit_empty_buf() {
    let mut m = mgr(true);
    m.send(0, b"hello".to_vec(), true).unwrap();
    assert!(m.emit(&mut []).is_none());
}

#[test]
fn loss_unknown_channel() {
    let mut m = mgr(true);
    m.loss(99, 0, 0, 5);
}

#[test]
fn loss_unknown_message() {
    let mut m = mgr(true);
    m.send(0, b"hello".to_vec(), true).unwrap();
    m.loss(0, 99, 0, 5);
}

#[test]
fn has_pending() {
    let mut m = mgr(true);
    assert!(!m.has_pending());

    m.send(0, b"hello".to_vec(), true).unwrap();
    assert!(m.has_pending());

    let mut out = [0u8; 100];
    m.emit(&mut out);
    assert!(!m.has_pending());
}

#[test]
fn recv_creates_channel() {
    let mut m = mgr(true);
    m.recv(5, 0, 0, b"data", true).unwrap();
    assert!(m.channels.contains_key(&5));
}

#[test]
fn multiple_channels() {
    let mut m = mgr(true);
    m.send(0, b"ch0".to_vec(), true).unwrap();
    m.send(2, b"ch2".to_vec(), true).unwrap();

    let mut out = [0u8; 100];
    let mut channel_ids = Vec::new();
    while let Some((ch, _, _, _, _)) = m.emit(&mut out) {
        channel_ids.push(ch);
    }
    assert_eq!(channel_ids.len(), 2);
    assert!(channel_ids.contains(&0));
    assert!(channel_ids.contains(&2));
}

#[test]
fn emit_round_robin_across_channels() {
    let mut m = mgr(true);
    m.send(0, b"AAAAAAAAAA".to_vec(), true).unwrap();
    m.send(2, b"BBBBBBBBBB".to_vec(), true).unwrap();
    m.send(4, b"CCCCCCCCCC".to_vec(), true).unwrap();

    let mut out = [0u8; 3];
    let mut order = Vec::new();
    for _ in 0..3 {
        let (ch, _, _, _, _) = m.emit(&mut out).unwrap();
        order.push(ch);
    }
    assert_eq!(order, vec![0, 2, 4]);

    let mut order2 = Vec::new();
    for _ in 0..3 {
        let (ch, _, _, _, _) = m.emit(&mut out).unwrap();
        order2.push(ch);
    }
    assert_eq!(order2, vec![0, 2, 4]);
}

#[test]
fn manager_roundtrip() {
    let mut sender = mgr(true);
    let mut receiver = mgr(false);

    sender.send(0, b"hello world!".to_vec(), true).unwrap();

    let mut buf = [0u8; 5];
    while let Some((ch, msg, off, len, fin)) = sender.emit(&mut buf) {
        receiver.recv(ch, msg, off, &buf[..len], fin).unwrap();
    }

    let msg = receiver.poll(0).unwrap();
    assert_eq!(msg, b"hello world!");
}

// ---------------------------------------------------------------------------
// Parity, gap-fill, MAX_CHANNELS
// ---------------------------------------------------------------------------

#[test]
fn peer_gap_fill() {
    let mut m = mgr(true);
    m.recv(5, 0, 0, b"data", true).unwrap();
    assert!(m.channels.contains_key(&1));
    assert!(m.channels.contains_key(&3));
    assert!(m.channels.contains_key(&5));
}

#[test]
fn peer_gap_fill_marks_updated() {
    let mut m = mgr(true);
    m.recv(5, 0, 0, b"data", true).unwrap();
    let mut updated = Vec::new();
    m.drain_updated(&mut updated);
    assert!(updated.contains(&1));
    assert!(updated.contains(&3));
    assert!(updated.contains(&5));
}

#[test]
fn local_gap_fill_queues_channel_open() {
    let mut m = mgr(true);
    m.send(4, b"data".to_vec(), true).unwrap();
    assert!(m.channels.contains_key(&0));
    assert!(m.channels.contains_key(&2));
    assert!(m.channels.contains_key(&4));
    let mut opens = Vec::new();
    m.drain_pending_opens(&mut opens);
    assert!(opens.contains(&0));
    assert!(opens.contains(&2));
    assert!(opens.contains(&4));
}

#[test]
fn too_many_peer_channels() {
    let mut m = mgr(true);
    m.recv(511, 0, 0, b"x", true).unwrap();
    assert_eq!(
        m.recv(513, 0, 0, b"x", true),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn too_many_local_channels() {
    let mut m = mgr(true);
    m.send(510, b"x".to_vec(), true).unwrap();
    assert_eq!(
        m.send(512, b"x".to_vec(), true),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn peer_id_reuse_rejected_after_full_close() {
    let mut m = mgr(true);
    m.recv(1, 0, 0, b"data", true).unwrap();
    let _ = m.poll(1).unwrap();
    m.on_peer_fin(1, 1).unwrap();
    m.close_send(1);
    assert!(!m.channels.contains_key(&1));
    assert_eq!(
        m.recv(1, 0, 0, b"reuse", true),
        Err(ChannelError::IdReused)
    );
}

#[test]
fn on_peer_fin_marks_recv_finished() {
    let mut m = mgr(true);
    m.recv(1, 0, 0, b"data", true).unwrap();
    let _ = m.poll(1).unwrap();
    m.on_peer_fin(1, 1).unwrap();
    assert!(m.channels.contains_key(&1));
    assert!({
        let mut t = Vec::new();
        m.drain_updated(&mut t);
        t
    }
    .contains(&1));
}

#[test]
fn recv_rejects_local_parity_when_missing() {
    let mut m = mgr(true);
    assert_eq!(
        m.recv(0, 0, 0, b"bogus", true),
        Err(ChannelError::UnknownChannel)
    );
    assert!(!m.channels.contains_key(&0));
}

#[test]
fn recv_accepts_local_parity_after_local_open() {
    let mut m = mgr(true);
    m.send(0, b"hi".to_vec(), true).unwrap();
    m.recv(0, 99, 0, b"from peer", true).unwrap();
}

#[test]
fn on_peer_open_rejects_local_parity() {
    let mut m = mgr(true);
    assert_eq!(m.on_peer_open(0), Err(ChannelError::UnknownChannel));
}

#[test]
fn cleanup_of_peer_channel_grants_credit() {
    let mut m = ChannelManager::new(BIG_BUF, BIG_WND, true, 2);
    m.recv(1, 0, 0, b"a", true).unwrap();
    m.recv(3, 0, 0, b"b", true).unwrap();
    assert_eq!(
        m.recv(5, 0, 0, b"c", true),
        Err(ChannelError::TooManyChannels)
    );

    let _ = m.poll(1).unwrap();
    m.on_peer_fin(1, 1).unwrap();
    m.close_send(1);
    assert!(!m.channels.contains_key(&1));
    m.recv(5, 0, 0, b"c", true).unwrap();
    assert!(m.channels.contains_key(&5));
}

#[test]
fn on_peer_open_existing_channel_is_noop() {
    let mut m = mgr(true);
    m.recv(1, 0, 0, b"data", true).unwrap();
    m.on_peer_open(1).unwrap();
}

#[test]
fn on_peer_open_new_channel() {
    let mut m = mgr(true);
    m.on_peer_open(1).unwrap();
    assert!(m.channels.contains_key(&1));
}

#[test]
fn on_peer_open_too_many() {
    let mut m = mgr(true);
    m.on_peer_open(511).unwrap();
    assert_eq!(m.on_peer_open(513), Err(ChannelError::TooManyChannels));
}

#[test]
fn requeue_open_and_fin() {
    let mut m = mgr(true);
    m.requeue_open(42);
    m.requeue_fin(99, 5);
    {
        let mut t = Vec::new();
        m.drain_pending_opens(&mut t);
        assert_eq!(t, vec![42]);
    }
    {
        let mut t = Vec::new();
        m.drain_pending_fins(&mut t);
        assert_eq!(t, vec![(99, 5)]);
    }
}

// ---------------------------------------------------------------------------
// Ack/loss bookkeeping
// ---------------------------------------------------------------------------

#[test]
fn ack_unknown_channel_noop() {
    let mut m = mgr(true);
    m.ack(99, 0, 0, 5);
}

#[test]
fn ack_unknown_message_noop() {
    let mut m = mgr(true);
    m.send(0, b"x".to_vec(), true).unwrap();
    m.ack(0, 99, 0, 5);
}

#[test]
fn ack_not_done_yet() {
    let mut m = mgr(true);
    m.send(0, b"ABCDEFGHIJ".to_vec(), true).unwrap();
    let mut out = [0u8; 5];
    let (_, _, off, len, _) = m.emit(&mut out).unwrap();
    m.ack(0, 0, off, len);
    assert!(m.channels[&0].send.contains_key(&0));
}

#[test]
fn ack_done_but_has_retransmits() {
    let mut m = mgr(true);
    m.send(0, b"ABCDE".to_vec(), true).unwrap();
    let mut out = [0u8; 100];
    let (_, _, off, len, _) = m.emit(&mut out).unwrap();
    m.loss(0, 0, 0, 3);
    m.ack(0, 0, off, len);
    assert!(!m.channels[&0].send.contains_key(&0));
}

#[test]
fn ack_partial_removes_retransmit_overlap() {
    let mut m = mgr(true);
    m.send(0, b"ABCDEFGHIJ".to_vec(), true).unwrap();
    let mut out = [0u8; 5];
    let (_, _, off1, len1, _) = m.emit(&mut out).unwrap();
    let (_, _, off2, len2, _) = m.emit(&mut out).unwrap();
    m.loss(0, 0, off1, len1);
    m.loss(0, 0, off2, len2);
    m.ack(0, 0, off2, len2);
    assert!(m.channels[&0].send.contains_key(&0));

    let mut out2 = [0u8; 100];
    let (_, _, roff, rlen, _) = m.emit(&mut out2).unwrap();
    assert_eq!((roff, rlen), (0, 5));
    m.ack(0, 0, roff, rlen);
    assert!(!m.channels[&0].send.contains_key(&0));
}

#[test]
fn on_peer_fin_nonexistent_noop() {
    let mut m = mgr(true);
    m.on_peer_fin(99, 5).unwrap();
}

// ---------------------------------------------------------------------------
// MAX_CHANNELS flow control
// ---------------------------------------------------------------------------

#[test]
fn cumulative_credit_caps_open() {
    let mut m = ChannelManager::new(BIG_BUF, BIG_WND, true, 2);
    m.send(0, b"a".to_vec(), true).unwrap();
    m.send(2, b"b".to_vec(), true).unwrap();
    assert_eq!(
        m.send(4, b"c".to_vec(), true),
        Err(ChannelError::TooManyChannels)
    );
}

#[test]
fn max_channels_update_grants_credit() {
    let mut m = ChannelManager::new(BIG_BUF, BIG_WND, true, 2);
    m.send(0, b"a".to_vec(), true).unwrap();
    m.send(2, b"b".to_vec(), true).unwrap();
    assert_eq!(
        m.send(4, b"c".to_vec(), true),
        Err(ChannelError::TooManyChannels)
    );
    m.update_send_max_channels(4);
    m.send(4, b"c".to_vec(), true).unwrap();
}

#[test]
fn cleanup_of_peer_channel_advances_advertised() {
    let mut m = ChannelManager::new(BIG_BUF, BIG_WND, true, 2);
    m.recv(1, 0, 0, b"hi", true).unwrap();
    let _ = m.poll(1).unwrap();
    m.on_peer_fin(1, 1).unwrap();
    m.close_send(1);
    assert_eq!(m.drain_max_channels_update(), Some(3));
    assert!(m.drain_max_channels_update().is_none());
}

#[test]
fn requeue_max_channels_forces_resend() {
    let mut m = ChannelManager::new(BIG_BUF, BIG_WND, true, 2);
    m.recv(1, 0, 0, b"hi", true).unwrap();
    let _ = m.poll(1).unwrap();
    m.on_peer_fin(1, 1).unwrap();
    m.close_send(1);
    let _ = m.drain_max_channels_update().unwrap();
    assert!(m.drain_max_channels_update().is_none());
    m.requeue_max_channels_update();
    assert_eq!(m.drain_max_channels_update(), Some(3));
}

#[test]
fn on_peer_fin_prunes_above_boundary_is_violation() {
    // Peer cannot claim "no msg >= 2" after we've already seen a fragment
    // for msg=2 — the protocol forbids it.
    let mut m = mgr(true);
    m.recv(1, 2, 0, b"part", false).unwrap();
    assert_eq!(
        m.on_peer_fin(1, 2),
        Err(ChannelError::ProtocolViolation)
    );
}

#[test]
fn recv_above_peer_fin_is_violation() {
    let mut m = mgr(true);
    // Need channel to exist before on_peer_fin can record the boundary.
    m.on_peer_open(1).unwrap();
    m.on_peer_fin(1, 5).unwrap();
    assert_eq!(
        m.recv(1, 5, 0, b"oops", true),
        Err(ChannelError::ProtocolViolation)
    );
}

#[test]
fn recv_assembler_error_propagated() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"hello", true).unwrap();
    let result = m.recv(0, 0, 0, b"helloworld", true);
    assert!(matches!(result, Err(ChannelError::AssemblerError(_))));
}

// ---------------------------------------------------------------------------
// Flow control: WouldBlock on local buffer cap
// ---------------------------------------------------------------------------

#[test]
fn ack_frees_send_buffer() {
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    m.send(0, b"aaaa".to_vec(), true).unwrap();
    m.send(0, b"bbbb".to_vec(), true).unwrap();
    // Emit and ack first message.
    let mut out = [0u8; 100];
    let (_, mid, off, len, _) = m.emit(&mut out).unwrap();
    assert_eq!(mid, 0);
    m.ack(0, 0, off, len);
    // 4 bytes freed → next 4-byte send fits.
    m.send(0, b"cccc".to_vec(), true).unwrap();
}

// ---------------------------------------------------------------------------
// Flow control: per-channel byte window (data_sent vs peer_max_data)
// ---------------------------------------------------------------------------

#[test]
fn emit_respects_window() {
    // Initial window = 5 bytes.
    let mut m = ChannelManager::new(BIG_BUF, 5, true, 256);
    m.send(0, b"ABCDEFGHIJ".to_vec(), true).unwrap();

    let mut out = [0u8; 100];
    // First emit produces 5 bytes (window cap).
    let (_, _, off, len, fin) = m.emit(&mut out).unwrap();
    assert_eq!((off, len), (0, 5));
    assert!(!fin);
    // Next emit blocked — window exhausted.
    assert!(m.emit(&mut out).is_none());
    // Peer grants more window.
    m.on_peer_max_data(0, 10);
    // Now we can emit the remaining 5 bytes (with fin).
    let (_, _, off, len, fin) = m.emit(&mut out).unwrap();
    assert_eq!((off, len), (0 + 5, 5));
    assert!(fin);
}

#[test]
fn retransmit_bypasses_window() {
    // Window=5, message=5 bytes.
    let mut m = ChannelManager::new(BIG_BUF, 5, true, 256);
    m.send(0, b"ABCDE".to_vec(), true).unwrap();
    let mut out = [0u8; 100];
    let (_, _, off, len, _) = m.emit(&mut out).unwrap();
    // Window is now exhausted.
    assert!(m.emit(&mut out).is_none());
    // Loss: retransmit must go through even though window is at the limit.
    m.loss(0, 0, off, len);
    let (_, _, roff, rlen, _) = m.emit(&mut out).unwrap();
    assert_eq!((roff, rlen), (0, 5));
}

#[test]
fn peer_max_data_is_monotonic() {
    let mut m = ChannelManager::new(BIG_BUF, 100, true, 256);
    m.on_peer_max_data(0, 50); // smaller — ignored
    m.send(0, vec![0u8; 100], true).unwrap();
    let mut out = [0u8; 200];
    // Initial window is 100 — full message fits.
    let (_, _, _, len, fin) = m.emit(&mut out).unwrap();
    assert_eq!(len, 100);
    assert!(fin);
}

// ---------------------------------------------------------------------------
// Receiver flow control: data_received tracking, MaxData advertising
// ---------------------------------------------------------------------------

#[test]
fn poll_releases_window_budget() {
    let mut m = ChannelManager::new(BIG_BUF, 1000, false, 256);
    m.recv(0, 0, 0, &vec![0u8; 600], true).unwrap();
    // Before poll: data_received = 600, released_total = 0.
    assert_eq!(m.channels[&0].data_received, 600);
    assert_eq!(m.channels[&0].released_total, 0);
    let _ = m.poll(0).unwrap();
    // After poll: 600 bytes released.
    assert_eq!(m.channels[&0].released_total, 600);
}

#[test]
fn max_data_update_fires_after_half_window_release() {
    let mut m = ChannelManager::new(BIG_BUF, 1000, false, 256);
    m.recv(0, 0, 0, &vec![0u8; 600], true).unwrap();
    let _ = m.poll(0).unwrap();
    let mut out = Vec::new();
    m.drain_max_data_updates(&mut out);
    // initial=1000, released=600, current=1600.  Half-window threshold=500
    // (1000/2).  Delta = 600 > 500 → fires.
    assert_eq!(out, vec![(0, 1600)]);
    // Subsequent drain: nothing pending.
    let mut out2 = Vec::new();
    m.drain_max_data_updates(&mut out2);
    assert!(out2.is_empty());
}

#[test]
fn max_data_update_skips_below_threshold() {
    let mut m = ChannelManager::new(BIG_BUF, 1000, false, 256);
    m.recv(0, 0, 0, &vec![0u8; 100], true).unwrap();
    let _ = m.poll(0).unwrap();
    let mut out = Vec::new();
    m.drain_max_data_updates(&mut out);
    // delta=100 < threshold=500.
    assert!(out.is_empty());
}

#[test]
fn requeue_max_data_forces_resend() {
    let mut m = ChannelManager::new(BIG_BUF, 1000, false, 256);
    m.recv(0, 0, 0, &vec![0u8; 600], true).unwrap();
    let _ = m.poll(0).unwrap();
    let mut out = Vec::new();
    m.drain_max_data_updates(&mut out);
    assert_eq!(out.len(), 1);
    let mut out2 = Vec::new();
    m.drain_max_data_updates(&mut out2);
    assert!(out2.is_empty());
    m.requeue_max_data_update(0);
    let mut out3 = Vec::new();
    m.drain_max_data_updates(&mut out3);
    assert_eq!(out3, vec![(0, 1600)]);
}

#[test]
fn receiver_rejects_overrun_of_advertised_window() {
    // Window = 5, peer sends 6 bytes -> protocol violation.
    let mut m = ChannelManager::new(BIG_BUF, 5, false, 256);
    let result = m.recv(0, 0, 0, b"abcdef", true);
    assert_eq!(result, Err(ChannelError::ProtocolViolation));
}

// ---------------------------------------------------------------------------
// Runtime resize of send_buf_cap and recv_buf_cap
// ---------------------------------------------------------------------------

#[test]
fn set_send_buf_cap_grow_admits_more() {
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    m.send(0, b"aaaaa".to_vec(), true).unwrap(); // 5/10 used
    m.send(0, b"bbbbb".to_vec(), true).unwrap(); // 10/10 used
    assert_eq!(
        m.send(0, b"c".to_vec(), true),
        Err(ChannelError::WouldBlock)
    );
    m.set_send_buf_cap(0, 20);
    m.send(0, b"ccccc".to_vec(), true).unwrap(); // now 15/20
}

#[test]
fn set_send_buf_cap_shrink_keeps_existing_blocks_new() {
    let mut m = ChannelManager::new(20, BIG_WND, true, 256);
    m.send(0, b"aaaaaaaaaa".to_vec(), true).unwrap(); // 10/20 used
    m.set_send_buf_cap(0, 5);
    // Already-queued message stays.
    assert_eq!(m.channels[&0].send.len(), 1);
    // New send blocked until existing drains.
    assert_eq!(
        m.send(0, b"b".to_vec(), true),
        Err(ChannelError::WouldBlock)
    );
}

#[test]
fn set_send_buf_cap_unknown_channel_returns_false() {
    let mut m = mgr(true);
    assert!(!m.set_send_buf_cap(99, 100));
}

#[test]
fn set_recv_buf_cap_grow_triggers_max_data_update() {
    let mut m = ChannelManager::new(BIG_BUF, 100, false, 256);
    // Force channel creation.
    m.recv(0, 0, 0, b"x", true).unwrap();
    let _ = m.poll(0).unwrap();
    let mut updates = Vec::new();
    m.drain_max_data_updates(&mut updates);
    // Released 1 byte; threshold = 50, no update yet.
    assert!(updates.is_empty());

    // Grow recv window from 100 → 1000.
    m.set_recv_buf_cap(0, 1000);
    let mut updates = Vec::new();
    m.drain_max_data_updates(&mut updates);
    // current_max_data = 1000 + 1 = 1001; sent_max_data was 100; delta = 901 ≥ 500.
    assert_eq!(updates, vec![(0, 1001)]);
}

#[test]
fn set_recv_buf_cap_shrink_does_not_revoke_credit() {
    let mut m = ChannelManager::new(BIG_BUF, 1000, false, 256);
    m.recv(0, 0, 0, b"x", true).unwrap();
    let _ = m.poll(0).unwrap();
    // Released 1; current_max_data = 1001.
    let mut updates = Vec::new();
    m.drain_max_data_updates(&mut updates);
    // delta = 1; threshold = 500; no update.
    assert!(updates.is_empty());

    // Shrink: cap = 10.  current_max_data formula would give 10+1=11,
    // but it's clamped to sent_max_data=1000 so we don't revoke.
    m.set_recv_buf_cap(0, 10);
    assert_eq!(m.channels[&0].sent_max_data, 1000);

    // No update emitted on shrink.
    let mut updates = Vec::new();
    m.drain_max_data_updates(&mut updates);
    assert!(updates.is_empty());

    // Further releases beyond the shrunken cap don't push max_data past
    // sent_max_data either — credit growth is paused until released_total
    // catches up.
    m.recv(0, 1, 0, &vec![0u8; 50], true).unwrap();
    let _ = m.poll(0).unwrap();
    // current_max_data = max(10 + 51, 1000) = 1000.  Still no update.
    let mut updates = Vec::new();
    m.drain_max_data_updates(&mut updates);
    assert!(updates.is_empty());
}

#[test]
fn set_recv_buf_cap_unknown_channel_returns_false() {
    let mut m = mgr(true);
    assert!(!m.set_recv_buf_cap(99, 100));
}

// ---------------------------------------------------------------------------
// Auto-eviction: send_unreliable on a full buffer evicts oldest unreliable
// ---------------------------------------------------------------------------

#[test]
fn auto_evict_oldest_unreliable_to_make_room() {
    // Cap = 10 bytes. Two unreliable messages of 4 bytes each (total 8).
    // A third 4-byte unreliable doesn't fit — oldest is evicted.
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    let mid0 = m.send(0, b"aaaa".to_vec(), false).unwrap();
    let mid1 = m.send(0, b"bbbb".to_vec(), false).unwrap();
    let mid2 = m.send(0, b"cccc".to_vec(), false).unwrap();
    // Oldest evicted: mid0 gone, mid1+mid2 remain.
    assert!(!m.channels[&0].send.contains_key(&mid0));
    assert!(m.channels[&0].send.contains_key(&mid1));
    assert!(m.channels[&0].send.contains_key(&mid2));
    // ChannelEvict for mid0 queued.
    let mut evs = Vec::new();
    m.drain_pending_evicts(&mut evs);
    assert_eq!(evs, vec![(0, mid0, 0)]);
}

#[test]
fn auto_evict_preserves_reliable() {
    // Reliable messages are never auto-evicted.
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    let mid_rel = m.send(0, b"aaaa".to_vec(), true).unwrap();
    let mid_unrel = m.send(0, b"bbbb".to_vec(), false).unwrap();
    // 3rd 4-byte send: evicts the unreliable, not the reliable.
    m.send(0, b"cccc".to_vec(), false).unwrap();
    assert!(m.channels[&0].send.contains_key(&mid_rel));
    assert!(!m.channels[&0].send.contains_key(&mid_unrel));
}

#[test]
fn send_returns_would_block_when_full_of_reliable() {
    // Buffer full of reliable, no unreliable to evict → WouldBlock.
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    m.send(0, b"aaaa".to_vec(), true).unwrap();
    m.send(0, b"bbbb".to_vec(), true).unwrap();
    assert_eq!(
        m.send(0, b"ccccc".to_vec(), true),
        Err(ChannelError::WouldBlock)
    );
    assert_eq!(
        m.send(0, b"ccccc".to_vec(), false),
        Err(ChannelError::WouldBlock)
    );
}

#[test]
fn auto_evict_records_partial_emit_size() {
    // Emit some bytes from an unreliable message, then evict via pressure.
    // The ChannelEvict frame's size must reflect max_offset_emitted.
    let mut m = ChannelManager::new(20, BIG_WND, true, 256);
    let mid0 = m.send(0, b"ABCDEFGHIJ".to_vec(), false).unwrap(); // 10 bytes
    // Emit 4 bytes.
    let mut out = [0u8; 4];
    let (_, _, _, n, _) = m.emit(&mut out).unwrap();
    assert_eq!(n, 4);
    // Fill the rest of the buffer and force eviction of mid0.
    m.send(0, vec![b'x'; 10].clone(), false).unwrap();
    m.send(0, vec![b'y'; 5].clone(), false).unwrap();
    // mid0 evicted; ChannelEvict carries size=4 (what was emitted).
    let mut evs = Vec::new();
    m.drain_pending_evicts(&mut evs);
    assert_eq!(evs, vec![(0, mid0, 4)]);
}

#[test]
fn requeue_evict_round_trips() {
    let mut m = ChannelManager::new(10, BIG_WND, true, 256);
    let mid = m.send(0, b"aaaa".to_vec(), false).unwrap();
    m.send(0, b"bbbb".to_vec(), false).unwrap();
    m.send(0, b"cccc".to_vec(), false).unwrap(); // evicts mid (oldest)
    let mut evs = Vec::new();
    m.drain_pending_evicts(&mut evs);
    assert_eq!(evs.len(), 1);
    // Carrier lost → requeue.
    m.requeue_evict(0, mid, 0);
    let mut evs2 = Vec::new();
    m.drain_pending_evicts(&mut evs2);
    assert_eq!(evs2, vec![(0, mid, 0)]);
}

// ---------------------------------------------------------------------------
// on_peer_evict (receiver-side handling of ChannelEvict frame)
// ---------------------------------------------------------------------------

#[test]
fn on_peer_evict_drops_assembling_releases_size() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"abc", false).unwrap();
    assert_eq!(m.channels[&0].data_received, 3);
    m.on_peer_evict(0, 0, 5).unwrap(); // sender says final_size=5
    assert!(!m.channels[&0].recv.contains_key(&0));
    assert_eq!(m.channels[&0].released_total, 5);
    // Tombstone with counted=3 (what we'd received).
}

#[test]
fn on_peer_evict_drops_ready_messages_from_delivery_queue() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"done", true).unwrap();
    assert!(!m.channels[&0].delivery_queue.is_empty());
    m.on_peer_evict(0, 0, 4).unwrap();
    assert!(m.channels[&0].delivery_queue.is_empty());
    assert!(!m.channels[&0].recv.contains_key(&0));
    assert_eq!(m.channels[&0].released_total, 4);
}

#[test]
fn on_peer_evict_before_any_fragment_gap_fills() {
    // Peer-parity channel: evict gap-fills like recv() does.
    let mut m = mgr(false);
    m.on_peer_evict(0, 5, 100).unwrap();
    assert!(m.channels.contains_key(&0));
    // Tombstone created for msg=5 with final_size=100.
    let ch = &m.channels[&0];
    assert!(ch.tombstones.contains_key(&5));
    assert_eq!(ch.released_total, 100);
}

#[test]
fn on_peer_evict_local_parity_without_open_is_unknown_channel() {
    let mut m = mgr(true);
    // Channel 0 is local-parity for is_initiator=true.  Peer cannot evict
    // on a channel we never opened.
    assert_eq!(
        m.on_peer_evict(0, 0, 100),
        Err(ChannelError::UnknownChannel)
    );
}

#[test]
fn on_peer_evict_with_size_below_received_is_violation() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"abcde", false).unwrap(); // received 5 bytes
    assert_eq!(
        m.on_peer_evict(0, 0, 3), // claims size=3 < 5
        Err(ChannelError::ProtocolViolation)
    );
}

#[test]
fn on_peer_evict_creates_tombstone_for_unseen_id() {
    let mut m = mgr(false);
    // Channel must exist first.
    m.recv(0, 0, 0, b"setup", true).unwrap();
    let _ = m.poll(0).unwrap();
    // Now evict an unseen id.
    m.on_peer_evict(0, 1, 50).unwrap();
    // Tombstone exists for id=1.
    let ch = &m.channels[&0];
    assert!(ch.tombstones.contains_key(&1) || ch.tombstone_watermark > 1);
    assert!(ch.released_total >= 50);
}

#[test]
fn on_peer_evict_idempotent_for_terminal_id() {
    let mut m = mgr(false);
    m.recv(0, 0, 0, b"abc", true).unwrap();
    let _ = m.poll(0).unwrap();
    let before = m.channels[&0].released_total;
    // Already delivered — evict is no-op.
    m.on_peer_evict(0, 0, 3).unwrap();
    assert_eq!(m.channels[&0].released_total, before);
}

#[test]
fn late_fragment_after_evict_counted_via_tombstone() {
    // Receiver sees evict first (frags reordered).  Tombstone counted=0.
    // Late frag arrives → advance counted, account for delta in data_received.
    let mut m = mgr(false);
    m.recv(0, 99, 0, b"keepalive", true).unwrap();
    let _ = m.poll(0).unwrap();

    // Evict msg=100 (size=5) before any frag.
    m.on_peer_evict(0, 100, 5).unwrap();
    let received_before = m.channels[&0].data_received;
    // Late frag at offset 0, len 3.
    m.recv(0, 100, 0, b"abc", false).unwrap();
    assert_eq!(m.channels[&0].data_received, received_before + 3);
    // Another late frag at offset 0, len 5 (covers more).
    m.recv(0, 100, 0, b"abcde", true).unwrap();
    assert_eq!(m.channels[&0].data_received, received_before + 5);
    // Duplicate — no change.
    m.recv(0, 100, 0, b"abcde", true).unwrap();
    assert_eq!(m.channels[&0].data_received, received_before + 5);
}

#[test]
fn late_fragment_past_evict_size_is_violation() {
    let mut m = mgr(false);
    m.recv(0, 99, 0, b"keepalive", true).unwrap();
    let _ = m.poll(0).unwrap();
    m.on_peer_evict(0, 100, 5).unwrap();
    // Frag claims offset+len = 6 > final_size=5.
    assert_eq!(
        m.recv(0, 100, 0, b"abcdef", true),
        Err(ChannelError::ProtocolViolation)
    );
}

#[test]
fn tombstone_watermark_advances_contiguously() {
    let mut m = mgr(false);
    // Drive ids 0, 1, 2 through delivery in order.
    for i in 0..3 {
        m.recv(0, i, 0, b"x", true).unwrap();
        let _ = m.poll(0).unwrap();
    }
    let ch = &m.channels[&0];
    assert_eq!(ch.tombstone_watermark, 3);
    assert!(ch.tombstones.is_empty());
}

#[test]
fn tombstone_keeps_noncontiguous_terminals() {
    let mut m = mgr(false);
    // Setup channel.
    m.recv(0, 0, 0, b"x", true).unwrap();
    let _ = m.poll(0).unwrap();
    // Skip msg=1, terminate msg=2.
    m.on_peer_evict(0, 2, 0).unwrap();
    let ch = &m.channels[&0];
    assert_eq!(ch.tombstone_watermark, 1);
    assert!(ch.tombstones.contains_key(&2));
    assert!(!ch.tombstones.contains_key(&1));
}

// ---------------------------------------------------------------------------
// MessageAssembler / MessageFragmenter (kept verbatim from prior tests)
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
    a.write(0, b"hello", true).unwrap();
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
    assert_eq!(data.len(), 10);
}

#[test]
fn assembler_incomplete_partial_from_zero() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"hel", false).unwrap();
    a.write(5, b"world", true).unwrap();
    assert!(!a.is_complete());
}

#[test]
fn assembler_take_without_fin() {
    let mut a = MessageAssembler::new(1024);
    a.write(0, b"partial", false).unwrap();
    assert!(!a.is_complete());
    let data = a.take();
    assert_eq!(data.len(), 7);
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
    a.write(100, b"hello", false).unwrap();
    assert!(a.data.len() >= 105);
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
    assert!(!fragments[0].2);
    assert!(!fragments[1].2);
    assert!(fragments[2].2);
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
    assert!(!fin);
    assert_eq!(&out, b"ABCDE");
    assert!(f.is_done());
}

#[test]
fn fragmenter_retransmit_last_carries_fin() {
    let mut f = MessageFragmenter::new(b"ABCDE".to_vec());
    let mut tmp = [0u8; 5];
    f.emit(&mut tmp);
    assert!(f.is_done());

    f.loss(0, 5);
    let (_, _, fin) = f.emit(&mut tmp).unwrap();
    assert!(fin);
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
    f.ack(0, 5);
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_range_before_retransmits() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(5, 5);
    f.ack(0, 3);
    assert_eq!(drain_retransmits(&mut f), vec![(5, 10)]);
}

#[test]
fn fragmenter_ack_range_after_retransmits() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 3);
    f.ack(5, 5);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_touches_retransmit_end() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 3);
    f.ack(3, 3);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_exact_match_removes() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(2, 5);
    f.ack(2, 5);
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_contains_retransmit() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(3, 3);
    f.ack(0, 10);
    assert!(!f.has_retransmits());
}

#[test]
fn fragmenter_ack_leaves_prefix() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 5);
    f.ack(3, 3);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3)]);
}

#[test]
fn fragmenter_ack_leaves_suffix() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(3, 5);
    f.ack(2, 4);
    assert_eq!(drain_retransmits(&mut f), vec![(6, 8)]);
}

#[test]
fn fragmenter_ack_splits_retransmit() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJ");
    f.loss(0, 10);
    f.ack(3, 4);
    assert_eq!(drain_retransmits(&mut f), vec![(0, 3), (7, 10)]);
}

#[test]
fn fragmenter_ack_multiple_retransmits() {
    let mut f = fragmenter_emitted(b"ABCDEFGHIJKLMNOPQRST");
    f.loss(0, 3);
    f.loss(5, 3);
    f.loss(10, 3);
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
