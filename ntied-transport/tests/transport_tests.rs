use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use ntied_transport::crypto::PeerId;
use ntied_transport::{Discovery, HashMapDiscovery, NetworkConfig, Node, PrivateKey, RouteInfo};

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

#[tokio::test]
async fn bind_auto_registers() {
    let discovery = Arc::new(HashMapDiscovery::new());
    let identity = PrivateKey::generate();
    let peer_id = identity.public_key().peer_id();

    let node = Node::bind(localhost(), identity, &discovery).await.unwrap();

    let local_addr = node.local_addr().unwrap();
    let route = discovery.resolve(&peer_id).await;
    assert_eq!(route, Some(ntied_transport::RouteInfo::Direct(local_addr)));
}

#[tokio::test]
async fn connect_unknown_peer_fails() {
    let discovery = Arc::new(HashMapDiscovery::new());
    let identity = PrivateKey::generate();

    let node = Node::bind(localhost(), identity, &discovery).await.unwrap();

    let unknown = PrivateKey::generate().public_key().peer_id();
    match node.connect(&unknown).await {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        Ok(_) => panic!("expected NotFound error for unknown peer"),
    }
}

#[tokio::test]
async fn two_transports_handshake() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let t_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();
    let t_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();

    let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await });
    let accept = tokio::spawn(async move { t_b.accept().await });

    let conn_a = connect.await.unwrap().unwrap();
    let conn_b = accept.await.unwrap().unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);
}

#[tokio::test]
async fn stream_over_discovery() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let t_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();
    let t_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();

    let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
    let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

    let conn_a = connect.await.unwrap();
    let conn_b = accept.await.unwrap();

    let stream_a = conn_a.open_stream(42).await.unwrap();
    stream_a.send(b"hello via discovery").await.unwrap();

    let (stream_b, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);

    let data = stream_b.recv().await.unwrap();
    assert_eq!(data, b"hello via discovery");
}

#[tokio::test]
async fn bidirectional_streams_over_discovery() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let t_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();
    let t_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();

    let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
    let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

    let conn_a = connect.await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"ping").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 1);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"ping");

    let sb2 = conn_b.open_stream(2).await.unwrap();
    sb2.send(b"pong").await.unwrap();

    let (sa2, purpose) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose, 2);

    let data = sa2.recv().await.unwrap();
    assert_eq!(data, b"pong");
}

#[tokio::test]
async fn multi_message_exchange() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let t_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();
    let t_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();

    let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
    let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

    let conn_a = Arc::new(connect.await.unwrap());
    let conn_b = Arc::new(accept.await.unwrap());

    let sa = conn_a.open_stream(10).await.unwrap();
    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 10);

    let mut expected = Vec::new();
    for i in 0..10u32 {
        let msg = format!("message-{i}");
        sa.send(msg.as_bytes()).await.unwrap();
        expected.extend_from_slice(msg.as_bytes());
    }

    let mut received = Vec::new();
    while received.len() < expected.len() {
        let chunk = sb.recv().await.unwrap();
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, expected);

    let sb_out = conn_b.open_stream(20).await.unwrap();
    let (sa_in, purpose) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose, 20);

    let large = vec![0xCDu8; 4000];
    sb_out.send(&large).await.unwrap();

    let mut received = Vec::new();
    while received.len() < large.len() {
        let chunk = sa_in.recv().await.unwrap();
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received.len(), 4000);
    assert!(received.iter().all(|&b| b == 0xCD));
}

#[tokio::test]
async fn connect_addr_handshake_and_stream() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let node_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();
    let b_addr = node_b.local_addr().unwrap();

    let node_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();

    let connect = tokio::spawn(async move { node_a.connect_addr(b_addr).await.unwrap() });
    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });

    let conn_a = connect.await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);

    let sa = conn_a.open_stream(99).await.unwrap();
    sa.send(b"via connect_addr").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 99);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"via connect_addr");
}

struct RelayDiscovery {
    relayed_peers: RwLock<HashSet<PeerId>>,
}

impl RelayDiscovery {
    fn new() -> Self {
        Self {
            relayed_peers: RwLock::new(HashSet::new()),
        }
    }

    fn add_relayed(&self, peer_id: PeerId) {
        self.relayed_peers.write().unwrap().insert(peer_id);
    }
}

#[async_trait]
impl Discovery for RelayDiscovery {
    async fn resolve(&self, peer_id: &PeerId) -> Option<RouteInfo> {
        if self.relayed_peers.read().unwrap().contains(peer_id) {
            Some(RouteInfo::Relayed {
                gateway_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            })
        } else {
            None
        }
    }

    async fn register(&self, _peer_id: PeerId, _addr: SocketAddr) {}
}

#[tokio::test]
async fn relay_through_gateway() {
    let gw_identity = PrivateKey::generate();
    let gw_discovery = Arc::new(HashMapDiscovery::new());
    let gw_node = Node::bind(localhost(), gw_identity, &gw_discovery)
        .await
        .unwrap();
    gw_node.enable_gateway();
    let gw_addr = gw_node.local_addr().unwrap();

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let peer_id_b = id_b.public_key().peer_id();

    let disc_a = Arc::new(RelayDiscovery::new());
    disc_a.add_relayed(peer_id_b);
    let disc_b = Arc::new(RelayDiscovery::new());
    disc_b.add_relayed(peer_id_a);

    let node_a = Node::bind(localhost(), id_a, &disc_a).await.unwrap();
    let node_b = Node::bind(localhost(), id_b, &disc_b).await.unwrap();

    let config_a = NetworkConfig {
        bootstrap: vec![gw_addr],
        preferred_gateway: None,
    };
    let config_b = NetworkConfig {
        bootstrap: vec![gw_addr],
        preferred_gateway: None,
    };

    node_a.join_network(config_a).await.unwrap();
    node_b.join_network(config_b).await.unwrap();

    let connect = tokio::spawn(async move { node_a.connect(&peer_id_b).await });
    let accept = tokio::spawn(async move { node_b.accept().await });

    let conn_a = connect.await.unwrap().unwrap();
    let conn_b = accept.await.unwrap().unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);

    let sa = conn_a.open_stream(7).await.unwrap();
    sa.send(b"hello via relay").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 7);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello via relay");

    let sb2 = conn_b.open_stream(8).await.unwrap();
    sb2.send(b"reply via relay").await.unwrap();

    let (sa2, purpose) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose, 8);
    let data = sa2.recv().await.unwrap();
    assert_eq!(data, b"reply via relay");
}
