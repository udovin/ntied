use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use ntied_transport::PrivateKey;
use ntied_transport::node_v2::{Node, Connection};

static TRACING_INIT: Once = Once::new();

fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
            )
            .with_target(false)
            .with_test_writer()
            .init();
    });
}

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn handshake_completes() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn connect_timeout() {
    init_tracing();

    let dead_socket = tokio::net::UdpSocket::bind(localhost()).await.unwrap();
    let dead_addr = dead_socket.local_addr().unwrap();

    let node = Node::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), node.connect(dead_addr))
        .await
        .unwrap();

    match result {
        Ok(_) => panic!("expected timeout error"),
        Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::TimedOut),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_send_recv() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let stream_b = conn_b.accept_stream().await.unwrap();
            assert_eq!(stream_b.purpose(), 42);
            let data = stream_b.recv().await.unwrap();
            assert_eq!(data, b"hello world");
            conn_b
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let stream_a = conn_a.open_stream(42);
        stream_a.write(b"hello world").unwrap();

        let conn_b = accept.await.unwrap();
        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn stream_close_by_drop() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let Ok(stream_b) = conn_b.accept_stream().await else {
                return conn_b; // channel already closed before accept — ok
            };
            let result = stream_b.recv().await;
            assert!(result.is_err(), "expected channel closed error");
            conn_b
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let stream_a = conn_a.open_stream(1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream_a);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let conn_b = accept.await.unwrap();
        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn connection_close_by_drop() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let Ok(stream_b) = conn_b.accept_stream().await else {
                return; // connection closed before accept — ok
            };
            match stream_b.recv().await {
                Err(_) => {} // connection closed — expected
                Ok(_) => panic!("expected connection closed error"),
            }
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let _stream_a = conn_a.open_stream(1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(_stream_a);
        conn_a.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        accept.await.unwrap();
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Scenario 1: Peer closes connection — channels allow draining
// ============================================================

/// Peer closes connection. We should be able to read all data
/// that was sent before close, then recv returns error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_connection_drain_channel() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let stream_b = conn_b.accept_stream().await.unwrap();

            // Read all data, then expect closed
            let mut received = Vec::new();
            loop {
                match stream_b.recv().await {
                    Ok(data) => received.extend_from_slice(&data),
                    Err(_) => break,
                }
            }
            assert_eq!(received, b"before-close");

            // accept_stream should also return error now
            let result = conn_b.accept_stream().await;
            assert!(result.is_err(), "accept_stream should fail after peer close");
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let stream_a = conn_a.open_stream(1);
        stream_a.write(b"before-close").unwrap();

        // Give time for data to arrive
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Close connection — peer should still be able to drain
        conn_a.close().await;

        accept.await.unwrap();
    })
    .await
    .expect("test timed out");
}

/// Peer closes connection. accept_stream returns error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_connection_accept_stream_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            // Don't open any channels, just wait for connection to close
            tokio::time::sleep(Duration::from_millis(300)).await;
            let result = conn_b.accept_stream().await;
            assert!(result.is_err(), "accept_stream should fail after peer closed connection");
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        conn_a.close().await;

        accept.await.unwrap();
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Scenario 2: Peer closes channel — drain then error
// ============================================================

/// Peer closes channel after sending data. We should read all
/// data, then recv returns error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_channel_drain() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let stream_b = conn_b.accept_stream().await.unwrap();

            let mut received = Vec::new();
            loop {
                match stream_b.recv().await {
                    Ok(data) => received.extend_from_slice(&data),
                    Err(_) => break,
                }
            }
            assert_eq!(received, b"hello");
            conn_b
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let stream_a = conn_a.open_stream(1);
        stream_a.write(b"hello").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream_a); // close channel

        let conn_b = accept.await.unwrap();
        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Scenario 3: We close connection — recv fails
// ============================================================

/// We close our own connection. Our channel recv should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_connection_recv_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_b = Arc::new(node_b);
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        // B opens a stream to A
        let stream_b = conn_b.open_stream(1);
        let _stream_a = conn_a.accept_stream().await.unwrap();

        // B closes its own connection
        conn_b.close().await;

        // B's stream recv should fail
        let result = stream_b.recv().await;
        assert!(result.is_err(), "recv should fail after we closed our connection");
    })
    .await
    .expect("test timed out");
}

/// We close connection. accept_stream should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_connection_accept_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_b = Arc::new(node_b);
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        conn_b.close().await;

        let result = conn_b.accept_stream().await;
        assert!(result.is_err(), "accept_stream should fail after we closed connection");
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Scenario 4: We close channel — recv fails
// ============================================================

/// We close our own channel. recv should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_channel_recv_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_b = Arc::new(node_b);
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        let stream_a = conn_a.open_stream(1);
        let _stream_b = conn_b.accept_stream().await.unwrap();

        // Close our channel explicitly
        stream_a.close().await;

        // recv on our closed channel should fail
        let result = stream_a.recv().await;
        assert!(result.is_err(), "recv should fail after we closed the channel");

        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Multiple channels + partial close
// ============================================================

/// Two channels open. Close one, other keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_one_channel_other_survives() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let node_b = Arc::new(node_b);
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let conn_b = accept.await.unwrap();

        let stream1_a = conn_a.open_stream(1);
        let stream2_a = conn_a.open_stream(2);

        let stream1_b = conn_b.accept_stream().await.unwrap();
        let stream2_b = conn_b.accept_stream().await.unwrap();

        // Write to both
        stream1_a.write(b"chan1").unwrap();
        stream2_a.write(b"chan2").unwrap();

        // Close channel 1
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream1_a);

        // Channel 2 should still work
        tokio::time::sleep(Duration::from_millis(100)).await;
        stream2_a.write(b"-more").unwrap();

        // Read from channel 1 — drain then error
        let mut received1 = Vec::new();
        loop {
            match stream1_b.recv().await {
                Ok(data) => received1.extend_from_slice(&data),
                Err(_) => break,
            }
        }
        assert_eq!(received1, b"chan1");

        // Read from channel 2 — should get both writes
        let mut received2 = Vec::new();
        let data = stream2_b.recv().await.unwrap();
        received2.extend_from_slice(&data);
        let data = stream2_b.recv().await.unwrap();
        received2.extend_from_slice(&data);
        assert_eq!(received2, b"chan2-more");

        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Datagram channel close behavior
// ============================================================

/// Datagram channel: peer close allows drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn datagram_peer_close_drain() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
        let addr_b = node_b.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let conn_b = node_b.accept().await.unwrap();
            let dg_b = conn_b.accept_datagram().await.unwrap();

            let mut received = Vec::new();
            loop {
                match dg_b.recv().await {
                    Ok(data) => received.push(data),
                    Err(_) => break,
                }
            }
            assert_eq!(received.len(), 1);
            assert_eq!(received[0], b"dgram");
            conn_b
        });

        let conn_a = node_a.connect(addr_b).await.unwrap();
        let dg_a = conn_a.open_datagram(1);
        dg_a.write(b"dgram").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(dg_a); // close datagram channel

        let conn_b = accept.await.unwrap();
        drop(conn_a);
        drop(conn_b);
    })
    .await
    .expect("test timed out");
}
