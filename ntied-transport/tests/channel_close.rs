//! Per-channel close: dropping a locally-opened channel must not leave a
//! stale accept candidate queued on our side.

mod common;

use std::time::Duration;

use common::{TEST_TIMEOUT, connect_pair, init_tracing};

/// Regression: after A opens a channel, exchanges data, and drops its handle,
/// A's own `accept_channel()` must not return a handle for that same id.
///
/// The bug surfaces a locally-initiated channel id through
/// `drain_updated_channels` (via `close_send` / `try_cleanup`) after
/// `Channel::drop` has already removed its entry from `channel_notifies`.
/// The node-level accept loop then treats the id as a fresh peer-initiated
/// channel and puts a stale handle into the accept queue; a later
/// `channel_send` on that handle trips `IdReused` inside `ChannelManager`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_local_channel_not_re_accepted() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;

        // A opens a channel, sends one message, B receives it.
        let ch_a = conn_a.open_channel().unwrap();
        ch_a.send(b"hello".to_vec()).await.unwrap();

        let ch_b = conn_b.accept_channel().await.unwrap();
        let msg = ch_b.recv().await.unwrap();
        assert_eq!(msg, b"hello");

        // Drop both sides so the channel goes through close_send + FIN
        // exchange on both peers. We want A's accept loop to tick at
        // least once while the close state changes are visible.
        drop(ch_a);
        drop(ch_b);

        // Give the connection time to process close/fin and for A's
        // accept loop to observe `drain_updated_channels` surfacing the
        // dropped id.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A has not opened anything new; nobody has opened a channel
        // to A. `accept_channel` must therefore block, not return a
        // stale handle pointing at the dropped id.
        let accept_res =
            tokio::time::timeout(Duration::from_millis(300), conn_a.accept_channel()).await;
        assert!(
            accept_res.is_err(),
            "accept_channel returned a stale handle after local drop: {:?}",
            accept_res.map(|r| r.map(|c| c.channel_id())),
        );
    })
    .await
    .expect("test timed out");
}
