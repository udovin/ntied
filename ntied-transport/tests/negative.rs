mod common;

use std::time::Duration;

use ntied_transport::{Node, PrivateKey};

use common::{connect_direct, connect_via_relay, init_tracing, localhost};

// ── Connection errors ──

#[tokio::test]
async fn connect_to_nobody_times_out() {
    init_tracing();
    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    // Connect to a port where nobody is listening — should timeout
    let addr = "127.0.0.1:19999".parse().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), node.connect(addr)).await;

    match result {
        Ok(Ok(_)) => panic!("should not connect to nobody"),
        Ok(Err(e)) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
        Err(_) => {} // test timeout is fine too
    }
}

#[tokio::test]
async fn connect_peer_without_relay() {
    init_tracing();
    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let fake_peer_id = PrivateKey::generate().public_key().peer_id();

    let result = node.connect_peer(&fake_peer_id).await;
    assert!(result.is_err(), "connect_peer without relay should fail");
}

// ── Channel errors after close ──

#[tokio::test]
async fn send_on_closed_stream_errors() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (_sb, _) = p.conn_b.accept_stream().await.unwrap();

    sa.close().await.unwrap();
    let result = sa.send(b"nope").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn send_on_closed_datagram_errors() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(1).await.unwrap();
    let (_db, _) = p.conn_b.accept_datagram().await.unwrap();

    da.close().await.unwrap();
    let result = da.send(b"nope").await;
    assert!(result.is_err());
}

// ── Operations after connection close ──

#[tokio::test]
async fn open_stream_after_close() {
    init_tracing();
    let p = connect_direct().await;

    p.conn_a.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The connection entry may be cleaned up; opening a stream should error
    let result = p.conn_a.open_stream(1).await;
    // This may succeed briefly or fail — the key thing is no panic
    drop(result);
}

#[tokio::test]
async fn double_close_is_safe() {
    init_tracing();
    let p = connect_direct().await;
    p.conn_a.close().await.unwrap();
    p.conn_a.close().await.unwrap();
    // No panic
}

// ── Node shutdown ──

#[tokio::test]
async fn accept_after_shutdown() {
    init_tracing();
    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    node.shutdown().await;

    let result = node.accept().await;
    assert!(result.is_err(), "accept after shutdown should error");
}

#[tokio::test]
async fn shutdown_while_accepting() {
    init_tracing();

    let node = std::sync::Arc::new(
        Node::bind(localhost(), PrivateKey::generate()).await.unwrap(),
    );
    let n2 = node.clone();
    let accept_task = tokio::spawn(async move { n2.accept().await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    node.shutdown().await;

    let result = tokio::time::timeout(Duration::from_secs(5), accept_task)
        .await
        .expect("accept task should resolve after shutdown")
        .unwrap();
    assert!(result.is_err(), "accept should error after shutdown");
}

// ── Relay negative ──

#[tokio::test]
async fn try_direct_on_direct_connection_is_noop() {
    init_tracing();
    let p = connect_direct().await;
    assert!(!p.conn_a.is_relayed().await);

    // try_direct on already-direct connection should succeed without error
    let result = p.conn_a.try_direct().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn relay_connect_unknown_peer_times_out() {
    init_tracing();
    let relay = ntied_transport::RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node.attach_relay(relay_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let fake_peer = PrivateKey::generate().public_key().peer_id();

    // connect_peer to a non-existent peer — handshake should timeout
    let result = tokio::time::timeout(Duration::from_secs(20), node.connect_peer(&fake_peer)).await;
    match result {
        Ok(Ok(_)) => panic!("should not connect to unknown peer"),
        Ok(Err(e)) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
        Err(_) => {} // overall timeout is fine too
    }
}

// ── Edge cases ──

#[tokio::test]
async fn accept_stream_on_connection_with_no_streams() {
    init_tracing();
    let p = connect_direct().await;

    // accept_stream should block; verify it doesn't return spuriously
    let result = tokio::time::timeout(Duration::from_millis(300), p.conn_b.accept_stream()).await;
    assert!(result.is_err(), "accept_stream should timeout when no streams opened");
}

#[tokio::test]
async fn accept_datagram_on_connection_with_no_datagrams() {
    init_tracing();
    let p = connect_direct().await;

    let result =
        tokio::time::timeout(Duration::from_millis(300), p.conn_b.accept_datagram()).await;
    assert!(
        result.is_err(),
        "accept_datagram should timeout when no datagrams opened"
    );
}

// ── Purpose values ──

#[tokio::test]
async fn stream_purpose_zero() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(0).await.unwrap();
    sa.send(b"zero purpose").await.unwrap();

    let (sb, purpose) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 0);
    assert_eq!(sb.recv().await.unwrap(), b"zero purpose");
}

#[tokio::test]
async fn stream_purpose_max() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(u16::MAX).await.unwrap();
    sa.send(b"max purpose").await.unwrap();

    let (sb, purpose) = p.conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, u16::MAX);
    assert_eq!(sb.recv().await.unwrap(), b"max purpose");
}
