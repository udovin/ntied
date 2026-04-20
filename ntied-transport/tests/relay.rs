//! Relay-routed connections + hole-punch upgrade to direct.

mod common;

use std::sync::Arc;
use std::time::Duration;

use ntied_transport::PrivateKey;
use ntied_transport::node::Node;

use common::{TEST_TIMEOUT, init_tracing, localhost};

/// A connects to B through a relay; bidirectional stream payload roundtrips.
#[tokio::test]
async fn two_peers_stream() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_relay = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let relay_addr = node_relay.local_addr().unwrap();
        let nr = node_relay.clone();
        let relay_task = tokio::spawn(async move {
            let _ = nr.serve_as_relay().await;
        });

        // B attaches to relay so A can reach it.
        let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let b_peer_id = node_b.peer_id();
        node_b.attach_relay(relay_addr).await.unwrap();
        let nb = node_b.clone();
        let accept_b = tokio::spawn(async move { nb.accept().await.unwrap() });

        // A → B via relay.
        let node_a = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let conn_a = node_a
            .connect_via_relay(b_peer_id, relay_addr)
            .await
            .unwrap();
        let conn_b = accept_b.await.unwrap();

        assert_eq!(conn_a.peer_id(), Some(node_b.peer_id()));
        assert_eq!(conn_b.peer_id(), Some(node_a.peer_id()));

        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"hello via relay").await.unwrap();

        let stream_b = conn_b.accept_stream().await.unwrap();
        let mut buf = [0u8; 64];
        let (n, _fin) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello via relay");

        relay_task.abort();
    })
    .await
    .expect("test timed out");
}

/// After try_direct(), both peers swap from relay to direct UDP, transparently.
#[tokio::test]
async fn upgrade_to_direct() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let node_relay = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let relay_addr = node_relay.local_addr().unwrap();
        let nr = node_relay.clone();
        let relay_task = tokio::spawn(async move {
            let _ = nr.serve_as_relay().await;
        });

        let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let b_peer_id = node_b.peer_id();
        node_b.attach_relay(relay_addr).await.unwrap();
        let nb = node_b.clone();
        let accept_b = tokio::spawn(async move { nb.accept().await.unwrap() });

        let node_a = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
        let conn_a = node_a
            .connect_via_relay(b_peer_id, relay_addr)
            .await
            .unwrap();
        let conn_b = accept_b.await.unwrap();

        assert!(!conn_a.is_using_direct_path());
        assert!(!conn_b.is_using_direct_path());

        conn_a.try_direct().await.unwrap();
        conn_b.try_direct().await.unwrap();

        // Drive traffic so the per-connection main loop iterates and probes.
        let stream_a = conn_a.open_stream().unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();
        for i in 0..40u32 {
            stream_a.send(format!("ping-{i}").as_bytes()).await.unwrap();
            let mut buf = [0u8; 64];
            let _ = stream_b.recv(&mut buf).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            if conn_a.is_using_direct_path() && conn_b.is_using_direct_path() {
                break;
            }
        }

        assert!(conn_a.is_using_direct_path(), "A did not upgrade");
        assert!(conn_b.is_using_direct_path(), "B did not upgrade");

        // Traffic still flows after the swap.
        stream_a.send(b"after-upgrade").await.unwrap();
        let mut buf = [0u8; 64];
        let (n, _fin) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"after-upgrade");

        relay_task.abort();
    })
    .await
    .expect("test timed out");
}
