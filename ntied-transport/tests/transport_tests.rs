use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Once, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use ntied_transport::crypto::PeerId;
use ntied_transport::{Discovery, HashMapDiscovery, NetworkConfig, Node, PrivateKey, RouteInfo};

static TRACING_INIT: Once = Once::new();

fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_target(false)
            .with_test_writer()
            .init();
    });
}

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
async fn accept_does_not_return_initiator_connection() {
    let discovery = Arc::new(HashMapDiscovery::new());

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let node_a = Node::bind(localhost(), id_a, &discovery).await.unwrap();
    let node_b = Node::bind(localhost(), id_b, &discovery).await.unwrap();
    let b_addr = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let _conn_a = node_a.connect_addr(b_addr).await.unwrap();
    let _conn_b = accept.await.unwrap();

    let stale = tokio::time::timeout(Duration::from_millis(200), node_a.accept()).await;
    assert!(
        stale.is_err(),
        "accept() must not return initiator connections"
    );
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
    gw_node.enable_gateway().await;
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

    let accept = tokio::spawn(async move { node_b.accept().await });
    let conn_a = node_a.connect(&peer_id_b).await.unwrap();
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

#[tokio::test]
async fn relay_accept_does_not_return_initiator() {
    let gw_identity = PrivateKey::generate();
    let gw_discovery = Arc::new(HashMapDiscovery::new());
    let gw_node = Node::bind(localhost(), gw_identity, &gw_discovery)
        .await
        .unwrap();
    gw_node.enable_gateway().await;
    let gw_addr = gw_node.local_addr().unwrap();

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let peer_id_a = id_a.public_key().peer_id();

    let disc_a = Arc::new(RelayDiscovery::new());
    disc_a.add_relayed(peer_id_b);
    let disc_b = Arc::new(RelayDiscovery::new());
    disc_b.add_relayed(peer_id_a);

    let node_a = Node::bind(localhost(), id_a, &disc_a).await.unwrap();
    let node_b = Node::bind(localhost(), id_b, &disc_b).await.unwrap();

    node_a
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();
    node_b
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let _conn_a = node_a.connect(&peer_id_b).await.unwrap();
    let _conn_b = accept.await.unwrap();

    let stale = tokio::time::timeout(Duration::from_millis(200), node_a.accept()).await;
    assert!(
        stale.is_err(),
        "relay initiator must not see E2E or gateway sessions in accept queue"
    );
}

#[tokio::test]
async fn dht_discovery_resolve_and_connect() {
    // Gateway node
    let gw_identity = PrivateKey::generate();
    let gw_discovery = Arc::new(HashMapDiscovery::new());
    let gw_node = Node::bind(localhost(), gw_identity, &gw_discovery)
        .await
        .unwrap();
    gw_node.enable_gateway().await;
    let gw_addr = gw_node.local_addr().unwrap();

    // Peer A — registers and publishes DHT record via join_network
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let disc_a = Arc::new(HashMapDiscovery::new());
    let node_a = Node::bind(localhost(), id_a, &disc_a).await.unwrap();

    node_a
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    // Small delay to ensure DhtPublish is processed by gateway
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Peer B — joins the same gateway, then uses DhtDiscovery to find Peer A
    let id_b = PrivateKey::generate();
    let disc_b = Arc::new(HashMapDiscovery::new());
    let node_b = Node::bind(localhost(), id_b, &disc_b).await.unwrap();

    node_b
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    // Use DhtDiscovery to resolve peer A
    let dht_disc = node_b.dht_discovery();
    let route = dht_disc.resolve(&peer_id_a).await;
    assert!(
        route.is_some(),
        "DhtDiscovery should resolve peer A's record"
    );
    let route = route.unwrap();
    match &route {
        RouteInfo::Relayed { gateway_addr } => {
            assert_eq!(*gateway_addr, gw_addr);
        }
        RouteInfo::Direct(_) => panic!("expected Relayed route"),
    }

    // Now connect B → A through the relay using DHT-resolved route
    let accept = tokio::spawn(async move { node_a.accept().await.unwrap() });

    // B connects via relay (DhtDiscovery returned Relayed, so connect_via_relay is used)
    let conn_b = node_b.connect(&peer_id_a).await.unwrap();
    let conn_a = accept.await.unwrap();

    assert!(conn_b.is_established().await);
    assert!(conn_a.is_established().await);

    // Verify data flows
    let sb = conn_b.open_stream(42).await.unwrap();
    sb.send(b"hello via dht").await.unwrap();

    let (sa, purpose) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sa.recv().await.unwrap();
    assert_eq!(data, b"hello via dht");
}

#[tokio::test]
async fn cross_gateway_two_gw_connect() {
    // 2 gateways, 1 client on each, test cross-GW connection
    let gw1_id = PrivateKey::generate();
    let gw1_disc = Arc::new(HashMapDiscovery::new());
    let gw1 = Node::bind(localhost(), gw1_id, &gw1_disc).await.unwrap();
    gw1.enable_gateway().await;
    let gw1_addr = gw1.local_addr().unwrap();

    let gw2_id = PrivateKey::generate();
    let gw2_disc = Arc::new(HashMapDiscovery::new());
    let gw2 = Node::bind(localhost(), gw2_id, &gw2_disc).await.unwrap();
    gw2.enable_gateway().await;
    let gw2_addr = gw2.local_addr().unwrap();

    // Peer the gateways
    gw1.add_gateway_peer(gw2_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Client A on GW1
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let disc_a = Arc::new(HashMapDiscovery::new());
    let node_a = Node::bind(localhost(), id_a, &disc_a).await.unwrap();
    node_a
        .join_network(NetworkConfig {
            bootstrap: vec![gw1_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    // Client B on GW2
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let disc_b = Arc::new(HashMapDiscovery::new());
    let node_b = Node::bind(localhost(), id_b, &disc_b).await.unwrap();
    node_b
        .join_network(NetworkConfig {
            bootstrap: vec![gw2_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    // Wait for DHT records to propagate across gateways
    tokio::time::sleep(Duration::from_millis(500)).await;

    // B connects to A (cross-GW)
    let accept = tokio::spawn(async move { node_a.accept().await.unwrap() });
    let conn_b = node_b.connect(&peer_id_a).await.unwrap();
    let conn_a = accept.await.unwrap();

    assert!(conn_b.is_established().await);
    assert!(conn_a.is_established().await);

    let sb = conn_b.open_stream(1).await.unwrap();
    sb.send(b"cross-gw hello").await.unwrap();

    let (sa, _) = conn_a.accept_stream().await.unwrap();
    let data = sa.recv().await.unwrap();
    assert_eq!(data, b"cross-gw hello");
}

#[tokio::test]
async fn two_sequential_cross_gw_connections() {
    init_tracing();
    // Minimal repro: 2 GWs, 2 sequential cross-GW connections
    // First connection works, second times out.
    let gw0 = Node::bind(localhost(), PrivateKey::generate(), &Arc::new(HashMapDiscovery::new()))
        .await.unwrap();
    gw0.enable_gateway().await;
    let gw0_addr = gw0.local_addr().unwrap();

    let gw1 = Node::bind(localhost(), PrivateKey::generate(), &Arc::new(HashMapDiscovery::new()))
        .await.unwrap();
    gw1.enable_gateway().await;
    let gw1_addr = gw1.local_addr().unwrap();

    gw0.add_gateway_peer(gw1_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    eprintln!("GW0 = {:?}", gw0.peer_id());
    eprintln!("GW1 = {:?}", gw1.peer_id());

    // Client A on GW0
    let id_a = PrivateKey::generate();
    let pid_a = id_a.public_key().peer_id();
    eprintln!("Client A = {:?}", pid_a);
    let node_a = Node::bind(localhost(), id_a, &Arc::new(HashMapDiscovery::new()))
        .await.unwrap();
    node_a.join_network(NetworkConfig { bootstrap: vec![gw0_addr], preferred_gateway: None })
        .await.unwrap();

    // Client B on GW0 (same GW as A)
    let id_b = PrivateKey::generate();
    let pid_b = id_b.public_key().peer_id();
    eprintln!("Client B = {:?}", pid_b);
    let node_b = Node::bind(localhost(), id_b, &Arc::new(HashMapDiscovery::new()))
        .await.unwrap();
    node_b.join_network(NetworkConfig { bootstrap: vec![gw0_addr], preferred_gateway: None })
        .await.unwrap();

    // Client C on GW1
    let id_c = PrivateKey::generate();
    let pid_c = id_c.public_key().peer_id();
    eprintln!("Client C = {:?}", pid_c);
    let node_c = Node::bind(localhost(), id_c, &Arc::new(HashMapDiscovery::new()))
        .await.unwrap();
    node_c.join_network(NetworkConfig { bootstrap: vec![gw1_addr], preferred_gateway: None })
        .await.unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    // Connection 1: A→C (cross-GW) — should work
    struct S(Node);
    let nodes: &'static [S] = Vec::leak(vec![S(node_a), S(node_b), S(node_c)]);
    let pids = [pid_a, pid_b, pid_c];

    {
        let acc = tokio::spawn(async move { nodes[2].0.accept().await.unwrap() });
        let conn = nodes[0].0.connect(&pids[2]).await.expect("A→C failed");
        let _ = acc.await.unwrap();
        assert!(conn.is_established().await);
        eprintln!("A→C OK");
        std::mem::forget(conn);
    }

    // Connection 2: B→C (cross-GW, same GW pair) — this one fails
    {
        let acc = tokio::spawn(async move { nodes[2].0.accept().await.unwrap() });
        let conn = nodes[1].0.connect(&pids[2]).await.expect("B→C failed");
        let _ = acc.await.unwrap();
        assert!(conn.is_established().await);
        eprintln!("B→C OK");
        std::mem::forget(conn);
    }
}

#[tokio::test]
async fn three_gw_all_pairs() {
    init_tracing();
    // 3 gateways, 1 client each, 3 cross-GW connections
    let mut gw_nodes = Vec::new();
    let mut gw_addrs = Vec::new();
    for _ in 0..3 {
        let gw = Node::bind(localhost(), PrivateKey::generate(), &Arc::new(HashMapDiscovery::new()))
            .await.unwrap();
        gw.enable_gateway().await;
        gw_addrs.push(gw.local_addr().unwrap());
        gw_nodes.push(gw);
    }
    // Peer mesh
    for i in 1..3 {
        for j in 0..i {
            gw_nodes[i].add_gateway_peer(gw_addrs[j]).await.unwrap();
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3 clients
    struct C { node: Node, pid: PeerId }
    let mut clients = Vec::new();
    for gw_idx in 0..3 {
        let id = PrivateKey::generate();
        let pid = id.public_key().peer_id();
        let node = Node::bind(localhost(), id, &Arc::new(HashMapDiscovery::new()))
            .await.unwrap();
        node.join_network(NetworkConfig { bootstrap: vec![gw_addrs[gw_idx]], preferred_gateway: None })
            .await.unwrap();
        clients.push(C { node, pid });
    }
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let clients: &'static [C] = Vec::leak(clients);

    // Connect each pair sequentially
    for (i, j, label) in [(0,1,"0->1"), (0,2,"0->2"), (1,2,"1->2")] {
        let accept = tokio::spawn(async move { clients[j].node.accept().await.unwrap() });
        let conn_i = clients[i].node.connect(&clients[j].pid).await
            .unwrap_or_else(|e| panic!("{label} connect failed: {e}"));
        let conn_j = accept.await.unwrap();
        assert!(conn_i.is_established().await, "{label} not established");
        eprintln!("{label} OK");
        // Keep connections alive to avoid ConnectionClose traffic
        std::mem::forget(conn_i);
        std::mem::forget(conn_j);
    }
}

#[tokio::test]
async fn multi_gateway_mesh_full_connectivity() {
    init_tracing();
    const NUM_GW: usize = 3;
    const CLIENTS_PER_GW: usize = 1;
    const NUM_CLIENTS: usize = NUM_GW * CLIENTS_PER_GW;

    // --- Create 8 gateways ---
    let mut gw_nodes = Vec::new();
    let mut gw_addrs = Vec::new();
    for _ in 0..NUM_GW {
        let gw_id = PrivateKey::generate();
        let gw_disc = Arc::new(HashMapDiscovery::new());
        let gw = Node::bind(localhost(), gw_id, &gw_disc).await.unwrap();
        gw.enable_gateway().await;
        gw_addrs.push(gw.local_addr().unwrap());
        gw_nodes.push(gw);
    }

    // --- Peer gateways in a mesh (each GW connects to all previous) ---
    for i in 1..NUM_GW {
        for j in 0..i {
            gw_nodes[i].add_gateway_peer(gw_addrs[j]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Small delay for DHT tables to settle
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- Create 16 clients (2 per GW), join their gateway ---
    struct ClientInfo {
        node: Node,
        peer_id: PeerId,
    }

    let mut clients: Vec<ClientInfo> = Vec::new();
    for gw_idx in 0..NUM_GW {
        for _ in 0..CLIENTS_PER_GW {
            let id = PrivateKey::generate();
            let peer_id = id.public_key().peer_id();
            let disc = Arc::new(HashMapDiscovery::new());
            let node = Node::bind(localhost(), id, &disc).await.unwrap();
            node.join_network(NetworkConfig {
                bootstrap: vec![gw_addrs[gw_idx]],
                preferred_gateway: None,
            })
            .await
            .unwrap();
            clients.push(ClientInfo { node, peer_id });
        }
        // Let the gateway process before next batch
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Wait for DHT records to propagate across all gateways
    tokio::time::sleep(Duration::from_millis(5000)).await;

    // Leak clients so we can use 'static references in spawn
    let clients: &'static [ClientInfo] = Vec::leak(clients);

    // --- Each client connects to every other client ---
    let expected_connections = NUM_CLIENTS * (NUM_CLIENTS - 1) / 2;
    let mut total_established = 0usize;

    for i in 0..NUM_CLIENTS {
        for j in (i + 1)..NUM_CLIENTS {
            let accept_handle = tokio::spawn(async move {
                clients[j].node.accept().await
            });

            let target_pid = clients[j].peer_id;
            let connect_handle = tokio::spawn(async move {
                clients[i].node.connect(&target_pid).await
            });

            let conn_i = connect_handle.await.unwrap().unwrap_or_else(|e| {
                panic!("connect {i}->{j} failed: {e}")
            });
            assert!(conn_i.is_established().await, "{i}->{j} not established");

            let si = conn_i.open_stream(1).await.unwrap();
            si.send(format!("{i}->{j}").as_bytes()).await.unwrap();
            std::mem::forget(conn_i);

            let conn_j = accept_handle.await.unwrap().unwrap_or_else(|e| {
                panic!("accept at {j} from {i} failed: {e}")
            });
            let (sj, _) = conn_j.accept_stream().await.unwrap();
            let data = sj.recv().await.unwrap();
            assert_eq!(String::from_utf8(data).unwrap(), format!("{i}->{j}"));
            std::mem::forget(conn_j);

            total_established += 1;
        }
    }

    assert_eq!(total_established, expected_connections);
    eprintln!(
        "mesh test: {NUM_GW} gateways, {NUM_CLIENTS} clients, {expected_connections} connections OK"
    );
}
