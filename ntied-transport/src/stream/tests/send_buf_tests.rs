use super::super::buffer::*;

#[test]
fn send_basic() {
    let mut buf = SendBuf::new(16);
    assert_eq!(buf.capacity(), 16);
    assert_eq!(buf.free(), 16);
    assert_eq!(buf.unsent(), 0);

    assert_eq!(buf.write(b"hello", false), 5);
    assert_eq!(buf.unsent(), 5);

    let mut out = [0u8; 5];
    let (off, n, fin) = buf.emit(&mut out);
    assert_eq!((off, n), (0, 5));
    assert!(!fin);
    assert_eq!(&out, b"hello");

    buf.ack(0, 5);
    assert!(buf.is_empty());
    assert_eq!(buf.free(), 16);
}

#[test]
fn send_partial_write_when_full() {
    let mut buf = SendBuf::new(4);
    assert_eq!(buf.write(b"abcd", false), 4);
    assert_eq!(buf.free(), 0);
    assert_eq!(buf.write(b"e", false), 0);
}

#[test]
fn send_emit_multiple_chunks() {
    let mut buf = SendBuf::new(1024);
    let data = vec![0xABu8; 1024];
    assert_eq!(buf.write(&data, false), 1024);

    let mut total = 0;
    let mut chunk = [0u8; 100];
    while buf.unsent() > 0 {
        let (off, n, _fin) = buf.emit(&mut chunk);
        assert_eq!(off, total as u64);
        total += n;
    }
    assert_eq!(total, 1024);
    assert_eq!(buf.free(), 0);
}

#[test]
fn send_ack_frees_space() {
    let mut buf = SendBuf::new(8);
    buf.write(b"abcdefgh", false);
    assert_eq!(buf.free(), 0);

    let mut out = [0u8; 4];
    buf.emit(&mut out);
    buf.ack(0, 4);
    assert_eq!(buf.free(), 4);
    assert_eq!(buf.ack_off(), 4);

    buf.update_max_data(12);
    assert_eq!(buf.write(b"ijkl", false), 4);
    assert_eq!(buf.unsent(), 8);
}

#[test]
fn send_loss_retransmit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);

    let mut out = [0u8; 5];
    buf.emit(&mut out);
    assert_eq!(&out, b"ABCDE");
    buf.emit(&mut out);
    assert_eq!(&out, b"FGHIJ");
    assert_eq!(buf.unsent(), 0);

    buf.loss(0, 5);
    assert!(buf.has_retransmits());

    let mut out2 = [0u8; 5];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (0, 5));
    assert_eq!(&out2, b"ABCDE");
}

// -- FIN tracking via phantom byte ---------------------------------------
//
// Contract: ack/loss receive a `len` that is the *tracking* length
// = wire bytes + (fin as usize). Connection layer adds the phantom byte;
// SendBuf treats it uniformly with data, so FIN-only frames are tracked
// via the [fin_off-1, fin_off) range.

#[test]
fn fin_only_ack_marks_finished() {
    let mut buf = SendBuf::new(64);
    buf.write(b"hello", false);
    let mut out = [0u8; 64];
    let (off1, n1, fin1) = buf.emit(&mut out);
    assert!(!fin1);
    buf.ack(off1, n1);

    buf.write(b"", true);
    let (off2, n2, fin2) = buf.emit(&mut out);
    assert!(fin2);
    assert_eq!(n2, 0);
    // FIN frame not yet acknowledged — must NOT be finished.
    assert!(!buf.is_finished());

    // Connection layer acks with phantom byte: len = wire_n + (fin as usize).
    buf.ack(off2, n2 + (fin2 as usize));
    assert!(buf.is_finished());
}

#[test]
fn fin_with_data_ack_marks_finished() {
    let mut buf = SendBuf::new(64);
    buf.write(b"hello", true);
    let mut out = [0u8; 64];
    let (off, n, fin) = buf.emit(&mut out);
    assert!(fin);
    assert_eq!(n, 5);
    assert!(!buf.is_finished());

    buf.ack(off, n + (fin as usize));
    assert!(buf.is_finished());
}

#[test]
fn fin_only_loss_retransmits() {
    let mut buf = SendBuf::new(64);
    buf.write(b"data", false);
    let mut out = [0u8; 64];
    let (off1, n1, _) = buf.emit(&mut out);
    buf.ack(off1, n1);

    buf.write(b"", true);
    let (off2, n2, fin2) = buf.emit(&mut out);
    assert!(fin2);

    buf.loss(off2, n2 + (fin2 as usize));
    assert!(buf.has_retransmits());

    let (off3, n3, fin3) = buf.emit(&mut out);
    assert_eq!((off3, n3), (off2, 0));
    assert!(fin3);
    assert!(!buf.has_retransmits());
}

#[test]
fn fin_with_data_loss_retransmits_with_fin() {
    let mut buf = SendBuf::new(64);
    buf.write(b"data", true);
    let mut out = [0u8; 64];
    let (off, n, fin) = buf.emit(&mut out);
    assert!(fin);

    buf.loss(off, n + (fin as usize));
    assert!(buf.has_retransmits());

    let (off2, n2, fin2) = buf.emit(&mut out);
    assert_eq!((off2, n2), (0, 4));
    assert!(fin2);
}

#[test]
fn send_loss_only_unacked_part() {
    let mut buf = SendBuf::new(64);
    buf.write(b"ABCDEFGHIJKLMNOP", false);
    let mut out = [0u8; 16];
    buf.emit(&mut out);

    buf.ack(0, 5);
    buf.ack(10, 6);
    buf.loss(0, 16);

    let mut out2 = [0u8; 16];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (5, 5));
    assert_eq!(&out2[..5], b"FGHIJ");
    assert!(!buf.has_retransmits());
}

#[test]
fn send_ack_removes_pending_retransmit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDE", false);
    let mut out = [0u8; 5];
    buf.emit(&mut out);

    buf.loss(0, 5);
    assert!(buf.has_retransmits());
    buf.ack(0, 5);
    assert!(!buf.has_retransmits());
}

#[test]
fn send_ack_overlapping_ranges() {
    let mut buf = SendBuf::new(32);
    buf.write(b"ABCDEFGHIJKLMNOP", false);
    let mut out = [0u8; 16];
    buf.emit(&mut out);

    // Create non-contiguous acked range that won't be drained.
    buf.ack(2, 4); // acked = {2: 6}. Not from 0, stays.
    // Now ack overlapping range: insert_range(4, 8).
    // range(..4) finds (2, 6). re=6 >= start=4 -> prev merge!
    buf.ack(4, 4);
    assert_eq!(buf.ack_off(), 0); // gap at [0..2)

    buf.ack(0, 2);
    assert_eq!(buf.ack_off(), 8);
}

#[test]
fn send_noncontiguous_ack_gap_between() {
    let mut buf = SendBuf::new(32);
    buf.write(b"ABCDEFGHIJKLMNOP", false);
    let mut out = [0u8; 16];
    buf.emit(&mut out);

    buf.ack(0, 5);
    buf.ack(10, 6);
    assert_eq!(buf.ack_off(), 5);
}

#[test]
fn send_noncontiguous_ack_no_free() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    buf.ack(5, 5);
    assert_eq!(buf.ack_off(), 0);

    buf.ack(0, 5);
    assert_eq!(buf.ack_off(), 10);
    assert_eq!(buf.free(), 16);
}

#[test]
fn send_partial_retransmit_emit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    buf.loss(0, 10);

    let mut small = [0u8; 3];
    let (off, n, _fin) = buf.emit(&mut small);
    assert_eq!((off, n), (0, 3));
    assert_eq!(&small, b"ABC");

    assert!(buf.has_retransmits());
    let mut rest = [0u8; 7];
    let (off, n, _fin) = buf.emit(&mut rest);
    assert_eq!((off, n), (3, 7));
    assert_eq!(&rest, b"DEFGHIJ");
}

#[test]
fn send_repeated_cycles() {
    let mut buf = SendBuf::new(4);
    for i in 0u8..=255 {
        let data = [i; 4];
        assert_eq!(buf.write(&data, false), 4);

        let mut out = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(n, 4);
        assert_eq!(out, data);

        buf.ack(off, n);
        buf.update_max_data(buf.ack_off() + 4);
        assert!(buf.is_empty());
    }
}

#[test]
fn send_empty_emit() {
    let mut buf = SendBuf::new(8);
    let mut out = [0u8; 4];
    let (off, n, _fin) = buf.emit(&mut out);
    assert_eq!(off, 0);
    assert_eq!(n, 0);
}

#[test]
fn send_fin_on_last_emit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"hello", true);

    let mut out = [0u8; 5];
    let (_, _, fin) = buf.emit(&mut out);
    assert!(fin);
}

#[test]
fn send_fin_partial_emit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"hello", true);

    let mut out = [0u8; 3];
    let (_, _, fin) = buf.emit(&mut out);
    assert!(!fin);

    let mut out2 = [0u8; 2];
    let (_, _, fin) = buf.emit(&mut out2);
    assert!(fin);
}

#[test]
fn send_fin_empty_data() {
    let mut buf = SendBuf::new(16);
    buf.write(b"hello", false);
    let mut out = [0u8; 5];
    buf.emit(&mut out);

    buf.write(b"", true);
    // fin_off includes phantom byte: write_off(5) + 1.
    assert_eq!(buf.fin_off(), Some(6));

    let (_, n, fin) = buf.emit(&mut [0u8; 1]);
    assert_eq!(n, 0);
    assert!(fin);
}

#[test]
fn send_fin_on_retransmit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"end", true);
    let mut out = [0u8; 3];
    let (_, n, fin) = buf.emit(&mut out);
    assert!(fin);

    // Tracking len = wire_n + fin as usize.
    buf.loss(0, n + (fin as usize));
    let (_, _, fin) = buf.emit(&mut out);
    assert!(fin);
}

#[test]
fn send_write_after_fin_rejected() {
    let mut buf = SendBuf::new(16);
    buf.write(b"hello", true);
    assert_eq!(buf.write(b"more", false), 0);
}

#[test]
fn send_is_finished() {
    let mut buf = SendBuf::new(16);
    buf.write(b"hi", true);
    assert!(!buf.is_finished());

    let mut out = [0u8; 2];
    let (_, n, fin) = buf.emit(&mut out);
    assert!(fin);
    assert!(!buf.is_finished());

    // Tracking len includes phantom byte.
    buf.ack(0, n + (fin as usize));
    assert!(buf.is_finished());
}

#[test]
fn send_emit_empty_out() {
    let mut buf = SendBuf::new(8);
    buf.write(b"abc", false);
    let (off, n, _fin) = buf.emit(&mut []);
    assert_eq!(n, 0);
    assert_eq!(off, 0);
}

#[test]
fn send_ack_zero_len() {
    let mut buf = SendBuf::new(8);
    buf.write(b"abc", false);
    let mut out = [0u8; 3];
    buf.emit(&mut out);
    buf.ack(0, 0);
    assert_eq!(buf.ack_off(), 0);
}

#[test]
fn send_loss_zero_len() {
    let mut buf = SendBuf::new(8);
    buf.write(b"abc", false);
    let mut out = [0u8; 3];
    buf.emit(&mut out);
    buf.loss(0, 0);
    assert!(!buf.has_retransmits());
}

#[test]
fn send_loss_fully_clamped() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDE", false);
    let mut out = [0u8; 5];
    buf.emit(&mut out);
    buf.ack(0, 5);

    buf.loss(0, 5);
    assert!(!buf.has_retransmits());
}

#[test]
fn send_ack_splits_retransmit() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    buf.loss(0, 10);
    buf.ack(3, 4);

    let mut out1 = [0u8; 3];
    let (off, n, _fin) = buf.emit(&mut out1);
    assert_eq!((off, n), (0, 3));

    let mut out2 = [0u8; 3];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (7, 3));
}

#[test]
fn send_ack_removes_multiple_retransmits() {
    let mut buf = SendBuf::new(32);
    buf.write(b"ABCDEFGHIJKLMNOP", false);
    let mut out = [0u8; 16];
    buf.emit(&mut out);

    buf.loss(2, 3);
    buf.loss(8, 3);

    buf.ack(0, 16);
    assert!(!buf.has_retransmits());
}

#[test]
fn send_ack_no_overlap_with_retransmit() {
    let mut buf = SendBuf::new(32);
    buf.write(b"ABCDEFGHIJKLMNOP", false);
    let mut out = [0u8; 16];
    buf.emit(&mut out);

    // Retransmit [0..5).
    buf.loss(0, 5);

    // Ack [5..10) -- does not overlap retransmit [0..5).
    buf.ack(5, 5);
    assert!(buf.has_retransmits()); // [0..5) still there
}

#[test]
fn send_ack_trims_retransmit_tail() {
    let mut buf = SendBuf::new(32);
    buf.write(b"ABCDEFGHIJKLMNOPQRST", false);
    let mut out = [0u8; 20];
    buf.emit(&mut out);

    buf.loss(7, 10);
    buf.ack(5, 10);

    let mut out2 = [0u8; 2];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (15, 2));
}

#[test]
fn send_loss_acked_covers_start_via_prev_range() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    buf.ack(5, 3);
    buf.loss(5, 5);

    let mut out2 = [0u8; 2];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (8, 2));
}

#[test]
fn send_loss_at_acked_boundary() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    // Non-contiguous ack so it won't drain.
    buf.ack(2, 3); // acked = {2: 5}. Not from 0.
    // Loss starting exactly where acked ends.
    // insert_non_acked: cursor=5, range(..=5) finds (2,5). re=5 > cursor=5? No.
    buf.loss(5, 5);
    assert!(buf.has_retransmits());

    let mut out2 = [0u8; 5];
    let (off, n, _fin) = buf.emit(&mut out2);
    assert_eq!((off, n), (5, 5));
}

#[test]
fn send_loss_entirely_covered_by_noncontiguous_ack() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJ", false);
    let mut out = [0u8; 10];
    buf.emit(&mut out);

    buf.ack(5, 5);
    buf.loss(5, 3);
    assert!(!buf.has_retransmits());
}

#[test]
fn send_blocked_at_set_and_cleared() {
    let mut buf = SendBuf::new(16);
    buf.write(b"ABCDEFGHIJKLMNOP", false);

    let mut out = [0u8; 16];
    buf.emit(&mut out);

    buf.ack(0, 16);
    buf.write(b"QRST", false);
    assert_eq!(buf.unsent(), 4);

    let (_, n, _) = buf.emit(&mut out);
    assert_eq!(n, 0);
    assert!(buf.is_blocked());
    assert_eq!(buf.blocked_at(), Some(16));

    buf.update_max_data(24);
    assert_eq!(buf.blocked_at(), None);
    assert!(!buf.is_blocked());

    let (off, n, _fin) = buf.emit(&mut out);
    assert_eq!((off, n), (16, 4));
}

#[test]
fn send_write_past_window_into_buffer() {
    let mut buf = SendBuf::new(64);
    assert_eq!(buf.write(&[0xAA; 64], false), 64);

    let mut out = [0u8; 64];
    buf.emit(&mut out);
    buf.ack(0, 64);

    assert_eq!(buf.write(b"HELLO", false), 5);
    assert_eq!(buf.cap(), 59);

    let (_, n, _) = buf.emit(&mut out);
    assert_eq!(n, 0);
    assert!(buf.is_blocked());
}

#[test]
fn send_window_default_is_capacity() {
    let buf = SendBuf::new(256);
    assert_eq!(buf.max_data(), 256);
}

#[test]
fn send_update_max_data_only_increases() {
    let mut buf = SendBuf::new(64);
    assert_eq!(buf.max_data(), 64);

    buf.update_max_data(100);
    assert_eq!(buf.max_data(), 100);

    buf.update_max_data(80);
    assert_eq!(buf.max_data(), 100);
}

#[test]
fn send_copy_out_wraps_in_deque() {
    // Force VecDeque internal wrap by cycling many times.
    let mut buf = SendBuf::new(4);
    for round in 0u64..10 {
        let data = [b'A' + (round as u8 % 26); 4];
        buf.write(&data, false);

        let mut out = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(n, 4);
        assert_eq!(out, data);

        buf.ack(off, n);
        buf.update_max_data(buf.ack_off() + 4);
    }
    // After many cycles, VecDeque head has wrapped.
    // Verify data is still correct.
    buf.write(b"WXYZ", false);
    let mut out = [0u8; 4];
    let (_, n, _) = buf.emit(&mut out);
    assert_eq!(n, 4);
    assert_eq!(&out, b"WXYZ");
}

#[test]
fn send_accessors() {
    let mut buf = SendBuf::new(16);
    assert_eq!(buf.capacity(), 16);
    assert_eq!(buf.send_off(), 0);
    assert_eq!(buf.write_off(), 0);

    buf.write(b"hello", false);
    assert_eq!(buf.write_off(), 5);

    let mut out = [0u8; 5];
    buf.emit(&mut out);
    assert_eq!(buf.send_off(), 5);
}
