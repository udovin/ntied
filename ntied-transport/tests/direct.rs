mod common;

use std::sync::Arc;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey};

use common::{connect_direct, init_tracing, localhost};

// ── Handshake ──

#[tokio::test]
async fn handshake_direct() {
    init_tracing();
    let p = connect_direct().await;
    assert!(p.conn_a.is_established().await);
    assert!(p.conn_b.is_established().await);
    assert!(p.conn_a.peer_id().await.is_some());
    assert!(p.conn_b.peer_id().await.is_some());
}

#[tokio::test]
async fn peer_ids_match_keys() {
    init_tracing();
    let id_a = PrivateKey::generate();
    let id_b = PrivateKey::generate();
    let expected_a = id_a.public_key().peer_id();
    let expected_b = id_b.public_key().peer_id();

    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert_eq!(conn_a.peer_id().await.unwrap(), expected_b);
    assert_eq!(conn_b.peer_id().await.unwrap(), expected_a);
}

#[tokio::test]
async fn connection_is_not_relayed() {
    init_tracing();
    let p = connect_direct().await;
    assert!(!p.conn_a.is_relayed().await);
    assert!(!p.conn_b.is_relayed().await);
}

// ── Streams ──

#[tokio::test]
async fn stream_send_recv() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello world").await.unwrap();

    let (sb, purpose) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello world");
}

#[tokio::test]
async fn bidirectional_streams() {
    init_tracing();
    let p = connect_direct().await;

    // A -> B
    let sa = p.conn_a.open_stream(1).await.unwrap();
    sa.send(b"from A").await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(sb.recv().await.unwrap(), b"from A");

    // B -> A
    let sb2 = p.conn_b.open_stream(2).await.unwrap();
    sb2.send(b"from B").await.unwrap();
    let (sa2, _) = p.conn_a.accept_stream().await.unwrap();
    assert_eq!(sa2.recv().await.unwrap(), b"from B");
}

#[tokio::test]
async fn multi_message_ordered() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    for i in 0..20 {
        let msg = format!("msg-{i}");
        sa.send(msg.as_bytes()).await.unwrap();
        let data = sb.recv().await.unwrap();
        assert_eq!(data, msg.as_bytes());
    }
}

#[tokio::test]
async fn multiple_concurrent_streams() {
    init_tracing();
    let p = connect_direct().await;

    let mut streams_a = Vec::new();
    let mut streams_b = Vec::new();

    for i in 0..5u16 {
        let s = p.conn_a.open_stream(100 + i).await.unwrap();
        streams_a.push(s);
        let (sb, purpose) = p.conn_b.accept_stream().await.unwrap();
        assert_eq!(purpose, 100 + i);
        streams_b.push(sb);
    }

    // Send on all, then receive on all
    for (i, s) in streams_a.iter().enumerate() {
        let msg = format!("stream-{i}");
        s.send(msg.as_bytes()).await.unwrap();
    }
    for (i, s) in streams_b.iter().enumerate() {
        let expected = format!("stream-{i}");
        let data = s.recv().await.unwrap();
        assert_eq!(data, expected.as_bytes());
    }
}

#[tokio::test]
async fn large_stream_message() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    // 64 KB message — exceeds MTU, should be fragmented internally
    let big = vec![0xABu8; 64 * 1024];
    sa.send(&big).await.unwrap();

    let mut received = Vec::new();
    while received.len() < big.len() {
        let chunk = tokio::time::timeout(Duration::from_secs(10), sb.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, big);
}

#[tokio::test]
async fn stream_close() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    sa.send(b"before close").await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"before close");

    sa.close().await.unwrap();

    // Sending on a closed stream should error
    let result = sa.send(b"after close").await;
    assert!(result.is_err());
}

// ── Datagrams ──

#[tokio::test]
async fn datagram_send_recv() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(99).await.unwrap();
    da.send(b"datagram hello").await.unwrap();

    let (db, purpose) = p.conn_b.accept_datagram().await.unwrap();
    assert_eq!(purpose, 99);
    let data = db.recv().await.unwrap();
    assert_eq!(data, b"datagram hello");
}

#[tokio::test]
async fn datagram_multiple_messages() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(1).await.unwrap();
    let (db, _) = p.conn_b.accept_datagram().await.unwrap();

    for i in 0..20u32 {
        da.send(&i.to_be_bytes()).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut received = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(2), db.recv()).await {
            Ok(Ok(data)) => received.push(u32::from_be_bytes(data.try_into().unwrap())),
            _ => break,
        }
    }
    // Datagrams are unreliable, but locally we expect most to arrive
    assert!(received.len() > 10, "received only {} / 20", received.len());
}

#[tokio::test]
async fn datagram_large_fragmented() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(1).await.unwrap();
    let (db, _) = p.conn_b.accept_datagram().await.unwrap();

    // A datagram larger than MTU should be fragmented and reassembled
    let big = vec![0xCDu8; 4096];
    da.send(&big).await.unwrap();

    let data = tokio::time::timeout(Duration::from_secs(5), db.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(data, big);
}

#[tokio::test]
async fn datagram_close() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(1).await.unwrap();
    let (_db, _) = p.conn_b.accept_datagram().await.unwrap();

    da.send(b"ok").await.unwrap();
    da.close().await.unwrap();

    let result = da.send(b"after close").await;
    assert!(result.is_err());
}

// ── Connection lifecycle ──

#[tokio::test]
async fn connection_close() {
    init_tracing();
    let p = connect_direct().await;
    assert!(p.conn_a.is_established().await);
    p.conn_a.close().await.unwrap();
    // Double close should not panic
    p.conn_a.close().await.unwrap();
}

#[tokio::test]
async fn accept_does_not_return_initiator() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let addr_b = node_b.local_addr().unwrap();

    let conn_a = node_a.connect(addr_b).await.unwrap();
    assert!(conn_a.is_established().await);

    // B should be able to accept
    let accept_b = tokio::time::timeout(Duration::from_secs(2), node_b.accept()).await;
    assert!(accept_b.is_ok());

    // A should not have anything to accept
    let accept_a = tokio::time::timeout(Duration::from_millis(500), node_a.accept()).await;
    assert!(accept_a.is_err());
}

#[tokio::test]
async fn multiple_connections_to_same_node() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept1 = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a1 = node_a.connect(addr_b).await.unwrap();
    let conn_b1 = accept1.await.unwrap();

    assert!(conn_a1.is_established().await);
    assert!(conn_b1.is_established().await);

    // Second connection
    let nb = node_b.clone();
    let accept2 = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a2 = node_a.connect(addr_b).await.unwrap();
    let conn_b2 = accept2.await.unwrap();

    assert!(conn_a2.is_established().await);
    assert!(conn_b2.is_established().await);

    // Both connections should work independently
    let s1 = conn_a1.open_stream(1).await.unwrap();
    s1.send(b"conn1").await.unwrap();
    let (r1, _) = conn_b1.accept_stream().await.unwrap();
    assert_eq!(r1.recv().await.unwrap(), b"conn1");

    let s2 = conn_a2.open_stream(2).await.unwrap();
    s2.send(b"conn2").await.unwrap();
    let (r2, _) = conn_b2.accept_stream().await.unwrap();
    assert_eq!(r2.recv().await.unwrap(), b"conn2");
}

// ── Mixed channels ──

#[tokio::test]
async fn streams_and_datagrams_on_same_connection() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let da = p.conn_a.open_datagram(2).await.unwrap();

    let (sb, _) = p.conn_b.accept_stream().await.unwrap();
    let (db, _) = p.conn_b.accept_datagram().await.unwrap();

    sa.send(b"stream data").await.unwrap();
    da.send(b"datagram data").await.unwrap();

    let stream_data = sb.recv().await.unwrap();
    assert_eq!(stream_data, b"stream data");

    let dg_data = tokio::time::timeout(Duration::from_secs(5), db.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    assert_eq!(dg_data, b"datagram data");
}

// ── Empty data ──

#[tokio::test]
async fn stream_empty_message() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    // Send empty data followed by non-empty to verify stream isn't broken
    sa.send(b"").await.unwrap();
    sa.send(b"after empty").await.unwrap();

    // The empty send may or may not produce a separate recv,
    // but "after empty" must arrive
    let mut got = Vec::new();
    while got.is_empty() || !got.ends_with(b"after empty") {
        let data = tokio::time::timeout(Duration::from_secs(5), sb.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        got.extend_from_slice(&data);
    }
    assert!(got.ends_with(b"after empty"));
}
