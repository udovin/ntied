//! Shared helpers for `ntied` integration tests.
//!
//! Tests run against a local `mainline::Testnet` so they don't depend on
//! the real public BitTorrent DHT.  [`testnet_config`] builds a
//! `DiscoveryConfig` that bootstraps only from the testnet.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use mainline::Testnet;
use ntied_server::RelayNode;
use ntied_transport::node::DiscoveryConfig;
use ntied_transport::PrivateKey;
use tokio::task::JoinHandle;

/// Bundle of shared state owned by a single test: a 5-node DHT testnet, a
/// running relay, and the bootstrap list used to point new transports at
/// that testnet.  Drop the bundle to tear everything down (the relay
/// task is aborted, the testnet shuts itself down with its `Dht` clones).
pub struct TestEnv {
    pub server_addr: SocketAddr,
    pub bootstrap: Vec<SocketAddr>,
    pub _testnet: Testnet,
    pub relay_task: JoinHandle<()>,
}

impl TestEnv {
    /// Build a `DiscoveryConfig` that bootstraps only from this env's
    /// testnet — no traffic to the real mainline DHT.
    pub fn discovery_config(&self) -> DiscoveryConfig {
        DiscoveryConfig {
            extra_bootstrap: self.bootstrap.clone(),
            use_default_bootstrap: false,
            // Tests don't exercise the discovery-pool top-up loop; keep
            // it quiet so it doesn't compete with the test relay.
            relay_target: 0,
            topup_interval: Duration::from_secs(60),
            grace_period: Duration::from_secs(60),
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        self.relay_task.abort();
    }
}

/// Spin up a 5-node testnet and start a relay configured to publish in
/// it.  Returns immediately after the relay has bound; the relay task
/// runs until the returned `TestEnv` is dropped.
pub async fn start_test_env() -> TestEnv {
    let testnet = Testnet::new_async(5).await.unwrap();
    let bootstrap: Vec<SocketAddr> = testnet
        .bootstrap
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let config = DiscoveryConfig {
        extra_bootstrap: bootstrap.clone(),
        use_default_bootstrap: false,
        relay_target: 0,
        topup_interval: Duration::from_secs(60),
        grace_period: Duration::from_secs(60),
    };
    let relay = RelayNode::bind_with_discovery(
        "127.0.0.1:0".parse().unwrap(),
        PrivateKey::generate(),
        config,
    )
    .await
    .unwrap();
    let server_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move {
        let _ = relay.run().await;
    });
    TestEnv {
        server_addr,
        bootstrap,
        _testnet: testnet,
        relay_task,
    }
}
