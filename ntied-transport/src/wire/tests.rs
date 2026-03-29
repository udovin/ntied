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

#[test]
fn socket_addr_ipv4_roundtrip() {
    use std::net::SocketAddr;
    let addr: SocketAddr = "192.168.1.1:8080".parse().unwrap();
    let mut w = Writer::new();
    w.write_socket_addr(&addr);
    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_socket_addr().unwrap(), addr);
    assert!(r.is_empty());
}

#[test]
fn socket_addr_ipv6_roundtrip() {
    use std::net::SocketAddr;
    let addr: SocketAddr = "[::1]:9090".parse().unwrap();
    let mut w = Writer::new();
    w.write_socket_addr(&addr);
    let mut r = Reader::new(w.as_bytes());
    assert_eq!(r.read_socket_addr().unwrap(), addr);
    assert!(r.is_empty());
}

#[test]
fn frame_gateway_register_roundtrip() {
    use crate::crypto::PrivateKey;
    let peer_id = PrivateKey::generate().public_key().peer_id();
    let frame = Frame::GatewayRegister(GatewayRegister {
        peer_id,
        flags: 0x0042,
        auth_data: vec![0xAA, 0xBB, 0xCC],
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::GatewayRegister(f) => {
            assert_eq!(f.peer_id, peer_id);
            assert_eq!(f.flags, 0x0042);
            assert_eq!(f.auth_data, vec![0xAA, 0xBB, 0xCC]);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_gateway_register_ack_roundtrip() {
    let frame = Frame::GatewayRegisterAck(GatewayRegisterAck {
        status: 0,
        relay_mtu: 1281,
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::GatewayRegisterAck(f) => {
            assert_eq!(f.status, 0);
            assert_eq!(f.relay_mtu, 1281);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_gateway_packet_roundtrip() {
    use crate::crypto::PrivateKey;
    let dest = PrivateKey::generate().public_key().peer_id();
    let src = PrivateKey::generate().public_key().peer_id();
    let inner = vec![0x10, 0x20, 0x30, 0x40, 0x50];
    let frame = Frame::GatewayPacket(GatewayPacket {
        dest_peer_id: dest,
        src_peer_id: src,
        inner: inner.clone(),
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::GatewayPacket(f) => {
            assert_eq!(f.dest_peer_id, dest);
            assert_eq!(f.src_peer_id, src);
            assert_eq!(f.inner, inner);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_hole_punch_notify_roundtrip() {
    use crate::crypto::PrivateKey;
    use std::net::SocketAddr;
    let requester = PrivateKey::generate().public_key().peer_id();
    let addrs: Vec<SocketAddr> = vec![
        "1.2.3.4:5000".parse().unwrap(),
        "[::1]:6000".parse().unwrap(),
    ];
    let frame = Frame::HolePunchNotify(HolePunchNotify {
        requester_peer_id: requester,
        addrs: addrs.clone(),
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::HolePunchNotify(f) => {
            assert_eq!(f.requester_peer_id, requester);
            assert_eq!(f.addrs, addrs);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_gateway_packet_with_ttl_roundtrip() {
    use crate::crypto::PrivateKey;
    let dest = PrivateKey::generate().public_key().peer_id();
    let src = PrivateKey::generate().public_key().peer_id();
    let inner = vec![0xDE, 0xAD];
    let frame = Frame::GatewayPacket(GatewayPacket {
        dest_peer_id: dest,
        src_peer_id: src,
        inner: inner.clone(),
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::GatewayPacket(f) => {
            assert_eq!(f.dest_peer_id, dest);
            assert_eq!(f.src_peer_id, src);
            assert_eq!(f.inner, inner);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_dht_find_node_roundtrip() {
    use crate::crypto::PrivateKey;
    let target = PrivateKey::generate().public_key().peer_id();
    let frame = Frame::DhtFindNode(DhtFindNode {
        target,
        request_id: 42,
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::DhtFindNode(f) => {
            assert_eq!(f.target, target);
            assert_eq!(f.request_id, 42);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_dht_find_node_reply_roundtrip() {
    use crate::crypto::PrivateKey;
    use crate::dht::DhtNode;
    use std::net::SocketAddr;
    let node1 = DhtNode {
        peer_id: PrivateKey::generate().public_key().peer_id(),
        addrs: vec!["10.0.0.1:3000".parse::<SocketAddr>().unwrap()],
    };
    let node2 = DhtNode {
        peer_id: PrivateKey::generate().public_key().peer_id(),
        addrs: vec![
            "10.0.0.2:4000".parse::<SocketAddr>().unwrap(),
            "[::1]:5000".parse::<SocketAddr>().unwrap(),
        ],
    };
    let frame = Frame::DhtFindNodeReply(DhtFindNodeReply {
        request_id: 99,
        nodes: vec![node1.clone(), node2.clone()],
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::DhtFindNodeReply(f) => {
            assert_eq!(f.request_id, 99);
            assert_eq!(f.nodes.len(), 2);
            assert_eq!(f.nodes[0].peer_id, node1.peer_id);
            assert_eq!(f.nodes[0].addrs, node1.addrs);
            assert_eq!(f.nodes[1].addrs.len(), 2);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_dht_publish_roundtrip() {
    let data = vec![0x01; 500];
    let frame = Frame::DhtPublish(DhtPublish {
        fragment_index: 0,
        fragment_total: 3,
        data: data.clone(),
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::DhtPublish(f) => {
            assert_eq!(f.fragment_index, 0);
            assert_eq!(f.fragment_total, 3);
            assert_eq!(f.data, data);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn frame_dht_query_reply_roundtrip() {
    let data = vec![0xFE; 200];
    let frame = Frame::DhtQueryReply(DhtQueryReply {
        request_id: 77,
        status: 1,
        fragment_index: 2,
        fragment_total: 5,
        data: data.clone(),
    });
    let encoded = encode_frames(&[frame]);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    match &decoded[0] {
        Frame::DhtQueryReply(f) => {
            assert_eq!(f.request_id, 77);
            assert_eq!(f.status, 1);
            assert_eq!(f.fragment_index, 2);
            assert_eq!(f.fragment_total, 5);
            assert_eq!(f.data, data);
        }
        _ => panic!("wrong frame type"),
    }
}

#[test]
fn multiple_gateway_frames_in_one_payload() {
    use crate::crypto::PrivateKey;
    let peer_a = PrivateKey::generate().public_key().peer_id();
    let peer_b = PrivateKey::generate().public_key().peer_id();
    let frames = vec![
        Frame::GatewayPacket(GatewayPacket {
            dest_peer_id: peer_a,
            src_peer_id: peer_b,
            inner: vec![1, 2, 3],
        }),
        Frame::GatewayPacket(GatewayPacket {
            dest_peer_id: peer_b,
            src_peer_id: peer_a,
            inner: vec![4, 5],
        }),
        Frame::Ping(Ping { ping_id: 42 }),
    ];
    let encoded = encode_frames(&frames);
    let decoded = decode_frames(&encoded).unwrap();
    assert_eq!(decoded.len(), 3);
    assert!(matches!(&decoded[0], Frame::GatewayPacket(_)));
    assert!(matches!(&decoded[1], Frame::GatewayPacket(_)));
    assert!(matches!(&decoded[2], Frame::Ping(_)));
}
