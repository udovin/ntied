//! Per-stream close: drop on sender, local close, data preservation across FIN.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

/// Sender drops the stream — receiver drains buffered data, then sees FIN.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_drains() {
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

/// Local close on the sender — local recv on the same stream errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_close_fails_recv() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let _stream_b = conn_b.accept_stream().await.unwrap();

        stream_a.close();

        let mut buf = [0u8; 64];
        assert!(stream_a.recv(&mut buf).await.is_err());
    })
    .await
    .expect("test timed out");
}

/// All sent bytes delivered to peer even if sender drops mid-pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_close_no_data_loss() {
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

/// Local close on the receiver MAY discard in-flight peer data — that's fine.
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

        stream_b.close();

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
