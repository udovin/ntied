use super::super::buffer::*;

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
    assert_eq!(
        buf.write(5, b"abcde", false),
        Err(RecvBufError::FlowControl)
    );
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
    assert_eq!(buf.write(16, b"q", false), Err(RecvBufError::FlowControl));
}
