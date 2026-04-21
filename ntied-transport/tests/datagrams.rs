//! Datagram channel (unreliable, message-oriented) data transfer.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_recv() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accept_after_first_send() {
    init_tracing();

    tokio::time::timeout(Duration::from_secs(5), async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        let channel_a = conn_a.open_channel().unwrap();

        // ChannelOpen is sent on first use; peer learns about it.
        // Return `conn_b` out of the spawn: dropping it would cancel its
        // cancel_token and abort `channel_b.recv()` below, races observed.
        let accept = tokio::spawn(async move {
            let ch = conn_b.accept_channel().await.unwrap();
            (ch, conn_b)
        });

        channel_a.send(b"hello".to_vec()).await.unwrap();

        let (channel_b, _conn_b) = accept.await.unwrap();
        let msg = channel_b.recv().await.unwrap();
        assert_eq!(msg, b"hello");
    })
    .await
    .expect("test timed out");
}
