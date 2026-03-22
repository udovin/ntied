use std::net::SocketAddr;

use crate::v2::crypto::PrivateKey;
use crate::v2::discovery::{Discovery, HashMapDiscovery};

fn localhost(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn make_peer_id() -> crate::v2::crypto::PeerId {
    PrivateKey::generate().public_key().peer_id()
}

#[tokio::test]
async fn resolve_unknown_returns_none() {
    let discovery = HashMapDiscovery::new();
    let peer_id = make_peer_id();
    assert!(discovery.resolve(&peer_id).await.is_none());
}

#[tokio::test]
async fn register_then_resolve() {
    let discovery = HashMapDiscovery::new();
    let peer_id = make_peer_id();
    let addr = localhost(9000);

    discovery.register(peer_id, addr).await;
    assert_eq!(discovery.resolve(&peer_id).await, Some(addr));
}

#[tokio::test]
async fn register_overwrites() {
    let discovery = HashMapDiscovery::new();
    let peer_id = make_peer_id();

    discovery.register(peer_id, localhost(9000)).await;
    discovery.register(peer_id, localhost(9001)).await;

    assert_eq!(discovery.resolve(&peer_id).await, Some(localhost(9001)));
}

#[tokio::test]
async fn multiple_peers() {
    let discovery = HashMapDiscovery::new();
    let id_a = make_peer_id();
    let id_b = make_peer_id();

    discovery.register(id_a, localhost(1000)).await;
    discovery.register(id_b, localhost(2000)).await;

    assert_eq!(discovery.resolve(&id_a).await, Some(localhost(1000)));
    assert_eq!(discovery.resolve(&id_b).await, Some(localhost(2000)));
}
