mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey, RelayNode};

use common::{
    DisconnectingRelayNode, LossyRelayNode, StreamReader, frame_message, init_tracing, localhost,
    make_payload, verify_payload,
};

// ── Helper: connect through lossy relay ──

async fn connect_lossy(
    drop_rate: f64,
) -> (
    Node,
    Arc<Node>,
    ntied_transport::Connection,
    ntied_transport::Connection,
    Arc<LossyRelayNode>,
) {
    let relay = Arc::new(
        LossyRelayNode::bind(localhost(), PrivateKey::generate(), drop_rate)
            .await
            .unwrap(),
    );
    let relay_addr = relay.local_addr().unwrap();
    let r = relay.clone();
    tokio::spawn(async move { r.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    (node_a, node_b, conn_a, conn_b, relay)
}

// ── Stream through lossy relay (10% loss) ──

#[tokio::test(flavor = "multi_thread")]
async fn stream_survives_10pct_loss() {
    init_tracing();

    let (_na, _nb, conn_a, conn_b, relay) = connect_lossy(0.10).await;

    let sa = conn_a.open_stream(1).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();

    const MSGS: u32 = 50;
    const PAYLOAD: usize = 200;

    let send_task = tokio::spawn(async move {
        for seq in 0..MSGS {
            let payload = make_payload(seq, PAYLOAD);
            let framed = frame_message(&payload);
            sa.send(&framed).await.unwrap();
        }
    });

    let recv_task = tokio::spawn(async move {
        let mut reader = StreamReader::new();
        let mut expected = 0u32;
        while expected < MSGS {
            while let Some(msg) = reader.try_read() {
                let seq = verify_payload(&msg).expect("corrupted through lossy relay");
                assert_eq!(seq, expected, "order mismatch through lossy relay");
                expected += 1;
            }
            if expected >= MSGS {
                break;
            }
            match tokio::time::timeout(Duration::from_secs(30), sb.recv()).await {
                Ok(Ok(data)) => reader.push(&data),
                Ok(Err(e)) => panic!("stream recv error: {e}"),
                Err(_) => panic!("stream timeout at seq {expected}"),
            }
        }
        expected
    });

    send_task.await.unwrap();
    let count = recv_task.await.unwrap();
    assert_eq!(count, MSGS);

    let (forwarded, dropped) = relay.stats();
    eprintln!("10% loss: forwarded={forwarded}, dropped={dropped}");
    assert!(dropped > 0, "lossy relay should have dropped some packets");
}

// ── Stream through lossy relay (20% loss) ──

#[tokio::test(flavor = "multi_thread")]
async fn stream_survives_20pct_loss() {
    init_tracing();

    // 20% is the practical upper bound — higher rates risk handshake timeout
    let (_na, _nb, conn_a, conn_b, relay) = connect_lossy(0.20).await;

    let sa = conn_a.open_stream(1).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();

    const MSGS: u32 = 30;
    const PAYLOAD: usize = 128;

    let send_task = tokio::spawn(async move {
        for seq in 0..MSGS {
            let payload = make_payload(seq, PAYLOAD);
            let framed = frame_message(&payload);
            sa.send(&framed).await.unwrap();
            if seq % 5 == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    });

    let recv_task = tokio::spawn(async move {
        let mut reader = StreamReader::new();
        let mut expected = 0u32;
        while expected < MSGS {
            while let Some(msg) = reader.try_read() {
                let seq = verify_payload(&msg).expect("corrupted");
                assert_eq!(seq, expected);
                expected += 1;
            }
            if expected >= MSGS {
                break;
            }
            match tokio::time::timeout(Duration::from_secs(60), sb.recv()).await {
                Ok(Ok(data)) => reader.push(&data),
                Ok(Err(e)) => panic!("recv error: {e}"),
                Err(_) => panic!("timeout at seq {expected}"),
            }
        }
        expected
    });

    send_task.await.unwrap();
    let count = recv_task.await.unwrap();
    assert_eq!(count, MSGS);

    let (forwarded, dropped) = relay.stats();
    eprintln!("20% loss: forwarded={forwarded}, dropped={dropped}");
}

// ── Datagram delivery rate under loss ──

#[tokio::test(flavor = "multi_thread")]
async fn datagram_delivery_under_loss() {
    init_tracing();

    let (_na, _nb, conn_a, conn_b, relay) = connect_lossy(0.15).await;

    let da = conn_a.open_datagram(1).await.unwrap();
    let (db, _) = conn_b.accept_datagram().await.unwrap();

    const MSGS: u32 = 100;

    for seq in 0..MSGS {
        let payload = make_payload(seq, 100);
        da.send(&payload).await.unwrap();
        if seq % 10 == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut received = HashSet::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), db.recv()).await {
            Ok(Ok(data)) => {
                if let Some(seq) = verify_payload(&data) {
                    received.insert(seq);
                }
            }
            _ => break,
        }
    }

    let (forwarded, dropped) = relay.stats();
    eprintln!(
        "datagram under 15% loss: received={}/{MSGS}, forwarded={forwarded}, dropped={dropped}",
        received.len()
    );

    // With 15% relay packet loss, datagrams may still all arrive due to
    // retransmission of the underlying transport packets. The key invariant is:
    // - the relay did drop some packets (drop_rate > 0)
    // - we received at least some datagrams
    // - no data corruption occurred (verify_payload checked above)
    assert!(
        received.len() > 0,
        "should receive at least some datagrams under loss"
    );
    assert!(dropped > 0, "lossy relay should have dropped some packets");
}

// ── Relay disconnect mid-stream ──

#[tokio::test(flavor = "multi_thread")]
async fn stream_resilience_after_relay_disconnect() {
    init_tracing();

    // Relay that stops forwarding after 100 tunnel messages.
    // The handshake + auth + pings consume many tunnel messages,
    // so 100 allows the connection to establish and some data to flow.
    let relay = DisconnectingRelayNode::bind(localhost(), PrivateKey::generate(), 100)
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay = Arc::new(relay);
    let r = relay.clone();
    tokio::spawn(async move { r.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(1).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();

    // Send a burst quickly, then collect what we can.
    // After ~20 relay forwards the relay stops, so later packets are lost.
    let mut sent = 0u32;
    for seq in 0..20u32 {
        let payload = make_payload(seq, 64);
        let framed = frame_message(&payload);
        match sa.send(&framed).await {
            Ok(_) => sent += 1,
            Err(_) => break,
        }
    }

    // Collect received messages (with short timeout per-recv to not wait for connection timeout)
    let mut reader = StreamReader::new();
    let mut received = 0u32;
    loop {
        match tokio::time::timeout(Duration::from_secs(3), sb.recv()).await {
            Ok(Ok(data)) => {
                reader.push(&data);
                while let Some(msg) = reader.try_read() {
                    if verify_payload(&msg).is_some() {
                        received += 1;
                    }
                }
            }
            _ => break,
        }
    }

    eprintln!(
        "disconnecting relay: sent={sent}, received={received}, relay forwarded={}",
        relay.total_forwarded.load(std::sync::atomic::Ordering::Relaxed)
    );

    // We should have received some messages before the relay stopped
    assert!(
        received > 0,
        "should receive some messages before relay disconnect"
    );
}

// ── Recovery: reattach to new relay after disconnect ──

#[tokio::test(flavor = "multi_thread")]
async fn recovery_after_relay_disconnect() {
    init_tracing();

    // First relay with limited forwarding
    let bad_relay = DisconnectingRelayNode::bind(localhost(), PrivateKey::generate(), 30)
        .await
        .unwrap();
    let bad_addr = bad_relay.local_addr().unwrap();
    let bad_relay = Arc::new(bad_relay);
    let br = bad_relay.clone();
    tokio::spawn(async move { br.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(bad_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(bad_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // Send a few messages through bad relay
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"before disconnect").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(5), sb.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(data, b"before disconnect");

    // Wait for relay to exhaust its forwarding budget
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Attach both to a good relay
    let good_relay = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let good_addr = good_relay.local_addr().unwrap();
    tokio::spawn(async move { good_relay.run().await });

    node_a.attach_relay(good_addr).await.unwrap();
    node_b.attach_relay(good_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send through new relay — should work
    sa.send(b"after recovery").await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(10), sb.recv())
        .await
        .expect("timeout after recovery")
        .expect("recv error after recovery");
    assert_eq!(data, b"after recovery");
}

// ── Lossy relay with hole punch migration ──

#[tokio::test(flavor = "multi_thread")]
async fn lossy_relay_then_direct_migration() {
    init_tracing();

    // Use low loss rate so handshake completes reliably
    let (_na, _nb, conn_a, conn_b, relay) = connect_lossy(0.05).await;

    assert!(conn_a.is_relayed().await);

    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"via lossy relay").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(10), sb.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(data, b"via lossy relay");

    // Try direct migration
    conn_a.try_direct().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send more data — should work regardless of path
    sa.send(b"maybe direct").await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(10), sb.recv())
        .await
        .expect("timeout after migration")
        .expect("recv error");
    assert_eq!(data, b"maybe direct");

    let (forwarded, dropped) = relay.stats();
    eprintln!("lossy+migration: forwarded={forwarded}, dropped={dropped}");
}
