//! Shared helpers for integration tests. Each test binary uses a subset
//! of these, so suppress the resulting "unused" warnings here.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;

use ntied_transport::PrivateKey;
use ntied_transport::node::{Connection, Node};

static TRACING_INIT: Once = Once::new();

pub fn init_tracing() {
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

pub fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Two peers connected directly. Caller must keep the `Arc<Node>` handles
/// alive — they own the underlying recv loop.
pub async fn connect_pair() -> (Connection, Connection, Arc<Node>, Arc<Node>) {
    let node_a = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    (conn_a, conn_b, node_a, node_b)
}
