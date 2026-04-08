use std::net::SocketAddr;
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
