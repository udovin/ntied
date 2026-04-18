use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;
use std::time::{Duration, Instant};

use ntied_transport::PrivateKey;
use ntied_transport::node_v2::{Connection, Node};

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

/// Returns (conn_a, conn_b, _node_a, _node_b).
/// Callers MUST keep node references alive for the connection to work.
async fn connect_pair() -> (Connection, Connection, Arc<Node>, Arc<Node>) {
    let node_a = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    (conn_a, conn_b, node_a, node_b)
}

// ============================================================
// Handshake
// ============================================================

#[tokio::test]
async fn handshake_completes() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;
        assert!(conn_a.peer_public_key().is_some());
        assert!(conn_b.peer_public_key().is_some());
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

    let node = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), node.connect(dead_addr))
        .await
        .unwrap();

    assert!(result.is_err());
}

// ============================================================
// Stream send/recv
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_send_recv() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"hello world").await.unwrap();

        let stream_b = conn_b.accept_stream().await.unwrap();
        let mut buf = [0u8; 1024];
        let (n, fin) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello world");
        assert!(!fin);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn stream_close_by_drop() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        // Send something so peer creates the stream.
        stream_a.send(b"x").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();

        // Drop sender side.
        drop(stream_a);
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Receiver should eventually get error or fin.
        let mut buf = [0u8; 64];
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((_, true)) => break,  // fin
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Connection close
// ============================================================

#[tokio::test]
async fn connection_close_by_drop() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let _stream_b = conn_b.accept_stream().await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream_a);
        conn_a.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Peer closes connection — channels allow draining
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_connection_drain_channel() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"before-close").await.unwrap();

        let stream_b = conn_b.accept_stream().await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        conn_a.close().await;

        let mut buf = [0u8; 1024];
        let mut received = Vec::new();
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((n, _)) if n > 0 => received.extend_from_slice(&buf[..n]),
                Ok((_, true)) => break,
                Ok(_) => break,
                Err(_) => break,
            }
        }
        assert_eq!(received, b"before-close");
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_connection_accept_stream_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        conn_a.close().await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        let result = conn_b.accept_stream().await;
        assert!(result.is_err());
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Peer closes channel — drain then error
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_channel_drain() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"hello").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream_a);

        let stream_b = conn_b.accept_stream().await.unwrap();
        let mut buf = [0u8; 1024];
        let mut received = Vec::new();
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((n, _)) if n > 0 => received.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        assert_eq!(received, b"hello");
    })
    .await
    .expect("test timed out");
}

// ============================================================
// We close our connection — recv fails
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_connection_recv_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_b = conn_b.open_stream().unwrap();
        stream_b.send(b"x").await.unwrap();
        let _stream_a = conn_a.accept_stream().await.unwrap();

        conn_b.close().await;

        let mut buf = [0u8; 64];
        let result = stream_b.recv(&mut buf).await;
        assert!(result.is_err());
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_connection_accept_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        conn_b.close().await;

        let result = conn_b.accept_stream().await;
        assert!(result.is_err());
        drop(conn_a);
    })
    .await
    .expect("test timed out");
}

// ============================================================
// We close channel — recv fails
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_channel_recv_fails() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let _stream_b = conn_b.accept_stream().await.unwrap();

        stream_a.close();

        let mut buf = [0u8; 64];
        let result = stream_a.recv(&mut buf).await;
        assert!(result.is_err());
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Multiple channels + partial close
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_one_stream_other_survives() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        // Open streams and send. Accept after a delay to let data arrive.
        let s1_a = conn_a.open_stream().unwrap();
        let s2_a = conn_a.open_stream().unwrap();
        s1_a.send(b"chan1").await.unwrap();
        s2_a.send(b"chan2").await.unwrap();

        // Wait for data to arrive and streams to be auto-accepted.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let first = tokio::time::timeout(Duration::from_secs(5), conn_b.accept_stream())
            .await.expect("accept first timeout").unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), conn_b.accept_stream())
            .await.expect("accept second timeout").unwrap();

        // Sort by stream_id.
        let (s1_b, s2_b) = if first.stream_id() == s1_a.stream_id() {
            (first, second)
        } else {
            (second, first)
        };


        // Close stream 1.
        drop(s1_a);
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Stream 2 should still work.
        s2_a.send(b"-more").await.unwrap();

        // Read from stream 1 — drain then FIN.
        let mut buf = [0u8; 1024];
        let mut received1 = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), s1_b.recv(&mut buf)).await {
                Ok(Ok((n, fin))) => {
                    if n > 0 { received1.extend_from_slice(&buf[..n]); }
                    if fin || n == 0 { break; }
                }
                _ => break,
            }
        }
        assert_eq!(received1, b"chan1");

        // Read from stream 2.
        let mut received2 = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), s2_b.recv(&mut buf)).await {
                Ok(Ok((n, _))) if n > 0 => {
                    received2.extend_from_slice(&buf[..n]);
                    if received2.len() >= 9 { break; }
                }
                _ => break,
            }
        }
        assert_eq!(received2, b"chan2-more");
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Datagram channel
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn datagram_send_recv() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let dg_a = conn_a.open_channel().unwrap();
        dg_a.send(b"dgram".to_vec()).await.unwrap();

        let dg_b = conn_b.accept_channel().await.unwrap();
        let data = dg_b.recv().await.unwrap();
        assert_eq!(data, b"dgram");
    })
    .await
    .expect("test timed out");
}

// ============================================================
// Data loss / preservation
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_connection_no_data_loss() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        for i in 0..10 {
            stream_a.send(format!("msg{i}").as_bytes()).await.unwrap();
        }

        let stream_b = conn_b.accept_stream().await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        conn_a.close().await;

        let mut buf = [0u8; 4096];
        let mut received = Vec::new();
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((n, _)) if n > 0 => received.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        let expected = (0..10).map(|i| format!("msg{i}")).collect::<String>();
        assert_eq!(received, expected.as_bytes());
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_channel_no_data_loss() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        for i in 0..10 {
            stream_a.send(format!("chunk{i}").as_bytes()).await.unwrap();
        }

        let stream_b = conn_b.accept_stream().await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream_a);

        let mut buf = [0u8; 4096];
        let mut received = Vec::new();
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((n, _)) if n > 0 => received.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        let expected = (0..10).map(|i| format!("chunk{i}")).collect::<String>();
        assert_eq!(received, expected.as_bytes());
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_connection_data_loss_ok() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();

        stream_a.send(b"some-data").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        conn_b.close().await;

        let mut buf = [0u8; 64];
        loop {
            match stream_b.recv(&mut buf).await {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_channel_data_loss_ok() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();

        stream_a.send(b"some-data").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        stream_b.close();

        let mut buf = [0u8; 64];
        loop {
            match stream_b.recv(&mut buf).await {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await
    .expect("test timed out");
}

// ============================================================
// accept_stream/channel before send
// ============================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_stream_after_first_send() {
    init_tracing();

    tokio::time::timeout(Duration::from_secs(5), async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();

        // Streams are lazy — peer learns about them on first data.
        // accept_stream works after first send triggers stream creation on peer.
        let accept = tokio::spawn(async move {
            conn_b.accept_stream().await.unwrap()
        });

        stream_a.send(b"hello").await.unwrap();

        let stream_b = accept.await.unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    })
    .await
    .expect("accept_stream_after_first_send timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_channel_after_first_send() {
    init_tracing();

    tokio::time::timeout(Duration::from_secs(5), async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let channel_a = conn_a.open_channel().unwrap();

        // Channels send ChannelOpen on first use — peer learns about them.
        let accept = tokio::spawn(async move {
            conn_b.accept_channel().await.unwrap()
        });

        channel_a.send(b"hello".to_vec()).await.unwrap();

        let channel_b = accept.await.unwrap();
        let msg = channel_b.recv().await.unwrap();
        assert_eq!(msg, b"hello");
    })
    .await
    .expect("accept_channel_after_first_send timed out");
}
