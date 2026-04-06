mod common;

use std::sync::Arc;
use std::time::Duration;

use ntied_transport::relay::protocol::{RelayMessage, PURPOSE_RELAY};
use ntied_transport::{Node, PrivateKey, RelayNode};

use common::{connect_via_relay, init_tracing, localhost};

// ── Raw tunnel ──

#[tokio::test]
async fn tunnel_bidirectional() {
    init_tracing();

    let relay = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let conn_a = node_a.connect(relay_addr).await.unwrap();
    let ch_a = conn_a.open_datagram(PURPOSE_RELAY).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    let conn_b = node_b.connect(relay_addr).await.unwrap();
    let ch_b = conn_b.open_datagram(PURPOSE_RELAY).await.unwrap();

    // Both receive Welcome
    let wa = ch_a.recv().await.unwrap();
    assert!(matches!(
        RelayMessage::decode(&wa),
        Some(RelayMessage::Welcome { .. })
    ));
    let wb = ch_b.recv().await.unwrap();
    assert!(matches!(
        RelayMessage::decode(&wb),
        Some(RelayMessage::Welcome { .. })
    ));

    // A -> B
    let msg = RelayMessage::Tunnel {
        peer_id: peer_id_b,
        data: b"hello from A".to_vec(),
    };
    ch_a.send(&msg.encode()).await.unwrap();

    let received = ch_b.recv().await.unwrap();
    match RelayMessage::decode(&received).unwrap() {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_a);
            assert_eq!(data, b"hello from A");
        }
        _ => panic!("expected Tunnel"),
    }

    // B -> A
    let reply = RelayMessage::Tunnel {
        peer_id: peer_id_a,
        data: b"hello from B".to_vec(),
    };
    ch_b.send(&reply.encode()).await.unwrap();

    let received = ch_a.recv().await.unwrap();
    match RelayMessage::decode(&received).unwrap() {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_b);
            assert_eq!(data, b"hello from B");
        }
        _ => panic!("expected Tunnel"),
    }

    relay_task.abort();
}

// ── connect_peer + stream ──

#[tokio::test]
async fn connect_peer_stream_bidirectional() {
    init_tracing();
    let p = connect_via_relay().await;

    assert!(p.conn_a.is_established().await);
    assert!(p.conn_b.is_established().await);
    assert!(p.conn_a.is_relayed().await);
    assert!(p.conn_b.is_relayed().await);

    // A -> B
    let sa = p.conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello via relay").await.unwrap();
    let (sb, purpose) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    assert_eq!(sb.recv().await.unwrap(), b"hello via relay");

    // B -> A
    let sb2 = p.conn_b.open_stream(99).await.unwrap();
    sb2.send(b"reply from B").await.unwrap();
    let (sa2, purpose2) = p.conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose2, 99);
    assert_eq!(sa2.recv().await.unwrap(), b"reply from B");

    p.relay_task.abort();
}

// ── Datagram through relay ──

#[tokio::test]
async fn datagram_through_relay() {
    init_tracing();
    let p = connect_via_relay().await;

    let da = p.conn_a.open_datagram(99).await.unwrap();
    let (db, purpose) = p.conn_b.accept_datagram().await.unwrap();
    assert_eq!(purpose, 99);

    for i in 0..10u32 {
        da.send(&i.to_be_bytes()).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut received = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(2), db.recv()).await {
            Ok(Ok(_)) => received += 1,
            _ => break,
        }
    }
    assert!(received > 0, "received 0 datagrams through relay");

    p.relay_task.abort();
}

// ── Connection survives relay restart ──

#[tokio::test]
async fn connection_survives_relay_restart() {
    init_tracing();

    let relay1 = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay1_addr = relay1.local_addr().unwrap();
    let relay1_task = tokio::spawn(async move { relay1.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay1_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay1_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // Verify before crash
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"before crash").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    assert_eq!(sb.recv().await.unwrap(), b"before crash");

    // Kill relay, start new one
    relay1_task.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let relay2 = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay2_addr = relay2.local_addr().unwrap();
    let _relay2_task = tokio::spawn(async move { relay2.run().await });

    node_a.attach_relay(relay2_addr).await.unwrap();
    node_b.attach_relay(relay2_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send via new relay
    sa.send(b"after recovery").await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(5), sb.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(data, b"after recovery");
}

// ── Relay to direct migration ──

#[tokio::test]
async fn relay_to_direct_migration() {
    init_tracing();
    let p = connect_via_relay().await;

    assert!(p.conn_a.is_relayed().await);
    assert!(p.conn_b.is_relayed().await);

    let sa = p.conn_a.open_stream(1).await.unwrap();
    sa.send(b"via relay").await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(sb.recv().await.unwrap(), b"via relay");

    // Initiate hole punch
    p.conn_a.try_direct().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    sa.send(b"via direct").await.unwrap();
    assert_eq!(sb.recv().await.unwrap(), b"via direct");

    let a_direct = !p.conn_a.is_relayed().await;
    let b_direct = !p.conn_b.is_relayed().await;
    assert!(
        a_direct || b_direct,
        "at least one side should have switched to direct"
    );

    p.relay_task.abort();
}

// ── Multiple peers on same relay ──

#[tokio::test]
async fn three_peers_on_same_relay() {
    init_tracing();

    let relay = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    let id_c = PrivateKey::generate();
    let peer_id_c = id_c.public_key().peer_id();
    let node_c = Arc::new(Node::bind(localhost(), id_c).await.unwrap());
    node_c.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // A -> B
    let nb = node_b.clone();
    let accept_b = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_ab = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_ba = accept_b.await.unwrap();

    // A -> C
    let nc = node_c.clone();
    let accept_c = tokio::spawn(async move { nc.accept().await.unwrap() });
    let conn_ac = node_a.connect_peer(&peer_id_c).await.unwrap();
    let conn_ca = accept_c.await.unwrap();

    // Send on both connections
    let sab = conn_ab.open_stream(1).await.unwrap();
    sab.send(b"to B").await.unwrap();
    let (rba, _) = conn_ba.accept_stream().await.unwrap();
    assert_eq!(rba.recv().await.unwrap(), b"to B");

    let sac = conn_ac.open_stream(2).await.unwrap();
    sac.send(b"to C").await.unwrap();
    let (rca, _) = conn_ca.accept_stream().await.unwrap();
    assert_eq!(rca.recv().await.unwrap(), b"to C");
}

// ── Large stream through relay ──

#[tokio::test]
async fn large_stream_through_relay() {
    init_tracing();
    let p = connect_via_relay().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    let big = vec![0xEFu8; 32 * 1024];
    sa.send(&big).await.unwrap();

    let mut received = Vec::new();
    while received.len() < big.len() {
        let chunk = tokio::time::timeout(Duration::from_secs(15), sb.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, big);

    p.relay_task.abort();
}
