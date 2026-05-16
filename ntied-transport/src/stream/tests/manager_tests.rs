use super::super::manager::*;

#[test]
fn write_creates_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    assert_eq!(mgr.write(0, b"hello", false).unwrap(), 5);
    assert!(mgr.streams.contains_key(&0));
}

#[test]
fn recv_creates_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.recv(5, 0, b"hello", false).unwrap();
    assert!(mgr.streams.contains_key(&5));
}

#[test]
fn local_id_reuse_rejected() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Create local stream 0, finish; auto-cleanup on final read.
    mgr.write(0, b"hi", true).unwrap();
    mgr.recv(0, 0, b"bye", true).unwrap();
    let mut buf = [0u8; 10];
    mgr.emit(&mut buf);
    // Tracking len includes phantom FIN byte: 2 data + 1 phantom.
    mgr.ack(0, 0, 3);
    mgr.read(0, &mut buf).unwrap();
    assert!(
        !mgr.streams.contains_key(&0),
        "auto-cleanup should remove finished stream"
    );

    // Reuse same local ID → rejected (local_next_id = 2, 0 < 2).
    assert_eq!(mgr.write(0, b"reuse", false), Err(StreamError::IdReused));

    // Next local ID works.
    assert!(mgr.write(2, b"ok", false).is_ok());
}

#[test]
fn peer_id_reuse_rejected() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Peer opens stream 1, finish; auto-cleanup on final read.
    mgr.recv(1, 0, b"data", true).unwrap();
    mgr.write(1, b"reply", true).unwrap();
    let mut buf = [0u8; 10];
    mgr.emit(&mut buf);
    // 5 data + 1 phantom.
    mgr.ack(1, 0, 6);
    mgr.read(1, &mut buf).unwrap();
    assert!(!mgr.streams.contains_key(&1));

    // Peer stream 1 reuse → rejected (≤ peer_highest and not in streams).
    assert_eq!(mgr.recv(1, 0, b"reuse", false), Err(StreamError::IdReused));

    // Peer stream 3 → ok (> peer_highest_id=1).
    assert!(mgr.recv(3, 0, b"new", false).is_ok());
}

#[test]
fn peer_out_of_order_streams() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Peer sends stream 5 first → streams 1, 3, 5 implicitly opened.
    mgr.recv(5, 0, b"five", false).unwrap();
    assert!(mgr.streams.contains_key(&1));
    assert!(mgr.streams.contains_key(&3));
    assert!(mgr.streams.contains_key(&5));

    // Stream 1 data arrives later → works (already created).
    mgr.recv(1, 0, b"one", false).unwrap();

    let mut buf = [0u8; 64];
    let (n, _) = mgr.read(1, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"one");
}

#[test]
fn write_read_roundtrip() {
    let mut mgr = StreamManager::new(64, true, 256);

    mgr.write(0, b"hello", false).unwrap();

    let mut buf = [0u8; 100];
    let Some((id, offset, len, fin)) = mgr.emit(&mut buf) else {
        panic!("expected data");
    };
    assert_eq!(id, 0);
    assert_eq!(offset, 0);
    assert_eq!(len, 5);
    assert!(!fin);

    mgr.recv(1, 0, b"world", false).unwrap();
    let mut out = [0u8; 5];
    let (n, fin) = mgr.read(1, &mut out).unwrap();
    assert_eq!(n, 5);
    assert!(!fin);
    assert_eq!(&out, b"world");
}

#[test]
fn send_round_robin() {
    // Both 0 and 2 are local-parity for is_initiator=true.
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"aaa", false).unwrap();
    mgr.write(2, b"bbb", false).unwrap();

    let mut buf = [0u8; 100];

    let first = mgr.emit(&mut buf).unwrap();
    let second = mgr.emit(&mut buf).unwrap();

    assert_ne!(first.0, second.0);
}

#[test]
fn round_robin_serves_each_stream_in_order() {
    // 4 streams: each emit should yield a distinct stream until all served.
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"a", false).unwrap();
    mgr.write(2, b"b", false).unwrap();
    mgr.write(4, b"c", false).unwrap();
    mgr.write(6, b"d", false).unwrap();

    let mut buf = [0u8; 1];
    let mut order = Vec::new();
    for _ in 0..4 {
        let (id, _, _, _) = mgr.emit(&mut buf).unwrap();
        order.push(id);
    }
    // BTreeMap range yields IDs ascending starting from cursor=0.
    assert_eq!(order, vec![0, 2, 4, 6]);
}

#[test]
fn round_robin_wraps_around() {
    // Force cursor past last ID, verify wrap-around.
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"a", false).unwrap();
    mgr.write(2, b"b", false).unwrap();

    let mut buf = [0u8; 1];
    let (a, _, _, _) = mgr.emit(&mut buf).unwrap(); // 0, cursor=1
    let (b, _, _, _) = mgr.emit(&mut buf).unwrap(); // 2, cursor=3
    // pass 1 (range(3..)) empty → wrap to range(..3) → starts at 0.
    mgr.write(0, b"x", false).unwrap();
    mgr.write(2, b"y", false).unwrap();
    let (c, _, _, _) = mgr.emit(&mut buf).unwrap(); // 0
    let (d, _, _, _) = mgr.emit(&mut buf).unwrap(); // 2
    assert_eq!((a, b, c, d), (0, 2, 0, 2));
}

#[test]
fn round_robin_skips_holes_from_cleanup() {
    // After auto-cleanup of one stream, cursor naturally skips the gap.
    let mut mgr = StreamManager::new(64, true, 256);
    // Open 3 local streams (0, 2, 4). Finish stream 2 fully.
    mgr.write(0, b"a", false).unwrap();
    mgr.write(2, b"b", true).unwrap();
    mgr.write(4, b"c", false).unwrap();
    mgr.recv(2, 0, b"x", true).unwrap();

    let mut buf = [0u8; 100];
    // Drain initial emits and ack stream 2 to drive auto-cleanup.
    for _ in 0..3 {
        if let Some((id, off, len, fin)) = mgr.emit(&mut buf) {
            if id == 2 {
                mgr.ack(2, off, len + (fin as usize));
            }
        }
    }
    let mut out = [0u8; 10];
    let _ = mgr.read(2, &mut out);
    assert!(!mgr.streams.contains_key(&2));

    // Fresh data on streams 0 and 4. Cursor must skip the hole at id=2.
    mgr.write(0, b"X", false).unwrap();
    mgr.write(4, b"Y", false).unwrap();

    let (a, _, _, _) = mgr.emit(&mut buf).unwrap();
    let (b, _, _, _) = mgr.emit(&mut buf).unwrap();
    assert_ne!(a, b);
    assert!(a == 0 || a == 4);
    assert!(b == 0 || b == 4);
}

#[test]
fn send_none_when_empty() {
    let mut mgr = StreamManager::new(64, true, 256);
    assert!(mgr.emit(&mut [0u8; 100]).is_none());
}

#[test]
fn ack_and_loss() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"ABCDE", false).unwrap();

    let mut buf = [0u8; 5];
    mgr.emit(&mut buf);

    mgr.loss(0, 0, 3);
    assert!(mgr.streams[&0].send.has_retransmits());

    mgr.ack(0, 0, 5);
    assert!(!mgr.streams[&0].send.has_retransmits());
}

#[test]
fn auto_cleanup_when_both_sides_finish() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"hi", true).unwrap();
    mgr.recv(0, 0, b"bye", true).unwrap();

    let mut buf = [0u8; 10];
    mgr.emit(&mut buf);
    // 2 data + 1 phantom FIN.
    mgr.ack(0, 0, 3);
    // After ack: send finished. recv not yet (haven't read).
    assert!(mgr.streams.contains_key(&0));

    let mut out = [0u8; 3];
    mgr.read(0, &mut out).unwrap();
    // After read draining all data + reaching FIN: cleanup fires.
    assert!(!mgr.streams.contains_key(&0));
}

#[test]
fn no_cleanup_when_not_finished() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"hi", false).unwrap();
    // Stream is not finished — must remain in map.
    assert!(mgr.streams.contains_key(&0));
}

#[test]
fn readable_writable() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"data", false).unwrap();
    mgr.recv(1, 0, b"incoming", false).unwrap();

    let readable: Vec<u64> = mgr.readable().collect();
    assert!(readable.contains(&1));
    assert!(!readable.contains(&0));

    let writable: Vec<u64> = mgr.writable().collect();
    assert!(writable.contains(&0));
    assert!(writable.contains(&1));
}

#[test]
fn read_unknown_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    assert_eq!(
        mgr.read(99, &mut [0u8; 10]),
        Err(StreamError::UnknownStream)
    );
}

#[test]
fn fin_roundtrip() {
    // is_initiator=false → stream 0 is peer-parity, recv auto-creates.
    let mut mgr = StreamManager::new(64, false, 256);

    mgr.recv(0, 0, b"done", true).unwrap();
    let mut out = [0u8; 4];
    let (n, fin) = mgr.read(0, &mut out).unwrap();
    assert_eq!(n, 4);
    assert!(!fin);

    let (n, fin) = mgr.read(0, &mut out).unwrap();
    assert_eq!(n, 0);
    assert!(fin);
}

#[test]
fn recv_flow_control_error() {
    let mut mgr = StreamManager::new(8, false, 256);
    // Buffer capacity is 8, so max_data is 8. Writing 9 bytes exceeds the window.
    let result = mgr.recv(0, 0, b"123456789", false);
    assert_eq!(result, Err(StreamError::FlowControl));
}

#[test]
fn recv_final_size_mismatch() {
    let mut mgr = StreamManager::new(64, false, 256);
    // First recv sets fin_off = 5
    mgr.recv(0, 0, b"hello", true).unwrap();
    // Second recv with fin at a different offset should fail
    let result = mgr.recv(0, 0, b"hi", true);
    assert_eq!(result, Err(StreamError::FinalSizeMismatch));
}

#[test]
fn window_updates_after_read() {
    // Use a small capacity so that reading triggers should_update_max_data.
    let mut mgr = StreamManager::new(8, false, 256);
    // Receive 6 bytes into stream 0.
    mgr.recv(0, 0, b"abcdef", false).unwrap();

    // Read 5 bytes so that remaining window < capacity/2 (i.e. (8 - 5) < 4).
    let mut out = [0u8; 5];
    let (n, _fin) = mgr.read(0, &mut out).unwrap();
    assert_eq!(n, 5);

    let mut updates = Vec::new();

    mgr.max_data_updates(&mut updates);
    assert!(!updates.is_empty());
    // The update should be for stream 0.
    let (id, new_max) = updates[0];
    assert_eq!(id, 0);
    // new max_data = read_off (5) + capacity (8) = 13
    assert_eq!(new_max, 13);
}

#[test]
fn update_send_max_data_existing() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"hello", false).unwrap();
    // Default max_data equals capacity (64). Increase it.
    mgr.update_send_max_data(0, 128);
    assert_eq!(mgr.streams[&0].send.max_data(), 128);
}

#[test]
fn update_send_max_data_nonexistent() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Should not panic when stream doesn't exist.
    mgr.update_send_max_data(99, 128);
}

// -- Coverage: readable() false branch (stream not readable) ---------------

#[test]
fn readable_excludes_non_readable_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Create stream 0 via write only — no recv data, so not readable.
    mgr.write(0, b"data", false).unwrap();

    let readable: Vec<u64> = mgr.readable().collect();
    assert!(
        !readable.contains(&0),
        "stream with no recv data should not be readable"
    );
}

// -- Coverage: writable() false branch (stream not writable) ---------------

#[test]
fn writable_excludes_fin_sent_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write with fin=true marks the send side as finished.
    mgr.write(0, b"done", true).unwrap();

    let writable: Vec<u64> = mgr.writable().collect();
    assert!(
        !writable.contains(&0),
        "stream with fin sent should not be writable"
    );
}

#[test]
fn writable_excludes_full_send_buffer() {
    let mut mgr = StreamManager::new(4, true, 256);
    // Fill the buffer completely.
    mgr.write(0, b"abcd", false).unwrap();

    let writable: Vec<u64> = mgr.writable().collect();
    assert!(
        !writable.contains(&0),
        "stream with full buffer should not be writable"
    );
}

// -- Coverage: get_or_create already-exists branch -------------------------

#[test]
fn get_or_create_returns_existing_stream() {
    let mut mgr = StreamManager::new(64, true, 256);
    // First write creates stream 0.
    mgr.write(0, b"first", false).unwrap();
    // Second write should use the existing stream (already-exists branch).
    mgr.write(0, b"second", false).unwrap();

    // Verify both writes went to the same stream.
    let mut buf = [0u8; 100];
    let (id, _offset, len, _fin) = mgr.emit(&mut buf).unwrap();
    assert_eq!(id, 0);
    assert_eq!(len, 11); // "first" (5) + "second" (6)
}

#[test]
fn emit_with_empty_buf_returns_none() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"data", false).unwrap();
    assert!(mgr.emit(&mut []).is_none());
}

#[test]
fn too_many_peer_streams() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Peer streams are odd (1, 3, 5, ...). Max is 256.
    // Stream 511 = (511 - 1) / 2 + 1 = 256 → at limit.
    mgr.recv(511, 0, b"x", false).unwrap();
    // Stream 513 would be 257th → TooManyStreams.
    assert_eq!(
        mgr.recv(513, 0, b"x", false),
        Err(StreamError::TooManyStreams)
    );
}

#[test]
fn too_many_local_streams() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Local streams are even (0, 2, 4, ...). Max is 256.
    // Stream 510 = 510 / 2 + 1 = 256 → at limit.
    mgr.write(510, b"x", false).unwrap();
    // Stream 512 would be 257th → TooManyStreams.
    assert_eq!(
        mgr.write(512, b"x", false),
        Err(StreamError::TooManyStreams)
    );
}

// -- MAX_STREAMS flow control --------------------------------------------

#[test]
fn cumulative_credit_exhausted_after_open_close_cycles() {
    // Without MAX_STREAMS updates from peer, we can never exceed the
    // initial credit, even if our streams are auto-cleaned up.
    let mut mgr = StreamManager::new(64, true, 4);
    for i in 0..4u64 {
        mgr.write(i * 2, b"hi", true).unwrap();
        mgr.recv(i * 2, 0, b"hi", true).unwrap();
        let mut buf = [0u8; 100];
        mgr.emit(&mut buf);
        mgr.ack(i * 2, 0, 3); // 2 data + 1 phantom FIN
        let mut out = [0u8; 10];
        let _ = mgr.read(i * 2, &mut out);
        assert!(!mgr.streams.contains_key(&(i * 2)));
    }
    // Cumulative=4, peer_max_streams=4 → next open rejected.
    assert_eq!(mgr.write(8, b"hi", false), Err(StreamError::TooManyStreams));
}

#[test]
fn max_streams_update_grants_more_credit() {
    let mut mgr = StreamManager::new(64, true, 4);
    for i in 0..4u64 {
        mgr.write(i * 2, b"hi", false).unwrap();
    }
    assert_eq!(mgr.write(8, b"hi", false), Err(StreamError::TooManyStreams));
    // Peer grants more credit.
    mgr.update_send_max_streams(8);
    // Now we can open 4 more (8, 10, 12, 14).
    mgr.write(8, b"hi", false).unwrap();
    mgr.write(14, b"hi", false).unwrap();
    // 16 would be 9th → over new limit of 8.
    assert_eq!(
        mgr.write(16, b"hi", false),
        Err(StreamError::TooManyStreams)
    );
}

#[test]
fn cleanup_of_peer_stream_increments_advertised() {
    let mut mgr = StreamManager::new(64, true, 4);
    mgr.recv(1, 0, b"hi", true).unwrap();
    mgr.write(1, b"bye", true).unwrap();
    let mut buf = [0u8; 100];
    mgr.emit(&mut buf);
    mgr.ack(1, 0, 4); // 3 data + 1 phantom
    let mut out = [0u8; 10];
    let _ = mgr.read(1, &mut out);
    assert!(!mgr.streams.contains_key(&1));
    // advertised should now be 5 (was 4, +1 from cleanup).
    // Threshold = max_streams/2 = 2 → not yet triggered.
    assert!(mgr.drain_max_streams_update().is_none());
}

#[test]
fn max_streams_update_triggered_at_threshold() {
    let mut mgr = StreamManager::new(64, true, 4);
    // Open 3 peer streams, finish them to bump advertised by 3.
    for i in 0..3u64 {
        let id = i * 2 + 1; // 1, 3, 5
        mgr.recv(id, 0, b"a", true).unwrap();
        mgr.write(id, b"b", true).unwrap();
        let mut buf = [0u8; 100];
        mgr.emit(&mut buf);
        mgr.ack(id, 0, 2);
        let mut out = [0u8; 10];
        let _ = mgr.read(id, &mut out);
    }
    // advertised = 4 + 3 = 7. sent = 4. Δ = 3 >= max_streams/2 = 2.
    let update = mgr.drain_max_streams_update();
    assert_eq!(update, Some(7));
    assert!(mgr.drain_max_streams_update().is_none()); // already drained
}

#[test]
fn requeue_max_streams_forces_resend() {
    let mut mgr = StreamManager::new(64, true, 4);
    // Bump advertised past threshold.
    for i in 0..3u64 {
        let id = i * 2 + 1;
        mgr.recv(id, 0, b"a", true).unwrap();
        mgr.write(id, b"b", true).unwrap();
        let mut buf = [0u8; 100];
        mgr.emit(&mut buf);
        mgr.ack(id, 0, 2);
        let mut out = [0u8; 10];
        let _ = mgr.read(id, &mut out);
    }
    let _ = mgr.drain_max_streams_update().unwrap();
    // Subsequent drains return None (already sent).
    assert!(mgr.drain_max_streams_update().is_none());
    // Loss → requeue forces resend.
    mgr.requeue_max_streams_update();
    assert_eq!(mgr.drain_max_streams_update(), Some(7));
}

#[test]
fn drain_updated_returns_peer_ids() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.recv(1, 0, b"data", false).unwrap();
    let mut updated = Vec::new();
    mgr.drain_updated(&mut updated);
    assert_eq!(updated, vec![1]);
    assert!(
        {
            let mut t = Vec::new();
            mgr.drain_updated(&mut t);
            t
        }
        .is_empty()
    );
}

#[test]
fn gap_fill_marks_all_updated() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Peer sends on stream 5 → gap-fill creates 1, 3, 5.
    mgr.recv(5, 0, b"five", false).unwrap();
    let mut updated = Vec::new();
    mgr.drain_updated(&mut updated);
    assert!(updated.contains(&1));
    assert!(updated.contains(&3));
    assert!(updated.contains(&5));
}

#[test]
fn has_pending_with_unsent_data() {
    let mut mgr = StreamManager::new(64, true, 256);
    assert!(!mgr.has_pending());

    mgr.write(0, b"data", false).unwrap();
    assert!(mgr.has_pending());

    let mut buf = [0u8; 100];
    mgr.emit(&mut buf);
    assert!(!mgr.has_pending());
}

#[test]
fn has_pending_with_retransmits() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"data", false).unwrap();

    let mut buf = [0u8; 100];
    mgr.emit(&mut buf);
    assert!(!mgr.has_pending());

    mgr.loss(0, 0, 4);
    assert!(mgr.has_pending());
}

#[test]
fn auto_cleanup_decrements_local_count() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"hi", true).unwrap();
    mgr.recv(0, 0, b"bye", true).unwrap();
    let mut buf = [0u8; 10];
    mgr.emit(&mut buf);
    // 2 data + 1 phantom FIN.
    mgr.ack(0, 0, 3);
    mgr.read(0, &mut buf).unwrap();
    assert!(!mgr.streams.contains_key(&0));
    // Local count freed → can write on stream 2.
    mgr.write(2, b"ok", false).unwrap();
}

#[test]
fn auto_cleanup_decrements_peer_count() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.recv(1, 0, b"hi", true).unwrap();
    mgr.write(1, b"bye", true).unwrap();
    let mut buf = [0u8; 10];
    mgr.emit(&mut buf);
    // 3 data + 1 phantom FIN.
    mgr.ack(1, 0, 4);
    mgr.read(1, &mut buf).unwrap();
    assert!(!mgr.streams.contains_key(&1));
}

#[test]
fn local_gap_fill() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write on stream 4 → gap-fill creates 0, 2, 4.
    mgr.write(4, b"data", false).unwrap();
    assert!(mgr.streams.contains_key(&0));
    assert!(mgr.streams.contains_key(&2));
    assert!(mgr.streams.contains_key(&4));
}

#[test]
fn ack_unknown_stream_noop() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.ack(99, 0, 5); // no crash
}

#[test]
fn loss_unknown_stream_noop() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.loss(99, 0, 5); // no crash
}

#[test]
fn unfinished_stream_stays_in_map() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"data", false).unwrap();
    assert!(mgr.streams.contains_key(&0));
}

#[test]
fn emit_empty_write_sends_open_frame() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"", false).unwrap(); // empty write
    let mut buf = [0u8; 100];
    // First emit: empty open frame to notify peer.
    let result = mgr.emit(&mut buf).unwrap();
    assert_eq!(result, (0, 0, 0, false));
    // Second emit: nothing left.
    assert!(mgr.emit(&mut buf).is_none());
}

#[test]
fn emit_fin_only_no_data() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write FIN with no data.
    mgr.write(0, b"", true).unwrap();
    let mut buf = [0u8; 100];
    // emit should return (stream_id, offset, 0, fin=true).
    let result = mgr.emit(&mut buf);
    assert!(result.is_some());
    let (id, _off, len, fin) = result.unwrap();
    assert_eq!(id, 0);
    assert_eq!(len, 0);
    assert!(fin);
}

#[test]
fn loss_covering_fin_resets_fin_sent() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"hi", true).unwrap();

    // Emit all data + FIN.
    let mut buf = [0u8; 100];
    let (_, off, len, fin) = mgr.emit(&mut buf).unwrap();
    assert!(fin);

    // Mark loss covering the FIN offset (tracking len = wire + phantom).
    mgr.loss(0, off, len + (fin as usize));

    // has_pending should be true (need to retransmit).
    assert!(mgr.has_pending());

    // Re-emit: should carry FIN again.
    let (_, _, _, fin2) = mgr.emit(&mut buf).unwrap();
    assert!(fin2);
}

#[test]
fn emit_after_fin_sent_retransmit_data_only() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write data + FIN.
    mgr.write(0, b"ABCDE", true).unwrap();

    // Emit all — FIN sent.
    let mut buf = [0u8; 100];
    let (_, _, _, fin) = mgr.emit(&mut buf).unwrap();
    assert!(fin);

    // Mark only the first 3 bytes as lost (NOT covering FIN offset=5).
    mgr.loss(0, 0, 3);

    // Retransmit: fin_sent is already true, so the `!self.fin_sent` branch
    // in the n>0 emit path evaluates to false.
    let (_, off, len, fin) = mgr.emit(&mut buf).unwrap();
    assert_eq!(off, 0);
    assert_eq!(len, 3);
    assert!(!fin); // FIN already sent, not re-sent.
}

#[test]
fn emit_partial_chunk_no_fin() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write data with FIN, but emit in small chunks.
    mgr.write(0, b"ABCDEFGHIJ", true).unwrap();

    // Emit only 3 bytes at a time — partial emit, fin NOT set.
    let mut small = [0u8; 3];
    let (_, _, len, fin) = mgr.emit(&mut small).unwrap();
    assert_eq!(len, 3);
    assert!(!fin); // not at fin offset yet

    // Emit more.
    let (_, _, len, fin) = mgr.emit(&mut small).unwrap();
    assert_eq!(len, 3);
    assert!(!fin);

    // Emit more.
    let (_, _, len, fin) = mgr.emit(&mut small).unwrap();
    assert_eq!(len, 3);
    assert!(!fin);

    // Last byte + FIN.
    let (_, _, len, fin) = mgr.emit(&mut small).unwrap();
    assert_eq!(len, 1);
    assert!(fin);
}

#[test]
fn no_cleanup_when_only_send_finished() {
    let mut mgr = StreamManager::new(64, true, 256);
    // Write FIN (send side finished).
    mgr.write(0, b"hi", true).unwrap();
    let mut buf = [0u8; 100];
    mgr.emit(&mut buf);
    // 2 data + 1 phantom.
    mgr.ack(0, 0, 3);
    // Don't recv FIN — recv not finished. Stream must remain.
    assert!(mgr.streams.contains_key(&0));
}

#[test]
fn loss_not_covering_fin() {
    let mut mgr = StreamManager::new(64, true, 256);
    mgr.write(0, b"ABCDE", true).unwrap();

    let mut buf = [0u8; 100];
    mgr.emit(&mut buf); // emit all

    // Loss of first 3 bytes only — doesn't cover FIN at offset 5.
    mgr.loss(0, 0, 3);

    let mut out = [0u8; 100];
    let (_, _, _, fin) = mgr.emit(&mut out).unwrap();
    // FIN was already sent and loss doesn't cover it → no fin on retransmit.
    assert!(!fin);
}
