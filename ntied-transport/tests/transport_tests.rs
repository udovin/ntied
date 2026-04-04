use std::net::SocketAddr;
use std::sync::Once;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey, RelayNode};
use ntied_transport::relay::protocol::{RelayMessage, PURPOSE_RELAY};

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
async fn two_nodes_handshake() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);
    assert!(conn_a.peer_id().await.is_some());
    assert!(conn_b.peer_id().await.is_some());
}

#[tokio::test]
async fn connect_and_stream() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello world").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello world");
}

#[tokio::test]
async fn bidirectional_streams() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // A → B
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"from A").await.unwrap();

    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"from A");

    // B → A
    let sb2 = conn_b.open_stream(2).await.unwrap();
    sb2.send(b"from B").await.unwrap();

    let (sa2, _) = conn_a.accept_stream().await.unwrap();
    let data2 = sa2.recv().await.unwrap();
    assert_eq!(data2, b"from B");
}

#[tokio::test]
async fn multi_message_exchange() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(1).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();

    for i in 0..10 {
        let msg = format!("message {i}");
        sa.send(msg.as_bytes()).await.unwrap();
        let data = sb.recv().await.unwrap();
        assert_eq!(data, msg.as_bytes());
    }
}

#[tokio::test]
async fn accept_does_not_return_initiator_connection() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let conn_a = node_a.connect(addr_b).await.unwrap();
    assert!(conn_a.is_established().await);

    // node_b.accept() should return the connection
    let accept = tokio::time::timeout(Duration::from_secs(2), node_b.accept()).await;
    assert!(accept.is_ok(), "accept should return the responder connection");

    // node_a.accept() should NOT return anything (initiator doesn't get accept)
    let accept_a = tokio::time::timeout(Duration::from_millis(500), node_a.accept()).await;
    assert!(accept_a.is_err(), "initiator should not get accept");
}

#[tokio::test]
async fn connection_close() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let _conn_b = accept.await.unwrap();

    conn_a.close().await.unwrap();
    // Connection should be closed gracefully
}

#[tokio::test]
async fn datagram_channel() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let da = conn_a.open_datagram(99).await.unwrap();
    da.send(b"datagram hello").await.unwrap();

    let (db, purpose) = conn_b.accept_datagram().await.unwrap();
    assert_eq!(purpose, 99);
    let data = db.recv().await.unwrap();
    assert_eq!(data, b"datagram hello");
}

// ── Relay tests ──

#[tokio::test]
async fn relay_two_clients_tunnel() {
    init_tracing();

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    // Client A connects to relay
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let conn_a = node_a.connect(relay_addr).await.unwrap();
    let relay_ch_a = conn_a.open_datagram(PURPOSE_RELAY).await.unwrap();

    // Client B connects to relay
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    let conn_b = node_b.connect(relay_addr).await.unwrap();
    let relay_ch_b = conn_b.open_datagram(PURPOSE_RELAY).await.unwrap();

    // Both receive welcome
    let welcome_a = relay_ch_a.recv().await.unwrap();
    assert!(matches!(RelayMessage::decode(&welcome_a), Some(RelayMessage::Welcome { .. })));

    let welcome_b = relay_ch_b.recv().await.unwrap();
    assert!(matches!(RelayMessage::decode(&welcome_b), Some(RelayMessage::Welcome { .. })));

    // A sends tunnel message to B
    let msg = RelayMessage::Tunnel {
        peer_id: peer_id_b,
        data: b"hello from A".to_vec(),
    };
    relay_ch_a.send(&msg.encode()).await.unwrap();

    // B receives tunnel message from A
    let received = relay_ch_b.recv().await.unwrap();
    let decoded = RelayMessage::decode(&received).unwrap();
    match decoded {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_a, "should be from A");
            assert_eq!(data, b"hello from A");
        }
        _ => panic!("expected Tunnel message"),
    }

    // B sends back to A
    let reply = RelayMessage::Tunnel {
        peer_id: peer_id_a,
        data: b"hello from B".to_vec(),
    };
    relay_ch_b.send(&reply.encode()).await.unwrap();

    // A receives reply from B
    let received = relay_ch_a.recv().await.unwrap();
    let decoded = RelayMessage::decode(&received).unwrap();
    match decoded {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_b, "should be from B");
            assert_eq!(data, b"hello from B");
        }
        _ => panic!("expected Tunnel message"),
    }

    relay_task.abort();
}

#[tokio::test]
async fn relay_connect_peer_and_stream() {
    init_tracing();

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    // Peer A attaches to relay
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    // Peer B attaches to relay
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    node_b.attach_relay(relay_addr).await.unwrap();

    // Small delay for relay registration
    tokio::time::sleep(Duration::from_millis(100)).await;

    // B accepts, A connects to B through relay
    let accept = tokio::spawn(async move {
        node_b.accept().await.unwrap()
    });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);

    // Open stream A → B
    let sa = conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello via relay").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello via relay");

    // Reply B → A
    let sb2 = conn_b.open_stream(99).await.unwrap();
    sb2.send(b"reply from B").await.unwrap();

    let (sa2, purpose2) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose2, 99);
    let data2 = sa2.recv().await.unwrap();
    assert_eq!(data2, b"reply from B");

    relay_task.abort();
}

#[tokio::test]
async fn relay_connection_survives_relay_restart() {
    init_tracing();

    // Start relay 1
    let relay1 = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay1_addr = relay1.local_addr().unwrap();
    let relay1_task = tokio::spawn(async move { relay1.run().await });

    // Peer A and B — keep references accessible
    let id_a = PrivateKey::generate();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.attach_relay(relay1_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = std::sync::Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay1_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Establish connection A → B through relay
    let node_b2 = node_b.clone();
    let accept_b = tokio::spawn(async move { node_b2.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept_b.await.unwrap();

    // Verify it works before crash
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"before crash").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"before crash");

    // Kill relay 1
    relay1_task.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Start relay 2 on new port
    let relay2 = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay2_addr = relay2.local_addr().unwrap();
    let _relay2_task = tokio::spawn(async move { relay2.run().await });

    // Re-attach both peers to new relay
    node_a.attach_relay(relay2_addr).await.unwrap();
    node_b.attach_relay(relay2_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send data through existing stream — should work via new relay
    sa.send(b"after recovery").await.unwrap();

    let data2 = tokio::time::timeout(Duration::from_secs(5), sb.recv()).await;
    match data2 {
        Ok(Ok(d)) => assert_eq!(d, b"after recovery"),
        Ok(Err(e)) => panic!("recv error after relay restart: {e}"),
        Err(_) => panic!("timeout waiting for data after relay restart"),
    }
}
