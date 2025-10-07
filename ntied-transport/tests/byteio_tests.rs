use ntied_transport::byteio::{Reader, Writer};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Test writing and reading u8 values
#[test]
fn test_u8_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    writer.write_u8(0);
    writer.write_u8(127);
    writer.write_u8(255);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_u8().unwrap(), 0);
    assert_eq!(reader.read_u8().unwrap(), 127);
    assert_eq!(reader.read_u8().unwrap(), 255);
}

/// Test writing and reading u16 values
#[test]
fn test_u16_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    writer.write_u16(0);
    writer.write_u16(256);
    writer.write_u16(32767);
    writer.write_u16(65535);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_u16().unwrap(), 0);
    assert_eq!(reader.read_u16().unwrap(), 256);
    assert_eq!(reader.read_u16().unwrap(), 32767);
    assert_eq!(reader.read_u16().unwrap(), 65535);
}

/// Test writing and reading u32 values
#[test]
fn test_u32_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    writer.write_u32(0);
    writer.write_u32(65536);
    writer.write_u32(2147483647);
    writer.write_u32(4294967295);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_u32().unwrap(), 0);
    assert_eq!(reader.read_u32().unwrap(), 65536);
    assert_eq!(reader.read_u32().unwrap(), 2147483647);
    assert_eq!(reader.read_u32().unwrap(), 4294967295);
}

/// Test writing and reading byte vectors
#[test]
fn test_bytes_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let empty_bytes = vec![];
    let small_bytes = vec![1, 2, 3, 4, 5];
    let large_bytes = vec![42u8; 1000];

    writer.write_bytes(&empty_bytes);
    writer.write_bytes(&small_bytes);
    writer.write_bytes(&large_bytes);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_bytes().unwrap(), empty_bytes);
    assert_eq!(reader.read_bytes().unwrap(), small_bytes);
    assert_eq!(reader.read_bytes().unwrap(), large_bytes);
}

/// Test writing and reading fixed-size arrays
#[test]
fn test_array_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let array1: [u8; 4] = [10, 20, 30, 40];
    let array2: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let array3: [u8; 12] = [100; 12];

    writer.write_array(&array1);
    writer.write_array(&array2);
    writer.write_array(&array3);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_array::<4>().unwrap(), array1);
    assert_eq!(reader.read_array::<8>().unwrap(), array2);
    assert_eq!(reader.read_array::<12>().unwrap(), array3);
}

/// Test writing and reading strings
#[test]
fn test_string_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let empty_string = "";
    let ascii_string = "Hello, World!";
    let unicode_string = "Привет, мир! 你好世界 🌍";

    writer.write_string(empty_string);
    writer.write_string(ascii_string);
    writer.write_string(unicode_string);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_string().unwrap(), empty_string);
    assert_eq!(reader.read_string().unwrap(), ascii_string);
    assert_eq!(reader.read_string().unwrap(), unicode_string);
}

/// Test writing and reading IPv4 addresses
#[test]
fn test_ipv4_addr_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let ip1 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    let ip3 = IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255));

    writer.write_ip_addr(&ip1);
    writer.write_ip_addr(&ip2);
    writer.write_ip_addr(&ip3);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_ip_addr().unwrap(), ip1);
    assert_eq!(reader.read_ip_addr().unwrap(), ip2);
    assert_eq!(reader.read_ip_addr().unwrap(), ip3);
}

/// Test writing and reading IPv6 addresses
#[test]
fn test_ipv6_addr_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let ip1 = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1));
    let ip2 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    let ip3 = IpAddr::V6(Ipv6Addr::new(
        0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    ));

    writer.write_ip_addr(&ip1);
    writer.write_ip_addr(&ip2);
    writer.write_ip_addr(&ip3);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_ip_addr().unwrap(), ip1);
    assert_eq!(reader.read_ip_addr().unwrap(), ip2);
    assert_eq!(reader.read_ip_addr().unwrap(), ip3);
}

/// Test writing and reading socket addresses
#[test]
fn test_socket_addr_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 443);
    let addr3 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 3000);

    writer.write_socket_addr(&addr1);
    writer.write_socket_addr(&addr2);
    writer.write_socket_addr(&addr3);

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_socket_addr().unwrap(), addr1);
    assert_eq!(reader.read_socket_addr().unwrap(), addr2);
    assert_eq!(reader.read_socket_addr().unwrap(), addr3);
}

/// Test reading u8 from empty data
#[test]
fn test_read_u8_empty_data() {
    let data = vec![];
    let mut reader = Reader::new(&data);
    assert!(reader.read_u8().is_err());
}

/// Test reading u16 from insufficient data
#[test]
fn test_read_u16_insufficient_data() {
    let data = vec![1]; // Only 1 byte, need 2
    let mut reader = Reader::new(&data);
    assert!(reader.read_u16().is_err());
}

/// Test reading u32 from insufficient data
#[test]
fn test_read_u32_insufficient_data() {
    let data = vec![1, 2, 3]; // Only 3 bytes, need 4
    let mut reader = Reader::new(&data);
    assert!(reader.read_u32().is_err());
}

/// Test reading bytes with invalid length
#[test]
fn test_read_bytes_insufficient_data() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);
    writer.write_u16(100); // Write length of 100
    writer.write_u8(42); // But only write 1 byte of actual data

    let mut reader = Reader::new(&data);
    assert!(reader.read_bytes().is_err());
}

/// Test reading array from insufficient data
#[test]
fn test_read_array_insufficient_data() {
    let data = vec![1, 2, 3]; // Only 3 bytes
    let mut reader = Reader::new(&data);
    assert!(reader.read_array::<5>().is_err()); // Try to read 5 bytes
}

/// Test reading string with invalid UTF-8
#[test]
fn test_read_string_invalid_utf8() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);
    writer.write_u16(4); // Length of 4
    data.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]); // Invalid UTF-8 bytes

    let mut reader = Reader::new(&data);
    assert!(reader.read_string().is_err());
}

/// Test reading IP address with invalid version
#[test]
fn test_read_ip_addr_invalid_version() {
    let mut data = Vec::new();
    data.push(7); // Invalid IP version (not 4 or 6)

    let mut reader = Reader::new(&data);
    assert!(reader.read_ip_addr().is_err());
}

/// Test mixed operations roundtrip
#[test]
fn test_mixed_operations_roundtrip() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    writer.write_u8(42);
    writer.write_string("test");
    writer.write_u32(123456);
    writer.write_bytes(&[1, 2, 3]);
    writer.write_array(&[4u8; 6]);
    writer.write_socket_addr(&SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        9999,
    ));

    let mut reader = Reader::new(&data);
    assert_eq!(reader.read_u8().unwrap(), 42);
    assert_eq!(reader.read_string().unwrap(), "test");
    assert_eq!(reader.read_u32().unwrap(), 123456);
    assert_eq!(reader.read_bytes().unwrap(), vec![1, 2, 3]);
    assert_eq!(reader.read_array::<6>().unwrap(), [4u8; 6]);
    assert_eq!(
        reader.read_socket_addr().unwrap(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 9999)
    );
}

/// Test big-endian encoding for u16
#[test]
fn test_u16_big_endian() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);
    writer.write_u16(0x1234);

    assert_eq!(data[0], 0x12); // High byte first
    assert_eq!(data[1], 0x34); // Low byte second
}

/// Test big-endian encoding for u32
#[test]
fn test_u32_big_endian() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);
    writer.write_u32(0x12345678);

    assert_eq!(data[0], 0x12);
    assert_eq!(data[1], 0x34);
    assert_eq!(data[2], 0x56);
    assert_eq!(data[3], 0x78);
}

/// Test maximum size bytes vector
#[test]
fn test_max_size_bytes() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let max_size_bytes = vec![0u8; u16::MAX as usize];
    writer.write_bytes(&max_size_bytes);

    let mut reader = Reader::new(&data);
    let read_bytes = reader.read_bytes().unwrap();
    assert_eq!(read_bytes.len(), u16::MAX as usize);
}

/// Test maximum size string
#[test]
fn test_max_size_string() {
    let mut data = Vec::new();
    let mut writer = Writer::new(&mut data);

    let max_size_string = "A".repeat(u16::MAX as usize);
    writer.write_string(&max_size_string);

    let mut reader = Reader::new(&data);
    let read_string = reader.read_string().unwrap();
    assert_eq!(read_string.len(), u16::MAX as usize);
}
