use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;

use ntied_transport::{NetworkConfig, Node, PrivateKey, RouteInfo};

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
async fn connect_unknown_peer_fails() {
    let identity = PrivateKey::generate();
    let node = Node::bind(localhost(), identity).await.unwrap();

    let unknown = PrivateKey::generate().public_key().peer_id();
    match node.connect(&unknown).await {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        Ok(_) => panic!("expected NotFound error for unknown peer"),
    }
}

#[tokio::test]
async fn two_transports_handshake() {
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let t_a = Node::bind(localhost(), id_a).await.unwrap();
    let t_b = Node::bind(localhost(), id_b).await.unwrap();
    let b_addr = t_b.local_addr().unwrap();

    let connect = tokio::spawn(async move { t_a.connect_addr(b_addr).await });
    let accept = tokio::spawn(async move { t_b.accept().await });

    let conn_a = connect.await.unwrap().unwrap();
    let conn_b = accept.await.unwrap().unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);
}

#[tokio::test]
async fn stream_over_connect_addr() {
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let t_a = Node::bind(localhost(), id_a).await.unwrap();
    let t_b = Node::bind(localhost(), id_b).await.unwrap();
    let b_addr = t_b.local_addr().unwrap();

    let connect = tokio::spawn(async move { t_a.connect_addr(b_addr).await.unwrap() });
    let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

    let conn_a = connect.await.unwrap();
    let conn_b = accept.await.unwrap();

    let stream_a = conn_a.open_stream(42).await.unwrap();
    stream_a.send(b"hello direct").await.unwrap();

    let (stream_b, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);

    let data = stream_b.recv().await.unwrap();
    assert_eq!(data, b"hello direct");
}

#[tokio::test]
async fn bidirectional_streams() {
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let t_a = Node::bind(localhost(), id_a).await.unwrap();
    let t_b = Node::bind(localhost(), id_b).await.unwrap();
    let b_addr = t_b.local_addr().unwrap();

    let connect = tokio::spawn(async move { t_a.connect_addr(b_addr).await.unwrap() });
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
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let t_a = Node::bind(localhost(), id_a).await.unwrap();
    let t_b = Node::bind(localhost(), id_b).await.unwrap();
    let b_addr = t_b.local_addr().unwrap();

    let connect = tokio::spawn(async move { t_a.connect_addr(b_addr).await.unwrap() });
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
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
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
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();

    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    let b_addr = node_b.local_addr().unwrap();

    let node_a = Node::bind(localhost(), id_a).await.unwrap();

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

#[tokio::test]
async fn relay_through_gateway() {
    let gw_node = Node::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    gw_node.enable_gateway().await;
    let gw_addr = gw_node.local_addr().unwrap();

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();

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

    tokio::time::sleep(Duration::from_millis(100)).await;

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
    let gw_node = Node::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    gw_node.enable_gateway().await;
    let gw_addr = gw_node.local_addr().unwrap();

    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();

    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();

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

    tokio::time::sleep(Duration::from_millis(100)).await;

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
    let gw_node = Node::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    gw_node.enable_gateway().await;
    let gw_addr = gw_node.local_addr().unwrap();

    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();

    node_a
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let id_b = PrivateKey::generate();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();

    node_b
        .join_network(NetworkConfig {
            bootstrap: vec![gw_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

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

    let accept = tokio::spawn(async move { node_a.accept().await.unwrap() });
    let conn_b = node_b.connect(&peer_id_a).await.unwrap();
    let conn_a = accept.await.unwrap();

    assert!(conn_b.is_established().await);
    assert!(conn_a.is_established().await);

    let sb = conn_b.open_stream(42).await.unwrap();
    sb.send(b"hello via dht").await.unwrap();

    let (sa, purpose) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sa.recv().await.unwrap();
    assert_eq!(data, b"hello via dht");
}

#[tokio::test]
async fn cross_gateway_two_gw_connect() {
    let gw1 = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    gw1.enable_gateway().await;
    let gw1_addr = gw1.local_addr().unwrap();

    let gw2 = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    gw2.enable_gateway().await;
    let gw2_addr = gw2.local_addr().unwrap();

    gw1.add_gateway_peer(gw2_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a
        .join_network(NetworkConfig {
            bootstrap: vec![gw1_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    let id_b = PrivateKey::generate();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    node_b
        .join_network(NetworkConfig {
            bootstrap: vec![gw2_addr],
            preferred_gateway: None,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

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
    let gw0 = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    gw0.enable_gateway().await;
    let gw0_addr = gw0.local_addr().unwrap();

    let gw1 = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    gw1.enable_gateway().await;
    let gw1_addr = gw1.local_addr().unwrap();

    gw0.add_gateway_peer(gw1_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let id_a = PrivateKey::generate();
    let pid_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.join_network(NetworkConfig { bootstrap: vec![gw0_addr], preferred_gateway: None })
        .await.unwrap();

    let id_b = PrivateKey::generate();
    let pid_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    node_b.join_network(NetworkConfig { bootstrap: vec![gw0_addr], preferred_gateway: None })
        .await.unwrap();

    let id_c = PrivateKey::generate();
    let pid_c = id_c.public_key().peer_id();
    let node_c = Node::bind(localhost(), id_c).await.unwrap();
    node_c.join_network(NetworkConfig { bootstrap: vec![gw1_addr], preferred_gateway: None })
        .await.unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;

    struct S(Node);
    let nodes: &'static [S] = Vec::leak(vec![S(node_a), S(node_b), S(node_c)]);
    let pids = [pid_a, pid_b, pid_c];

    {
        let acc = tokio::spawn(async move { nodes[2].0.accept().await.unwrap() });
        let conn = nodes[0].0.connect(&pids[2]).await.expect("A→C failed");
        let _ = acc.await.unwrap();
        assert!(conn.is_established().await);
        std::mem::forget(conn);
    }

    {
        let acc = tokio::spawn(async move { nodes[2].0.accept().await.unwrap() });
        let conn = nodes[1].0.connect(&pids[2]).await.expect("B→C failed");
        let _ = acc.await.unwrap();
        assert!(conn.is_established().await);
        std::mem::forget(conn);
    }
}

#[tokio::test]
async fn three_gw_all_pairs() {
    init_tracing();
    let mut gw_nodes = Vec::new();
    let mut gw_addrs = Vec::new();
    for _ in 0..3 {
        let gw = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        gw.enable_gateway().await;
        gw_addrs.push(gw.local_addr().unwrap());
        gw_nodes.push(gw);
    }
    for i in 1..3 {
        for j in 0..i {
            gw_nodes[i].add_gateway_peer(gw_addrs[j]).await.unwrap();
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    struct C { node: Node, pid: ntied_transport::crypto::PeerId }
    let mut clients = Vec::new();
    for gw_idx in 0..3 {
        let id = PrivateKey::generate();
        let pid = id.public_key().peer_id();
        let node = Node::bind(localhost(), id).await.unwrap();
        node.join_network(NetworkConfig { bootstrap: vec![gw_addrs[gw_idx]], preferred_gateway: None })
            .await.unwrap();
        clients.push(C { node, pid });
    }
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let clients: &'static [C] = Vec::leak(clients);

    for (i, j, label) in [(0,1,"0->1"), (0,2,"0->2"), (1,2,"1->2")] {
        let accept = tokio::spawn(async move { clients[j].node.accept().await.unwrap() });
        let conn_i = clients[i].node.connect(&clients[j].pid).await
            .unwrap_or_else(|e| panic!("{label} connect failed: {e}"));
        let conn_j = accept.await.unwrap();
        assert!(conn_i.is_established().await, "{label} not established");
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

    let mut gw_nodes = Vec::new();
    let mut gw_addrs = Vec::new();
    for _ in 0..NUM_GW {
        let gw = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        gw.enable_gateway().await;
        gw_addrs.push(gw.local_addr().unwrap());
        gw_nodes.push(gw);
    }

    for i in 1..NUM_GW {
        for j in 0..i {
            gw_nodes[i].add_gateway_peer(gw_addrs[j]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    struct ClientInfo {
        node: Node,
        peer_id: ntied_transport::crypto::PeerId,
    }

    let mut clients: Vec<ClientInfo> = Vec::new();
    for gw_idx in 0..NUM_GW {
        for _ in 0..CLIENTS_PER_GW {
            let id = PrivateKey::generate();
            let peer_id = id.public_key().peer_id();
            let node = Node::bind(localhost(), id).await.unwrap();
            node.join_network(NetworkConfig {
                bootstrap: vec![gw_addrs[gw_idx]],
                preferred_gateway: None,
            })
            .await
            .unwrap();
            clients.push(ClientInfo { node, peer_id });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(5000)).await;
    let clients: &'static [ClientInfo] = Vec::leak(clients);

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
}
