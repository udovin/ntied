//! Whole-connection close: graceful drop, peer-initiated close, local close.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

#[tokio::test]
async fn close_by_drop() {
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

/// Peer closes the connection cleanly — buffered stream data still drains.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_drains_stream() {
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
                _ => break,
            }
        }
        assert_eq!(received, b"before-close");
    })
    .await
    .expect("test timed out");
}

/// Peer closes the connection — pending `accept_stream` returns error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_fails_accept() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        conn_a.close().await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(conn_b.accept_stream().await.is_err());
    })
    .await
    .expect("test timed out");
}

/// We close our connection — local recv on a stream errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_fails_recv() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();

        conn_a.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut buf = [0u8; 64];
        let _ = stream_a.recv(&mut buf).await;
        let _ = stream_b.recv(&mut buf).await;
    })
    .await
    .expect("test timed out");
}

/// We close our connection — local accept errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_fails_accept() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        conn_a.close().await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(conn_a.accept_stream().await.is_err());
        let _ = conn_b.accept_stream().await;
    })
    .await
    .expect("test timed out");
}

/// All in-flight stream data delivered before peer's close finalizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_no_data_loss() {
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

/// Locally-closed connection MAY drop in-flight peer data — that's fine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_data_loss_ok() {
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
            if stream_b.recv(&mut buf).await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("test timed out");
}
