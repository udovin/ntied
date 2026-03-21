use super::*;

#[test]
fn writer_u8_reader_u8() {
    let mut w = Writer::new();
    w.write_u8(0x00);
    w.write_u8(0xFF);
    w.write_u8(0x42);

    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_u8().unwrap(), 0x00);
    assert_eq!(r.read_u8().unwrap(), 0xFF);
    assert_eq!(r.read_u8().unwrap(), 0x42);
    assert!(r.is_empty());
}

#[test]
fn writer_u16_reader_u16_big_endian() {
    let mut w = Writer::new();
    w.write_u16(0x0102);

    assert_eq!(w.as_bytes(), &[0x01, 0x02]);

    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_u16().unwrap(), 0x0102);
    assert!(r.is_empty());
}

#[test]
fn writer_u32_reader_u32_big_endian() {
    let mut w = Writer::new();
    w.write_u32(0x01020304);

    assert_eq!(w.as_bytes(), &[0x01, 0x02, 0x03, 0x04]);

    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_u32().unwrap(), 0x01020304);
    assert!(r.is_empty());
}

#[test]
fn writer_u64_reader_u64_big_endian() {
    let mut w = Writer::new();
    w.write_u64(0x0102030405060708);

    assert_eq!(
        w.as_bytes(),
        &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    );

    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_u64().unwrap(), 0x0102030405060708);
    assert!(r.is_empty());
}

#[test]
fn read_array_fixed_size() {
    let data = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    let mut r = Reader::new(&data);

    let arr: [u8; 3] = r.read_array().unwrap();
    assert_eq!(arr, [0xAA, 0xBB, 0xCC]);
    assert_eq!(r.remaining_len(), 2);

    let arr: [u8; 2] = r.read_array().unwrap();
    assert_eq!(arr, [0xDD, 0xEE]);
    assert!(r.is_empty());
}

#[test]
fn read_bytes_borrows_slice() {
    let data = [1, 2, 3, 4, 5];
    let mut r = Reader::new(&data);

    let slice = r.read_bytes(3).unwrap();
    assert_eq!(slice, &[1, 2, 3]);

    let slice = r.read_bytes(2).unwrap();
    assert_eq!(slice, &[4, 5]);
    assert!(r.is_empty());
}

#[test]
fn remaining_returns_unread_data() {
    let data = [10, 20, 30, 40];
    let mut r = Reader::new(&data);

    r.read_u8().unwrap();
    assert_eq!(r.remaining(), &[20, 30, 40]);
    assert_eq!(r.remaining_len(), 3);
}

#[test]
fn read_u8_underflow() {
    let mut r = Reader::new(&[]);
    assert_eq!(r.read_u8(), Err(CodecError::UnexpectedEnd));
}

#[test]
fn read_u16_underflow() {
    let mut r = Reader::new(&[0x01]);
    assert_eq!(r.read_u16(), Err(CodecError::UnexpectedEnd));
}

#[test]
fn read_u32_underflow() {
    let mut r = Reader::new(&[0x01, 0x02, 0x03]);
    assert_eq!(r.read_u32(), Err(CodecError::UnexpectedEnd));
}

#[test]
fn read_u64_underflow() {
    let mut r = Reader::new(&[0; 7]);
    assert_eq!(r.read_u64(), Err(CodecError::UnexpectedEnd));
}

#[test]
fn read_array_underflow() {
    let mut r = Reader::new(&[0x01, 0x02]);
    assert_eq!(r.read_array::<4>(), Err(CodecError::UnexpectedEnd));
}

#[test]
fn read_bytes_underflow() {
    let mut r = Reader::new(&[0x01]);
    assert_eq!(r.read_bytes(5), Err(CodecError::UnexpectedEnd));
}

#[test]
fn writer_len_and_empty() {
    let mut w = Writer::new();
    assert!(w.is_empty());
    assert_eq!(w.len(), 0);

    w.write_u32(1);
    assert!(!w.is_empty());
    assert_eq!(w.len(), 4);
}

#[test]
fn writer_write_bytes() {
    let mut w = Writer::new();
    w.write_bytes(&[0xDE, 0xAD]);
    w.write_bytes(&[0xBE, 0xEF]);

    assert_eq!(w.as_bytes(), &[0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn writer_into_vec() {
    let mut w = Writer::with_capacity(8);
    w.write_u16(0xCAFE);
    let vec = w.into_vec();
    assert_eq!(vec, vec![0xCA, 0xFE]);
}

#[test]
fn mixed_types_roundtrip() {
    let mut w = Writer::new();
    w.write_u8(0x10);
    w.write_u64(0xDEADBEEFCAFEBABE);
    w.write_u64(42);
    w.write_u32(7);
    w.write_u16(999);
    w.write_bytes(&[1, 2, 3]);

    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_u8().unwrap(), 0x10);
    assert_eq!(r.read_u64().unwrap(), 0xDEADBEEFCAFEBABE);
    assert_eq!(r.read_u64().unwrap(), 42);
    assert_eq!(r.read_u32().unwrap(), 7);
    assert_eq!(r.read_u16().unwrap(), 999);
    assert_eq!(r.remaining(), &[1, 2, 3]);
}
