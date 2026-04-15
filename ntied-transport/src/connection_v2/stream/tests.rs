// -- SendBuf / RecvBuf --

mod buffer_tests {
    use super::super::buffer::*;

    // -- SendBuf tests -------------------------------------------------------

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
        assert_eq!(buf.fin_off(), Some(5));

        let (_, n, fin) = buf.emit(&mut [0u8; 1]);
        assert_eq!(n, 0);
        assert!(fin);
    }

    #[test]
    fn send_fin_on_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"end", true);
        let mut out = [0u8; 3];
        buf.emit(&mut out);

        buf.loss(0, 3);
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
        buf.emit(&mut out);
        assert!(!buf.is_finished());

        buf.ack(0, 2);
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

    // -- RecvBuf tests -------------------------------------------------------

    #[test]
    fn recv_in_order() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 5);

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.read_off(), 5);
    }

    #[test]
    fn recv_out_of_order() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.readable(), 0);

        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_duplicate() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 0);
    }

    #[test]
    fn recv_overlapping() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();
        assert_eq!(buf.write(3, b"loworl", false).unwrap(), 0);
    }

    #[test]
    fn recv_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);

        assert_eq!(buf.write(0, b"hell", false).unwrap(), 0);
    }

    #[test]
    fn recv_partial_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hel", false).unwrap();
        let mut out = [0u8; 3];
        buf.read(&mut out);

        buf.update_max_data();
        assert_eq!(buf.write(1, b"ello", false).unwrap(), 2);
        assert_eq!(buf.readable(), 2);
    }

    #[test]
    fn recv_flow_control() {
        let mut buf = RecvBuf::new(8);
        assert_eq!(buf.write(5, b"abcde", false), Err(RecvBufError::FlowControl));
    }

    #[test]
    fn recv_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"done", true).unwrap();
        let mut out = [0u8; 4];
        buf.read(&mut out);
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_fin_with_gap() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"world", true).unwrap();
        assert!(!buf.is_finished());

        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"helloworld");
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_partial_read() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.readable(), 5);

        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"world");
    }

    #[test]
    fn recv_retransmit_overlaps_with_received() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"FGHIJ", false).unwrap();
        assert_eq!(buf.write(0, b"ABCDEFG", false).unwrap(), 5);
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJ");
    }

    #[test]
    fn recv_retransmit_bridges_three_ranges() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"AB", false).unwrap();
        buf.write(5, b"FG", false).unwrap();
        buf.write(10, b"KL", false).unwrap();

        assert_eq!(buf.write(1, b"BCDEFGHIJK", false).unwrap(), 6);
        assert_eq!(buf.readable(), 12);

        let mut out = [0u8; 12];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJKL");
    }

    #[test]
    fn recv_fin_size_mismatch() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert_eq!(
            buf.write(0, b"helloworld", true),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_data_past_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert_eq!(
            buf.write(3, b"loworld", false),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_same_fin_twice_ok() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert!(buf.write(0, b"hello", true).is_ok());
    }

    #[test]
    fn recv_read_nothing_readable() {
        let mut buf = RecvBuf::new(64);
        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 0);

        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.read(&mut out), 0);
    }

    #[test]
    fn recv_write_empty_with_fin() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(5, b"", true).unwrap(), 0);
        assert!(!buf.is_finished());
    }

    #[test]
    fn recv_is_readable() {
        let mut buf = RecvBuf::new(64);
        assert!(!buf.is_readable());

        buf.write(0, b"hi", false).unwrap();
        assert!(buf.is_readable());
    }

    #[test]
    fn recv_flow_control_window() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.window(), 64);

        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);

        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.max_data_next(), 5 + 64);

        let big = vec![0u8; 59];
        buf.update_max_data();
        buf.write(5, &big, false).unwrap();
        let mut tmp = [0u8; 33];
        buf.read(&mut tmp); // read_off = 38. remaining = 69 - 38 = 31 < 32

        assert!(buf.should_update_max_data());
        buf.update_max_data();
        assert_eq!(buf.max_data(), 38 + 64);
    }

    #[test]
    fn recv_should_update_not_after_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);
        assert!(!buf.should_update_max_data());
    }

    #[test]
    fn recv_read_frees_and_allows_more() {
        let mut buf = RecvBuf::new(8);
        buf.write(0, b"abcdefgh", false).unwrap();

        let mut out = [0u8; 4];
        buf.read(&mut out);

        buf.update_max_data();
        buf.write(8, b"ijkl", false).unwrap();
        assert_eq!(buf.readable(), 8);
    }

    #[test]
    fn recv_accessors() {
        let buf = RecvBuf::new(32);
        assert_eq!(buf.capacity(), 32);
        assert_eq!(buf.read_off(), 0);
    }

    #[test]
    fn recv_capacity_limits() {
        let mut buf = RecvBuf::new(16);
        buf.write(0, b"abcdefghijklmnop", false).unwrap();
        assert_eq!(
            buf.write(16, b"q", false),
            Err(RecvBufError::FlowControl)
        );
    }
}

// -- Experimental Buffer --

mod experimental_buffer_tests {
    use super::super::experimental_buffer::*;

    // -- SendBuf tests -------------------------------------------------------

    #[test]
    fn send_basic() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.free(), 16);
        assert_eq!(buf.unsent(), 0);

        assert_eq!(buf.write(b"hello", false), 5);
        assert_eq!(buf.unsent(), 5);
        assert_eq!(buf.free(), 11);

        let mut out = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(off, 0);
        assert_eq!(n, 5);
        assert_eq!(&out, b"hello");

        assert_eq!(buf.unsent(), 0);
        assert_eq!(buf.free(), 11); // not freed until ack

        buf.ack(0, 5);
        assert_eq!(buf.free(), 16);
        assert!(buf.is_empty());
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
        assert_eq!(buf.free(), 0); // not freed until ack
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

        // Window update from peer (they read 4 bytes, new limit = 4 + 8 = 12).
        buf.update_max_data(12);
        assert_eq!(buf.write(b"ijkl", false), 4);
        assert_eq!(buf.unsent(), 8);
    }

    #[test]
    fn send_loss_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false); // 10 bytes

        // Emit in two chunks.
        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(&out, b"ABCDE");

        buf.emit(&mut out);
        assert_eq!(&out, b"FGHIJ");
        assert_eq!(buf.unsent(), 0);
        assert!(!buf.has_retransmits());

        // First chunk [0..5) lost.
        buf.loss(0, 5);
        assert!(buf.has_retransmits());

        // Next emit returns the lost chunk, not new data.
        let mut out2 = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!(off, 0);
        assert_eq!(n, 5);
        assert_eq!(&out2, b"ABCDE");
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_only_unacked_part() {
        let mut buf = SendBuf::new(64);
        buf.write(b"ABCDEFGHIJKLMNOP", false); // 16 bytes

        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Ack [0..5) and [10..16).
        buf.ack(0, 5);
        buf.ack(10, 6);

        // Report loss of entire [0..16). Only [5..10) should be retransmitted.
        buf.loss(0, 16);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 16];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!(off, 5);
        assert_eq!(n, 5);
        assert_eq!(&out2[..5], b"FGHIJ");
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_removes_pending_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);

        let mut out = [0u8; 5];
        buf.emit(&mut out);

        // Loss detected, then late ack arrives.
        buf.loss(0, 5);
        assert!(buf.has_retransmits());

        buf.ack(0, 5);
        assert!(!buf.has_retransmits()); // ack removed it
    }

    #[test]
    fn send_noncontiguous_ack_no_free() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);

        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [5..10) but not [0..5). ack_off stays at 0.
        buf.ack(5, 5);
        assert_eq!(buf.ack_off(), 0);
        assert_eq!(buf.free(), 6); // 16 - (10 - 0) = 6

        // Now ack [0..5). ack_off jumps to 10.
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

        // Emit only 3 bytes of the retransmit.
        let mut small = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut small);
        assert_eq!(off, 0);
        assert_eq!(n, 3);
        assert_eq!(&small, b"ABC");

        // Remaining 7 bytes still in retransmit queue.
        assert!(buf.has_retransmits());
        let mut rest = [0u8; 7];
        let (off, n, _fin) = buf.emit(&mut rest);
        assert_eq!(off, 3);
        assert_eq!(n, 7);
        assert_eq!(&rest, b"DEFGHIJ");
    }

    #[test]
    fn send_wrap_around() {
        let mut buf = SendBuf::new(8);

        buf.write(b"abcdef", false);
        let mut tmp = [0u8; 6];
        buf.emit(&mut tmp);
        buf.ack(0, 6);
        buf.update_max_data(14); // peer window slides

        assert_eq!(buf.write(b"ghijklmn", false), 8);
        assert_eq!(buf.unsent(), 8);

        let mut out = [0u8; 8];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(off, 6);
        assert_eq!(n, 8);
        assert_eq!(&out, b"ghijklmn");
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
            buf.update_max_data(buf.ack_off() + 4); // peer window slides
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
        let (off, n, fin) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 5));
        assert!(fin);
    }

    #[test]
    fn send_fin_partial_emit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", true);

        // Emit only 3 bytes -- not at fin yet.
        let mut out = [0u8; 3];
        let (_, _, fin) = buf.emit(&mut out);
        assert!(!fin);

        // Emit remaining 2 -- now at fin.
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

        // Fin with no data.
        buf.write(b"", true);
        assert_eq!(buf.fin_off(), Some(5));

        // Bare fin emit.
        let (_, n, fin) = buf.emit(&mut [0u8; 1]);
        assert_eq!(n, 0);
        assert!(fin);
    }

    #[test]
    fn send_fin_on_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"end", true);

        let mut out = [0u8; 3];
        buf.emit(&mut out); // send [0..3) with fin

        // Lost -- retransmit.
        buf.loss(0, 3);
        let (off, n, fin) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 3));
        assert!(fin); // retransmit also carries fin
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
        buf.emit(&mut out);
        assert!(!buf.is_finished());

        buf.ack(0, 2);
        assert!(buf.is_finished());
    }

    #[test]
    fn send_window_blocks_emit() {
        let mut buf = SendBuf::new(64);
        buf.write(b"ABCDEFGHIJ", false); // 10 bytes

        // Shrink window to 5 (only allow sending up to offset 5).
        // max_data starts at capacity (64), set it lower manually for test.
        buf.max_data = 5;

        let mut out = [0u8; 10];
        let (off, n, _) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 5)); // only 5 emitted

        // Still have unsent data but blocked.
        assert_eq!(buf.unsent(), 5);
        assert!(buf.is_blocked());

        // Peer sends window update.
        buf.update_max_data(10);
        assert!(!buf.is_blocked());

        let (off, n, _) = buf.emit(&mut out);
        assert_eq!((off, n), (5, 5));
    }

    #[test]
    fn send_window_no_effect_on_retransmit() {
        let mut buf = SendBuf::new(64);
        buf.write(b"ABCDE", false);

        let mut out = [0u8; 5];
        buf.emit(&mut out);

        // Set tiny window -- retransmit should still work.
        buf.max_data = 0;
        buf.loss(0, 5);

        let (off, n, _) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 5)); // retransmit ignores window
        assert_eq!(&out, b"ABCDE");
    }

    #[test]
    fn send_blocked_at_set_and_cleared() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJKLMNOP", false); // 16 bytes, max_data=16

        let mut out = [0u8; 16];
        buf.emit(&mut out); // send_off = 16 = max_data

        // Ring freed by ack. App writes more -- past window.
        buf.ack(0, 16);
        buf.write(b"QRST", false); // write_off = 20 > max_data = 16
        assert_eq!(buf.unsent(), 4);

        // Emit blocked by window.
        let (_, n, _) = buf.emit(&mut out);
        assert_eq!(n, 0);
        assert!(buf.is_blocked());
        assert_eq!(buf.blocked_at(), Some(16));

        // Peer sends WindowUpdate.
        buf.update_max_data(24);
        assert_eq!(buf.blocked_at(), None);
        assert!(!buf.is_blocked());

        let (off, n, _) = buf.emit(&mut out);
        assert_eq!((off, n), (16, 4));
    }

    #[test]
    fn send_write_past_window_into_ring() {
        let mut buf = SendBuf::new(64);
        assert_eq!(buf.write(&[0xAA; 64], false), 64);

        let mut out = [0u8; 64];
        buf.emit(&mut out);
        buf.ack(0, 64);

        // write_off exceeds max_data -- ring has space, window doesn't.
        assert_eq!(buf.write(b"HELLO", false), 5);
        assert_eq!(buf.write_off(), 69);
        assert_eq!(buf.cap(), 59); // ring free, not window limited

        let (_, n, _) = buf.emit(&mut out);
        assert_eq!(n, 0);
        assert!(buf.is_blocked());
    }

    #[test]
    fn recv_should_update_not_after_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);
        assert!(!buf.should_update_max_data());
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

        buf.update_max_data(80); // lower -- ignored
        assert_eq!(buf.max_data(), 100);
    }

    #[test]
    fn recv_flow_control_window() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.window(), 64);

        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);

        // max_data stays at 64 until we commit update.
        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.max_data_next(), 5 + 64); // read_off + capacity

        // After half consumed, should_update_max_data triggers.
        let big = vec![0u8; 59]; // write up to capacity
        buf.write(5, &big, false).unwrap();
        let mut tmp = [0u8; 32];
        buf.read(&mut tmp); // read_off = 37. remaining = 64 - 37 = 27 < 32

        assert!(buf.should_update_max_data());

        buf.update_max_data();
        assert_eq!(buf.max_data(), 37 + 64); // read_off + capacity
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
        buf.ack(0, 0); // no-op
        assert_eq!(buf.ack_off(), 0);
    }

    #[test]
    fn send_loss_zero_len() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abc", false);
        let mut out = [0u8; 3];
        buf.emit(&mut out);
        buf.loss(0, 0); // no-op
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_fully_clamped() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);
        let mut out = [0u8; 5];
        buf.emit(&mut out);
        buf.ack(0, 5);

        // Loss range is fully acked -- clamped to empty.
        buf.loss(0, 5);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_splits_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Mark [0..10) as lost.
        buf.loss(0, 10);
        assert!(buf.has_retransmits());

        // Ack middle [3..7) -- should remove that part from retransmits.
        buf.ack(3, 4);

        // Retransmits should now be [0..3) and [7..10).
        let mut out1 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out1);
        assert_eq!((off, n), (0, 3));
        assert_eq!(&out1, b"ABC");

        let mut out2 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (7, 3));
        assert_eq!(&out2, b"HIJ");

        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_with_partial_ack_at_start() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..4).
        buf.ack(0, 4);

        // Loss [2..8) -- [2..4) is acked, only [4..8) should retransmit.
        buf.loss(2, 6);

        let mut out2 = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (4, 4));
        assert_eq!(&out2, b"EFGH");
    }

    #[test]
    fn send_ack_removes_multiple_retransmits() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false); // 16 bytes
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Create two retransmit ranges.
        buf.loss(2, 3); // [2..5)
        buf.loss(8, 3); // [8..11)

        // Ack [0..16) should remove both from retransmits.
        buf.ack(0, 16);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_fully_covered_by_ack() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack entire range.
        buf.ack(0, 10);

        // Loss of [0..10) -- entirely acked, nothing to retransmit.
        buf.loss(0, 10);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_trims_retransmit_tail() {
        // Tests remove_range inner loop where retransmit starts AFTER ack start
        // and extends past ack end.
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOPQRST", false); // 20 bytes
        let mut out = [0u8; 20];
        buf.emit(&mut out);

        // Retransmit [7..17).
        buf.loss(7, 10);

        // Ack [5..15) -- retransmit [7..15) removed, tail [15..17) remains.
        buf.ack(5, 10);

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (15, 2));
        assert_eq!(&out2, b"PQ");
    }

    #[test]
    fn send_loss_acked_covers_start_via_prev_range() {
        // Tests insert_non_acked where acked range at cursor extends past it.
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Non-contiguous ack: [5..8) only.
        buf.ack(5, 3);
        assert_eq!(buf.ack_off(), 0); // not contiguous from 0

        // Loss [5..10) -- acked [5..8) covers [5..8), only [8..10) retransmitted.
        buf.loss(5, 5);
        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (8, 2));
    }

    #[test]
    fn send_loss_entirely_covered_by_noncontiguous_ack() {
        // Tests insert_non_acked early return when cursor >= end.
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Non-contiguous ack: [5..10) only.
        buf.ack(5, 5);

        // Loss [5..8) -- entirely within acked [5..10).
        buf.loss(5, 3);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_adjacent_to_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Retransmit [0..5).
        buf.loss(0, 5);

        // Ack [5..10) -- adjacent, does not overlap retransmit.
        buf.ack(5, 5);
        assert!(buf.has_retransmits()); // [0..5) still pending
    }

    #[test]
    fn send_ack_partial_overlap_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Retransmit [0..8).
        buf.loss(0, 8);

        // Ack [3..6) -- splits retransmit into [0..3) and [6..8).
        buf.ack(3, 3);

        let mut out1 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out1);
        assert_eq!((off, n), (0, 3));

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (6, 2));
    }

    #[test]
    fn send_loss_acked_covers_start_exactly() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..5) -- covers exactly the start.
        buf.ack(0, 5);

        // Loss [0..5) -- fully acked, nothing to retransmit.
        buf.loss(0, 5);
        assert!(!buf.has_retransmits());

        // Loss [0..8) -- only [5..8) should retransmit.
        buf.loss(0, 8);
        let mut out2 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (5, 3));
    }

    #[test]
    fn send_loss_acked_at_start() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..6).
        buf.ack(0, 6);

        // Loss [0..10) -- acked covers start, only [6..10) retransmitted.
        buf.loss(0, 10);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (6, 4));
        assert_eq!(&out2, b"GHIJ");
    }

    #[test]
    fn send_accessors() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.send_off(), 0);
        assert_eq!(buf.write_off(), 0);

        buf.write(b"hello", false);
        assert_eq!(buf.write_off(), 5);
        assert_eq!(buf.send_off(), 0);

        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(buf.send_off(), 5);
    }

    // -- RecvBuf tests -------------------------------------------------------

    #[test]
    fn recv_in_order() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 5);
        assert_eq!(buf.ranges.len(), 1);

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.read_off(), 5);
        assert!(buf.ranges.is_empty());
    }

    #[test]
    fn recv_out_of_order() {
        let mut buf = RecvBuf::new(64);

        assert_eq!(buf.write(5, b"world", false).unwrap(), 5);
        assert_eq!(buf.readable(), 0);

        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 10);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_duplicate() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 0); // no new bytes

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn recv_overlapping() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();
        assert_eq!(buf.write(3, b"loworl", false).unwrap(), 0); // fully contained

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 10);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();

        let mut out = [0u8; 5];
        buf.read(&mut out);
        assert_eq!(buf.read_off(), 5);

        assert_eq!(buf.write(0, b"hell", false).unwrap(), 0);
    }

    #[test]
    fn recv_partial_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hel", false).unwrap();

        let mut out = [0u8; 3];
        buf.read(&mut out);
        assert_eq!(buf.read_off(), 3);

        // Only bytes [3, 5) are new.
        assert_eq!(buf.write(1, b"ello", false).unwrap(), 2);
        assert_eq!(buf.readable(), 2);

        let mut out2 = [0u8; 2];
        buf.read(&mut out2);
        assert_eq!(&out2, b"lo");
    }

    #[test]
    fn recv_flow_control() {
        let mut buf = RecvBuf::new(8);
        assert_eq!(
            buf.write(5, b"abcde", false),
            Err(RecvBufError::FlowControl)
        );
        assert!(buf.ranges.is_empty());
    }

    #[test]
    fn recv_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"done", true).unwrap();

        let mut out = [0u8; 4];
        buf.read(&mut out);
        assert_eq!(&out, b"done");
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_fin_with_gap() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"world", true).unwrap();
        assert!(!buf.is_finished());

        buf.write(0, b"hello", false).unwrap();

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"helloworld");
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_partial_read() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.readable(), 5);

        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"world");
    }

    #[test]
    fn recv_retransmit_overlaps_with_received() {
        let mut buf = RecvBuf::new(64);

        buf.write(5, b"FGHIJ", false).unwrap();
        assert_eq!(buf.readable(), 0);

        // Retransmit [0..7) overlaps [5..10).
        assert_eq!(buf.write(0, b"ABCDEFG", false).unwrap(), 5); // only 5 new
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJ");
    }

    #[test]
    fn recv_retransmit_bridges_three_ranges() {
        let mut buf = RecvBuf::new(64);

        buf.write(0, b"AB", false).unwrap();
        buf.write(5, b"FG", false).unwrap();
        buf.write(10, b"KL", false).unwrap();
        assert_eq!(buf.readable(), 2);

        // [1..11) bridges all three ranges.
        assert_eq!(buf.write(1, b"BCDEFGHIJK", false).unwrap(), 6); // 6 new bytes
        assert_eq!(buf.readable(), 12);

        let mut out = [0u8; 12];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJKL");
    }

    #[test]
    fn recv_multiple_gaps_then_fill() {
        let mut buf = RecvBuf::new(64);

        buf.write(10, b"cc", false).unwrap();
        buf.write(20, b"ee", false).unwrap();
        buf.write(0, b"aa", false).unwrap();
        assert_eq!(buf.readable(), 2);

        let mut out = [0u8; 2];
        buf.read(&mut out);
        assert_eq!(&out, b"aa");

        buf.write(2, b"bbbbbbbb", false).unwrap();
        assert_eq!(buf.readable(), 10);

        let mut out2 = [0u8; 10];
        buf.read(&mut out2);
        assert_eq!(&out2, b"bbbbbbbbcc");
    }

    #[test]
    fn recv_fin_size_mismatch() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap(); // fin at 5

        // Different fin offset -> error.
        assert_eq!(
            buf.write(0, b"helloworld", true),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_data_past_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap(); // fin at 5

        // Non-fin data extending past fin -> error.
        assert_eq!(
            buf.write(3, b"loworld", false),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_same_fin_twice_ok() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        // Same fin offset is fine.
        assert!(buf.write(0, b"hello", true).is_ok());
    }

    #[test]
    fn recv_full_duplicate_skips_ring_write() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        // Fully covered -- returns 0, no redundant work.
        assert_eq!(buf.write(1, b"ell", false).unwrap(), 0);
    }

    #[test]
    fn recv_read_nothing_readable() {
        let mut buf = RecvBuf::new(64);
        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 0);

        // Gap at [0, 5) -- data at 5 but nothing contiguous from 0.
        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.read(&mut out), 0);
    }

    #[test]
    fn recv_write_empty_with_fin() {
        let mut buf = RecvBuf::new(64);
        // Empty data with fin -- sets fin but writes 0 bytes.
        assert_eq!(buf.write(5, b"", true).unwrap(), 0);
        assert!(!buf.is_finished()); // not finished, read_off=0 != fin=5
    }

    #[test]
    fn recv_is_readable() {
        let mut buf = RecvBuf::new(64);
        assert!(!buf.is_readable());

        buf.write(0, b"hi", false).unwrap();
        assert!(buf.is_readable());
    }

    #[test]
    fn recv_wrap_around() {
        let mut buf = RecvBuf::new(8);

        buf.write(0, b"abcdef", false).unwrap();
        let mut tmp = [0u8; 6];
        buf.read(&mut tmp);
        assert_eq!(buf.read_off(), 6);

        // Slide window so [6..14) is within max_data.
        buf.update_max_data(); // max_data = 6 + 8 = 14

        buf.write(6, b"ghijklmn", false).unwrap();
        assert_eq!(buf.readable(), 8);

        let mut out = [0u8; 8];
        buf.read(&mut out);
        assert_eq!(&out, b"ghijklmn");
    }

    #[test]
    fn recv_readable_after_partial_read() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap(); // [0, 10)

        // Partial read: 3 bytes. read_off = 3, range still {0: 10}.
        let mut out = [0u8; 3];
        buf.read(&mut out);
        assert_eq!(&out, b"hel");
        assert_eq!(buf.read_off(), 3);

        // readable should be 7, not 10.
        assert_eq!(buf.readable(), 7);

        // Read the rest.
        let mut out2 = [0u8; 7];
        assert_eq!(buf.read(&mut out2), 7);
        assert_eq!(&out2, b"loworld");
    }

    #[test]
    fn recv_accessors() {
        let buf = RecvBuf::new(32);
        assert_eq!(buf.capacity(), 32);
        assert_eq!(buf.read_off(), 0);
    }

    #[test]
    fn recv_capacity_limits_total_stored() {
        let mut buf = RecvBuf::new(16);
        buf.write(0, b"abcdefghijklmnop", false).unwrap();
        assert_eq!(buf.ranges.len(), 1);

        // window_end = 0 + 16 = 16, offset 16 is out of window.
        assert_eq!(buf.write(16, b"q", false), Err(RecvBufError::FlowControl));
    }
}

// -- StreamManager --

mod manager_tests {
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
        // Create local stream 0, finish, and remove.
        mgr.write(0, b"hi", true).unwrap();
        mgr.recv(0, 0, b"bye", true).unwrap();
        let mut buf = [0u8; 10];
        mgr.emit(&mut buf);
        mgr.ack(0, 0, 2);
        mgr.read(0, &mut buf).unwrap();
        assert!(mgr.remove(0));

        // Reuse same local ID → rejected (local_next_id = 2, 0 < 2).
        assert_eq!(mgr.write(0, b"reuse", false), Err(StreamError::IdReused));

        // Next local ID works.
        assert!(mgr.write(2, b"ok", false).is_ok());
    }

    #[test]
    fn peer_id_reuse_rejected() {
        let mut mgr = StreamManager::new(64, true, 256);
        // Peer opens stream 1, finish, remove.
        mgr.recv(1, 0, b"data", true).unwrap();
        mgr.write(1, b"reply", true).unwrap();
        let mut buf = [0u8; 10];
        mgr.emit(&mut buf);
        mgr.ack(1, 0, 5);
        mgr.read(1, &mut buf).unwrap();
        assert!(mgr.remove(1));

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
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.write(0, b"aaa", false).unwrap();
        mgr.write(1, b"bbb", false).unwrap();

        let mut buf = [0u8; 100];

        let first = mgr.emit(&mut buf).unwrap();
        let second = mgr.emit(&mut buf).unwrap();

        assert_ne!(first.0, second.0);
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
    fn remove_finished() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.write(0, b"hi", true).unwrap();
        mgr.recv(0, 0, b"bye", true).unwrap();

        let mut buf = [0u8; 10];
        mgr.emit(&mut buf);
        mgr.ack(0, 0, 2);

        let mut out = [0u8; 3];
        mgr.read(0, &mut out).unwrap();

        assert!(mgr.remove(0));
        assert!(!mgr.streams.contains_key(&0));
    }

    #[test]
    fn remove_not_finished() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.write(0, b"hi", false).unwrap();
        assert!(!mgr.remove(0));
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
        let mut mgr = StreamManager::new(64, true, 256);

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
        let mut mgr = StreamManager::new(8, true, 256);
        // Buffer capacity is 8, so max_data is 8. Writing 9 bytes exceeds the window.
        let result = mgr.recv(0, 0, b"123456789", false);
        assert_eq!(result, Err(StreamError::FlowControl));
    }

    #[test]
    fn recv_final_size_mismatch() {
        let mut mgr = StreamManager::new(64, true, 256);
        // First recv sets fin_off = 5
        mgr.recv(0, 0, b"hello", true).unwrap();
        // Second recv with fin at a different offset should fail
        let result = mgr.recv(0, 0, b"hi", true);
        assert_eq!(result, Err(StreamError::FinalSizeMismatch));
    }

    #[test]
    fn remove_nonexistent() {
        let mut mgr = StreamManager::new(64, true, 256);
        assert!(!mgr.remove(42));
    }

    #[test]
    fn window_updates_after_read() {
        // Use a small capacity so that reading triggers should_update_max_data.
        let mut mgr = StreamManager::new(8, true, 256);
        // Receive 6 bytes into stream 0.
        mgr.recv(0, 0, b"abcdef", false).unwrap();

        // Read 5 bytes so that remaining window < capacity/2 (i.e. (8 - 5) < 4).
        let mut out = [0u8; 5];
        let (n, _fin) = mgr.read(0, &mut out).unwrap();
        assert_eq!(n, 5);

        let updates = mgr.window_updates();
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
        assert!(!readable.contains(&0), "stream with no recv data should not be readable");
    }

    // -- Coverage: writable() false branch (stream not writable) ---------------

    #[test]
    fn writable_excludes_fin_sent_stream() {
        let mut mgr = StreamManager::new(64, true, 256);
        // Write with fin=true marks the send side as finished.
        mgr.write(0, b"done", true).unwrap();

        let writable: Vec<u64> = mgr.writable().collect();
        assert!(!writable.contains(&0), "stream with fin sent should not be writable");
    }

    #[test]
    fn writable_excludes_full_send_buffer() {
        let mut mgr = StreamManager::new(4, true, 256);
        // Fill the buffer completely.
        mgr.write(0, b"abcd", false).unwrap();

        let writable: Vec<u64> = mgr.writable().collect();
        assert!(!writable.contains(&0), "stream with full buffer should not be writable");
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

    #[test]
    fn drain_updated_returns_peer_ids() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.recv(1, 0, b"data", false).unwrap();
        let updated = mgr.drain_updated();
        assert_eq!(updated, vec![1]);
        assert!(mgr.drain_updated().is_empty());
    }

    #[test]
    fn gap_fill_marks_all_updated() {
        let mut mgr = StreamManager::new(64, true, 256);
        // Peer sends on stream 5 → gap-fill creates 1, 3, 5.
        mgr.recv(5, 0, b"five", false).unwrap();
        let updated = mgr.drain_updated();
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
    fn remove_decrements_local_count() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.write(0, b"hi", true).unwrap();
        mgr.recv(0, 0, b"bye", true).unwrap();
        let mut buf = [0u8; 10];
        mgr.emit(&mut buf);
        mgr.ack(0, 0, 2);
        mgr.read(0, &mut buf).unwrap();
        assert!(mgr.remove(0));
        // Should be able to write on stream 2 (local count freed).
        mgr.write(2, b"ok", false).unwrap();
    }

    #[test]
    fn remove_decrements_peer_count() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.recv(1, 0, b"hi", true).unwrap();
        mgr.write(1, b"bye", true).unwrap();
        let mut buf = [0u8; 10];
        mgr.emit(&mut buf);
        mgr.ack(1, 0, 3);
        mgr.read(1, &mut buf).unwrap();
        assert!(mgr.remove(1));
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
    fn remove_nonexistent_returns_false() {
        let mut mgr = StreamManager::new(64, true, 256);
        assert!(!mgr.remove(99));
    }

    #[test]
    fn remove_not_finished_returns_false() {
        let mut mgr = StreamManager::new(64, true, 256);
        mgr.write(0, b"data", false).unwrap();
        assert!(!mgr.remove(0));
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

        // Mark loss covering the FIN offset.
        mgr.loss(0, off, len);

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
    fn remove_send_finished_recv_not() {
        let mut mgr = StreamManager::new(64, true, 256);
        // Write FIN (send side finished).
        mgr.write(0, b"hi", true).unwrap();
        let mut buf = [0u8; 100];
        mgr.emit(&mut buf);
        mgr.ack(0, 0, 2);
        // Don't recv FIN — recv not finished.
        // remove() should return false: send finished, recv not finished.
        assert!(!mgr.remove(0));
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
}
