//! Integration tests for BitTorrent-DHT discovery against a local
//! `mainline::Testnet` (no traffic to the real public DHT).
//!
//! Each test spins up a fresh 5-node testnet (10–50 ms on a hot machine)
//! and configures `Discovery` with `use_default_bootstrap = false` so the
//! actor only talks to the testnet.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mainline::Testnet;
use ntied_transport::PeerId;
use ntied_transport::PrivateKey;
use ntied_transport::discovery::PeerRoutes;
use ntied_transport::node::{DiscoveryConfig, Node};

use common::{TEST_TIMEOUT, init_tracing, localhost};

/// Poll `lookup_peer` every 200 ms until a route appears or `deadline` is
/// reached.  DHT propagation in testnet is usually sub-second but not
/// always synchronous.
async fn lookup_peer_until(
    node: &Node,
    peer_id: PeerId,
    deadline: Duration,
) -> PeerRoutes {
    let start = std::time::Instant::now();
    loop {
        let routes = node.lookup_peer(peer_id).await.unwrap();
        if !routes.is_empty() {
            return routes;
        }
        if start.elapsed() >= deadline {
            return routes;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn parse_bootstrap(testnet: &Testnet) -> Vec<SocketAddr> {
    testnet
        .bootstrap
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// `DiscoveryConfig` that talks **only** to the given testnet.  Top-up loop
/// is effectively disabled by setting `relay_target = 0`, so tests that
/// don't exercise it aren't perturbed by background activity.
fn testnet_config(bootstrap: Vec<SocketAddr>) -> DiscoveryConfig {
    DiscoveryConfig {
        extra_bootstrap: bootstrap,
        use_default_bootstrap: false,
        relay_target: 0,
        topup_interval: Duration::from_secs(60),
        grace_period: Duration::from_secs(60),
    }
}

/// Peer publishes its direct addr; another node looks it up and finds the
/// announcing peer's socket address.
#[tokio::test]
async fn announce_and_lookup_peer_direct() {
    init_tracing();
    let testnet = Testnet::new_async(5).await.unwrap();
    let bootstrap = parse_bootstrap(&testnet);

    tokio::time::timeout(TEST_TIMEOUT, async {
        // Publisher.
        let node_a = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        node_a
            .enable_discovery(testnet_config(bootstrap.clone()))
            .await
            .unwrap();
        node_a.enable_public_peer().await.unwrap();

        // Give the DHT a moment to spread the announce.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Lookup from a different node.
        let node_b = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        node_b
            .enable_discovery(testnet_config(bootstrap))
            .await
            .unwrap();
        let routes = node_b.lookup_peer(node_a.peer_id()).await.unwrap();
        assert!(
            !routes.direct.is_empty(),
            "expected direct route for A, got {routes:?}"
        );
        assert!(
            routes.via_relay.is_empty(),
            "A is not behind a relay, got {routes:?}"
        );
    })
    .await
    .unwrap();
}

/// A node calls `enable_public_relay`; another node finds it via
/// `lookup_relays`.
#[tokio::test]
async fn announce_and_lookup_relays() {
    init_tracing();
    let testnet = Testnet::new_async(5).await.unwrap();
    let bootstrap = parse_bootstrap(&testnet);

    tokio::time::timeout(TEST_TIMEOUT, async {
        let relay = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        relay
            .enable_discovery(testnet_config(bootstrap.clone()))
            .await
            .unwrap();
        relay.enable_public_relay().await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;

        let client = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        client
            .enable_discovery(testnet_config(bootstrap))
            .await
            .unwrap();
        let relays = client.lookup_relays().await.unwrap();
        assert!(!relays.is_empty(), "expected at least one relay registered");
    })
    .await
    .unwrap();
}

/// Full pipeline: relay serves, peer B attaches (relay publishes B in DHT),
/// peer A discovers B by `peer_id` and connects through the relay.
#[tokio::test]
async fn connect_peer_end_to_end_via_dht() {
    init_tracing();
    let testnet = Testnet::new_async(5).await.unwrap();
    let bootstrap = parse_bootstrap(&testnet);

    tokio::time::timeout(Duration::from_secs(60), async {
        // -- Relay ----------------------------------------------------
        let relay = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        let relay_addr = relay.local_addr().unwrap();
        relay
            .enable_discovery(testnet_config(bootstrap.clone()))
            .await
            .unwrap();
        let r = relay.clone();
        let _relay_task = tokio::spawn(async move {
            let _ = r.serve_as_relay().await;
        });

        // -- Peer B: attaches to relay -------------------------------
        let node_b = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        let b_peer_id = node_b.peer_id();
        node_b
            .enable_discovery(testnet_config(bootstrap.clone()))
            .await
            .unwrap();
        node_b.attach_relay(relay_addr).await.unwrap();
        node_b
            .wait_relay_connected(relay_addr, Duration::from_secs(5))
            .await
            .unwrap();

        let nb = node_b.clone();
        let accept_b = tokio::spawn(async move { nb.accept().await.unwrap() });

        // -- Peer A: discovers B and connects ------------------------
        let node_a = Arc::new(
            Node::bind(localhost(), PrivateKey::generate())
                .await
                .unwrap(),
        );
        node_a
            .enable_discovery(testnet_config(bootstrap))
            .await
            .unwrap();

        // Poll until the relay's announce_peer_via_relay has propagated.
        // (`serve_as_relay` does it after registering B; can race
        // wait_relay_connected by a few hundred ms.)
        let routes = lookup_peer_until(&node_a, b_peer_id, Duration::from_secs(15)).await;
        assert!(
            !routes.via_relay.is_empty(),
            "expected via_relay route for B, got {routes:?}"
        );

        let conn_a = node_a.connect_peer(b_peer_id).await.unwrap();
        let conn_b = accept_b.await.unwrap();

        assert_eq!(conn_a.peer_id(), Some(node_b.peer_id()));
        assert_eq!(conn_b.peer_id(), Some(node_a.peer_id()));

        // Quick roundtrip over a stream to prove the tunnel works.
        let stream_a = conn_a.open_stream().unwrap();
        stream_a.send(b"hi via DHT").await.unwrap();
        let stream_b = conn_b.accept_stream().await.unwrap();
        let mut buf = [0u8; 32];
        let (n, _fin) = stream_b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hi via DHT");
    })
    .await
    .unwrap();
}
