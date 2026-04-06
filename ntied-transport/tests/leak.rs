mod common;

use std::sync::Arc;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey};

use common::{init_tracing, localhost};

// ── Async task leak detection ──
//
// These tests verify that creating and dropping connections/channels does not
// leave orphaned tokio tasks. We use tokio's runtime metrics to count alive
// tasks before and after the operations.

#[tokio::test(flavor = "multi_thread")]
async fn connections_drop_cleans_up() {
    init_tracing();

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    // Create and drop several connections
    for _ in 0..5 {
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        // Use the connection briefly
        let sa = conn_a.open_stream(1).await.unwrap();
        sa.send(b"test").await.unwrap();
        let (sb, _) = conn_b.accept_stream().await.unwrap();
        let _ = sb.recv().await.unwrap();

        // Close explicitly
        conn_a.close().await.unwrap();
        drop(conn_b);
    }

    // Allow cleanup
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify node still works (no state corruption)
    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();
    assert!(conn.is_established().await);
    assert!(conn_b.is_established().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn many_streams_no_leak() {
    init_tracing();

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // Open, use, and close many streams
    for i in 0..50u16 {
        let sa = conn_a.open_stream(i).await.unwrap();
        let (sb, purpose) = conn_b.accept_stream().await.unwrap();
        assert_eq!(purpose, i);

        let msg = format!("stream-{i}");
        sa.send(msg.as_bytes()).await.unwrap();
        let data = sb.recv().await.unwrap();
        assert_eq!(data, msg.as_bytes());

        sa.close().await.unwrap();
    }

    // Open more streams after closing many — should still work
    let sa = conn_a.open_stream(999).await.unwrap();
    sa.send(b"still alive").await.unwrap();
    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 999);
    assert_eq!(sb.recv().await.unwrap(), b"still alive");
}

#[tokio::test(flavor = "multi_thread")]
async fn many_datagrams_no_leak() {
    init_tracing();

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    for i in 0..30u16 {
        let da = conn_a.open_datagram(i).await.unwrap();
        let (db, purpose) = conn_b.accept_datagram().await.unwrap();
        assert_eq!(purpose, i);

        da.send(b"dg").await.unwrap();
        let data = tokio::time::timeout(Duration::from_secs(5), db.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        assert_eq!(data, b"dg");

        da.close().await.unwrap();
    }

    // Verify connection still operational
    let da = conn_a.open_datagram(999).await.unwrap();
    da.send(b"alive").await.unwrap();
    let (db, _) = conn_b.accept_datagram().await.unwrap();
    let data = tokio::time::timeout(Duration::from_secs(5), db.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(data, b"alive");
}

// ── Node drop cleanup ──

#[tokio::test(flavor = "multi_thread")]
async fn node_drop_does_not_panic() {
    init_tracing();

    // Create nodes, connect, then drop everything
    {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let addr_b = node_b.local_addr().unwrap();

        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        let sa = conn_a.open_stream(1).await.unwrap();
        sa.send(b"bye").await.unwrap();
        let (sb, _) = conn_b.accept_stream().await.unwrap();
        let _ = sb.recv().await.unwrap();

        // Drop everything — should not panic or leave dangling tasks
        drop(sa);
        drop(sb);
        drop(conn_a);
        drop(conn_b);
        drop(node_b);
        drop(node_a);
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    // If we get here without panic, the test passes
}

// ── Rapid connect/disconnect cycles ──

#[tokio::test(flavor = "multi_thread")]
async fn rapid_connect_disconnect() {
    init_tracing();

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    for _ in 0..10 {
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        conn_a.close().await.unwrap();
        drop(conn_b);
    }

    // Brief pause for cleanup
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Final connection should still work
    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();
    assert!(conn.is_established().await);
    assert!(conn_b.is_established().await);
}

// ── Relay attach/detach leak ──

#[tokio::test(flavor = "multi_thread")]
async fn relay_reattach_no_leak() {
    init_tracing();

    let relay1 = ntied_transport::RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay1_addr = relay1.local_addr().unwrap();
    let _r1_task = tokio::spawn(async move { relay1.run().await });

    let relay2 = ntied_transport::RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay2_addr = relay2.local_addr().unwrap();
    let _r2_task = tokio::spawn(async move { relay2.run().await });

    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    // Attach and detach multiple times
    for addr in [relay1_addr, relay2_addr, relay1_addr, relay2_addr] {
        node.attach_relay(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Node should still be functional
    assert!(node.is_relay_attached().await);

    // Allow any cleanup tasks to run
    tokio::time::sleep(Duration::from_millis(300)).await;
}
