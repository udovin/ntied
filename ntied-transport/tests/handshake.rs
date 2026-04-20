//! V2 handshake: completion, timeouts.

mod common;

use std::time::Duration;

use ntied_transport::PrivateKey;
use ntied_transport::node::Node;

use common::{TEST_TIMEOUT, connect_pair, init_tracing, localhost};

#[tokio::test]
async fn handshake_completes() {
    init_tracing();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (conn_a, conn_b, _na, _nb) = connect_pair().await;
        assert!(conn_a.peer_public_key().is_some());
        assert!(conn_b.peer_public_key().is_some());
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
