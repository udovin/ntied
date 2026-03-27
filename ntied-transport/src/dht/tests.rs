use std::net::SocketAddr;

use super::*;
use crate::crypto::PrivateKey;
use crate::wire::{Reader, Writer};

fn test_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], port))
}

fn make_gateway_info(port: u16) -> GatewayInfo {
    let gw_id = PrivateKey::generate().public_key().peer_id();
    GatewayInfo {
        gateway_peer_id: gw_id,
        addrs: vec![test_addr(port)],
        latency_hint: 50,
    }
}

#[test]
fn dht_node_roundtrip() {
    let node = DhtNode {
        peer_id: PrivateKey::generate().public_key().peer_id(),
        addrs: vec![test_addr(3000), test_addr(3001)],
    };
    let mut w = Writer::new();
    node.encode(&mut w);
    let mut r = Reader::new(w.as_bytes());
    let decoded = DhtNode::decode(&mut r).unwrap();
    assert_eq!(decoded.peer_id, node.peer_id);
    assert_eq!(decoded.addrs, node.addrs);
}

#[test]
fn dht_record_sign_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(4000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    assert!(record.verify());
}

#[test]
fn dht_record_encode_decode_roundtrip() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(5000), make_gateway_info(5001)],
        RoutingPolicy::Open,
        42,
        100_000,
        &identity,
    );

    let encoded = record.encode();
    let decoded = DhtRecord::decode(&encoded).unwrap();

    assert!(decoded.verify());
    assert_eq!(decoded.peer_id, record.peer_id);
    assert_eq!(decoded.version, 42);
    assert_eq!(decoded.expires_at, 100_000);
    assert_eq!(decoded.gateways.len(), 2);
}

#[test]
fn dht_record_gateway_restricted_roundtrip() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();
    let gw_id = PrivateKey::generate().public_key().peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(6000)],
        RoutingPolicy::GatewayRestricted(vec![gw_id]),
        1,
        9999,
        &identity,
    );

    let encoded = record.encode();
    let decoded = DhtRecord::decode(&encoded).unwrap();

    assert!(decoded.verify());
    match &decoded.routing_policy {
        RoutingPolicy::GatewayRestricted(gws) => {
            assert_eq!(gws.len(), 1);
            assert_eq!(gws[0], gw_id);
        }
        _ => panic!("expected GatewayRestricted"),
    }
}

#[test]
fn dht_record_tampered_version_fails_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let mut record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(7000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    record.version = 999;
    assert!(!record.verify());
}

#[test]
fn dht_record_wrong_peer_id_fails_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let wrong_peer_id = PrivateKey::generate().public_key().peer_id();

    let record = DhtRecord::sign(
        wrong_peer_id,
        pk,
        vec![make_gateway_info(8000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    assert!(!record.verify());
}
