//! Stream (reliable, ordered) data transfer and lifecycle.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_recv() {
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
async fn drop_signals_close() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"x").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();

        drop(stream_a);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut buf = [0u8; 64];
        loop {
            match stream_b.recv(&mut buf).await {
                Ok((_, true)) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_after_first_send() {
    init_tracing();

    tokio::time::timeout(Duration::from_secs(5), async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let stream_a = conn_a.open_stream().unwrap();

        // Streams are lazy — peer learns about them on first data.
        let accept = tokio::spawn(async move { conn_b.accept_stream().await.unwrap() });

        stream_a.send(b"hello").await.unwrap();

        let stream_b = accept.await.unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_one_other_survives() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let s1_a = conn_a.open_stream().unwrap();
        let s2_a = conn_a.open_stream().unwrap();
        s1_a.send(b"chan1").await.unwrap();
        s2_a.send(b"chan2").await.unwrap();

        // Wait for data to arrive and streams to be auto-accepted.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let first = tokio::time::timeout(Duration::from_secs(5), conn_b.accept_stream())
            .await
            .expect("accept first timeout")
            .unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), conn_b.accept_stream())
            .await
            .expect("accept second timeout")
            .unwrap();

        // Match accepts to opens by stream_id.
        let (s1_b, s2_b) = if first.stream_id() == s1_a.stream_id() {
            (first, second)
        } else {
            (second, first)
        };

        // Close stream 1; stream 2 keeps working.
        drop(s1_a);
        tokio::time::sleep(Duration::from_millis(200)).await;
        s2_a.send(b"-more").await.unwrap();

        // Stream 1: drain remaining bytes, then FIN.
        let mut buf = [0u8; 1024];
        let mut received1 = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), s1_b.recv(&mut buf)).await {
                Ok(Ok((n, fin))) => {
                    if n > 0 {
                        received1.extend_from_slice(&buf[..n]);
                    }
                    if fin || n == 0 {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert_eq!(received1, b"chan1");

        // Stream 2: combined payload.
        let mut received2 = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), s2_b.recv(&mut buf)).await {
                Ok(Ok((n, _))) if n > 0 => {
                    received2.extend_from_slice(&buf[..n]);
                    if received2.len() >= 9 {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert_eq!(received2, b"chan2-more");
    })
    .await
    .expect("test timed out");
}
