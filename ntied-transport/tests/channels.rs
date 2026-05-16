//! Per-message reliable/unreliable channels.
//!
//! - `send`: reliable.  Delivery guaranteed.
//! - `send_unreliable`: best-effort.  Transport auto-evicts the oldest
//!   unreliable in-flight message when the local send buffer is full,
//!   signalling the peer via `ChannelEvict`.
//! - Per-channel byte window managed via `ChannelMaxData`.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reliable_roundtrip() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let ch_a = conn_a.open_channel().unwrap();
        ch_a.send(b"reliable".to_vec()).await.unwrap();

        let ch_b = conn_b.accept_channel().await.unwrap();
        let data = ch_b.recv().await.unwrap();
        assert_eq!(data, b"reliable");
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_reliable_messages_arrive_intact() {
    // Reliable messages should all arrive.  Order is not guaranteed across
    // messages on a channel (channels are unordered), but each message is
    // intact and complete.
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let ch_a = conn_a.open_channel().unwrap();
        for i in 0..16u32 {
            ch_a.send(i.to_be_bytes().to_vec()).await.unwrap();
        }

        let ch_b = conn_b.accept_channel().await.unwrap();
        let mut got = Vec::new();
        for _ in 0..16 {
            let msg = ch_b.recv().await.unwrap();
            assert_eq!(msg.len(), 4);
            let n = u32::from_be_bytes(msg.try_into().unwrap());
            got.push(n);
        }
        got.sort();
        assert_eq!(got, (0..16u32).collect::<Vec<_>>());
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_reliable_unreliable() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let ch_a = conn_a.open_channel().unwrap();
        ch_a.send(b"r1".to_vec()).await.unwrap();
        ch_a.send_unreliable(b"u1".to_vec()).await.unwrap();
        ch_a.send(b"r2".to_vec()).await.unwrap();

        let ch_b = conn_b.accept_channel().await.unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(ch_b.recv().await.unwrap());
        }
        // All three should arrive (no buffer pressure, no evictions).
        got.sort();
        assert_eq!(got, vec![b"r1".to_vec(), b"r2".to_vec(), b"u1".to_vec()]);
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_reliable_message_fragmented() {
    // Single reliable message larger than UDP MTU — exercises fragmentation
    // + assembly + the per-channel flow control allowing the full message
    // through (initial window must be ≥ message size).
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let ch_a = conn_a.open_channel().unwrap();
        let payload: Vec<u8> = (0..32_000u32).map(|i| (i & 0xFF) as u8).collect();
        ch_a.send(payload.clone()).await.unwrap();

        let ch_b = conn_b.accept_channel().await.unwrap();
        let data = ch_b.recv().await.unwrap();
        assert_eq!(data, payload);
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_recv_handle_does_not_block_send_side_cleanup() {
    // Receiver drops handle without polling all delivered messages.  Sender
    // closes its side.  After both sides close, channel must eventually
    // be removed (not leak).
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let ch_a = conn_a.open_channel().unwrap();
        ch_a.send(b"one".to_vec()).await.unwrap();
        ch_a.send(b"two".to_vec()).await.unwrap();

        let ch_b = conn_b.accept_channel().await.unwrap();
        // Read only one, drop the handle without reading the other.
        let _ = ch_b.recv().await.unwrap();
        drop(ch_b);

        // Closing sender should not panic / deadlock.
        drop(ch_a);

        // Give some time for fin handshake to complete on background tasks.
        tokio::time::sleep(Duration::from_millis(200)).await;
    })
    .await
    .expect("test timed out");
}
