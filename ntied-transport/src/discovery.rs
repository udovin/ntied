//! BitTorrent DHT–based discovery.
//!
//! Maps `PeerId` ↔ reachability information (direct white-IP addrs and/or
//! relay addrs) on top of the public mainline DHT (BEP-5 `announce_peer` /
//! `get_peers`).  Discovery is *advisory*: the actual connection handshake
//! verifies the peer's full public key against `PeerId`, so a poisoned DHT
//! record at worst causes a failed handshake (DoS-by-bad-address), never a
//! security compromise.
//!
//! # Layout — three info_hashes
//!
//! ```text
//! H_peer_direct(peer_id) = sha1("ntied:peer:direct:v1" || peer_id)
//!     announced by:  the peer itself (only if it has a public IPv4)
//!     stored value:  (peer_ip, peer_port)
//!
//! H_peer_relay(peer_id)  = sha1("ntied:peer:relay:v1"  || peer_id)
//!     announced by:  every relay this peer is attached to
//!     stored value:  (relay_ip, relay_port)  ← BEP-5 records the announcer
//!
//! H_relays               = sha1("ntied:relay:v1")
//!     announced by:  every relay
//!     stored value:  (relay_ip, relay_port)  ← open registry
//! ```
//!
//! # Refresh
//!
//! BEP-5 storage expires ~30 min after the last `announce_peer`.  Each
//! `announce_*` call registers the (info_hash, port) pair with a background
//! task that re-announces every 25 min until the [`Discovery`] is dropped.
//!
//! # Limitations
//!
//! - mainline reports peer addresses as IPv4 only.  IPv6-only peers cannot
//!   be discovered this way; they need to be direct-reachable or bridge via
//!   a dual-stack relay.
//! - Anyone can announce on any info_hash.  Authentication is at the
//!   transport handshake layer (peer's full pubkey vs `PeerId` hash).
//! - DHT lookups reveal `peer_id → addr` correlations to any DHT observer.
//!   This is inherent to public-DHT discovery; do not enable for users who
//!   need traffic-pattern privacy.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use mainline::async_dht::{AsyncDht, GetStream};
use mainline::{Dht, Id};
use sha1_smol::Sha1;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::crypto::PeerId;

/// Re-announce period.  BEP-5 storage timeout is ~30 min; we refresh a bit
/// before that.
const REFRESH_INTERVAL: Duration = Duration::from_secs(25 * 60);

/// Info-hash prefixes — versioned so we can roll a new scheme later without
/// colliding with old records still in the DHT.
const PEER_DIRECT_PREFIX: &[u8] = b"ntied:peer:direct:v1";
const PEER_RELAY_PREFIX: &[u8] = b"ntied:peer:relay:v1";
const RELAY_PREFIX: &[u8] = b"ntied:relay:v1";

fn sha1(parts: &[&[u8]]) -> Id {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    let digest = h.digest().bytes();
    Id::from_bytes(digest).expect("sha1 digest is 20 bytes")
}

/// Info-hash for "direct addr of `peer_id`".  Announced by the peer itself.
pub fn h_peer_direct(peer_id: PeerId) -> Id {
    sha1(&[PEER_DIRECT_PREFIX, peer_id.as_bytes()])
}

/// Info-hash for "relays via which `peer_id` is reachable".  Announced by
/// each attached relay (so the DHT records the relay's IP).
pub fn h_peer_relay(peer_id: PeerId) -> Id {
    sha1(&[PEER_RELAY_PREFIX, peer_id.as_bytes()])
}

/// Info-hash for the global "any relay" registry.  Announced by every relay.
pub fn h_relays() -> Id {
    sha1(&[RELAY_PREFIX])
}

/// Routes returned by [`Discovery::lookup_peer`].
#[derive(Debug, Default, Clone)]
pub struct PeerRoutes {
    /// Addresses announced by the peer itself.  Try these as direct UDP
    /// targets (`Node::connect`).
    pub direct: Vec<SocketAddr>,
    /// Addresses announced by relays serving this peer.  Try these as
    /// relays (`Node::connect_relay_peer(addr, peer_id)`).
    pub via_relay: Vec<SocketAddr>,
}

impl PeerRoutes {
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.via_relay.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
struct RefreshItem {
    info_hash: Id,
    port: u16,
}

struct RefreshState {
    items: Vec<RefreshItem>,
    handle: Option<JoinHandle<()>>,
}

/// BitTorrent-DHT–backed discovery handle.
///
/// Construct with [`Discovery::new`]; share via [`Arc`].  Drop to shut down
/// the DHT client and the refresh task.
pub struct Discovery {
    dht: AsyncDht,
    cancel: CancellationToken,
    refresh: Arc<Mutex<RefreshState>>,
}

impl Discovery {
    /// Spin up a DHT client.
    ///
    /// `extra_bootstrap` is the list of bootstrap addresses.  When
    /// `use_default_bootstrap` is `true` (the production-typical case), it
    /// is *appended* to mainline's built-in defaults
    /// (`router.bittorrent.com:6881`, `dht.transmissionbt.com:6881`, …).
    /// When `false`, only `extra_bootstrap` is used — useful for tests
    /// against an isolated [`mainline::Testnet`].
    ///
    /// The DHT actor runs on its own thread and owns its own UDP socket
    /// (separate from the transport socket).  Bootstrap is asynchronous; the
    /// first announce/lookup may return empty results until the routing
    /// table fills in (a few seconds typically).
    pub fn new(
        extra_bootstrap: &[SocketAddr],
        use_default_bootstrap: bool,
    ) -> std::io::Result<Self> {
        let mut builder = Dht::builder();
        let bs: Vec<String> = extra_bootstrap.iter().map(|a| a.to_string()).collect();
        if !use_default_bootstrap {
            // Replace defaults entirely (isolated mode).
            builder.bootstrap(&bs);
        } else if !bs.is_empty() {
            // Append to defaults.
            builder.extra_bootstrap(&bs);
        }
        let dht = builder.build()?.as_async();
        Ok(Self {
            dht,
            cancel: CancellationToken::new(),
            refresh: Arc::new(Mutex::new(RefreshState {
                items: Vec::new(),
                handle: None,
            })),
        })
    }

    /// True after the initial routing-table fill-in completes.  Useful to
    /// gate the first announce so it propagates.
    pub async fn bootstrapped(&self) -> bool {
        self.dht.bootstrapped().await
    }

    // -- Generic announce -----------------------------------------------------

    /// Best-effort first announce, then register the (info_hash, port) pair
    /// for periodic re-announce (every [`REFRESH_INTERVAL`]) until shutdown.
    async fn announce(&self, info_hash: Id, port: u16) {
        match self.dht.announce_peer(info_hash, Some(port)).await {
            Ok(_) => debug!(?info_hash, port, "dht announce ok"),
            Err(e) => warn!(?e, ?info_hash, port, "dht announce failed"),
        }
        let mut state = self.refresh.lock().await;
        state.items.push(RefreshItem { info_hash, port });
        if state.handle.is_none() {
            let dht = self.dht.clone();
            let state_arc = self.refresh.clone();
            let cancel = self.cancel.clone();
            state.handle = Some(tokio::spawn(refresh_loop(dht, state_arc, cancel)));
        }
    }

    // -- High-level announce wrappers ----------------------------------------

    /// Publish "I am `peer_id` and I can be reached directly at this UDP
    /// port (on the address the DHT sees me from)."  Only meaningful when
    /// the local node has a public IPv4.
    pub async fn announce_self_direct(&self, peer_id: PeerId, port: u16) {
        self.announce(h_peer_direct(peer_id), port).await;
    }

    /// Called on the relay side: publish "`peer_id` is reachable via this
    /// relay" (the DHT records the relay's own IP:port).
    pub async fn announce_peer_via_relay(&self, peer_id: PeerId, relay_port: u16) {
        self.announce(h_peer_relay(peer_id), relay_port).await;
    }

    /// Called on the relay side: publish ourselves in the global relay
    /// registry.
    pub async fn announce_self_as_relay(&self, port: u16) {
        self.announce(h_relays(), port).await;
    }

    /// Stop re-announcing the given (info_hash, port) pair.  No-op if it
    /// was never registered.  The DHT entry will age out naturally after
    /// ~30 min.
    pub async fn stop_announce(&self, info_hash: Id, port: u16) {
        let mut state = self.refresh.lock().await;
        state
            .items
            .retain(|it| !(it.info_hash == info_hash && it.port == port));
    }

    // -- Lookups -------------------------------------------------------------

    /// Look up reachability for `peer_id`.  Queries both `H_peer_direct`
    /// and `H_peer_relay` in parallel.  Returns whatever the DHT has, which
    /// may be partial — repeat after a few seconds if empty (bootstrap may
    /// still be ongoing).
    pub async fn lookup_peer(&self, peer_id: PeerId) -> PeerRoutes {
        let direct_fut = collect_peers(self.dht.get_peers(h_peer_direct(peer_id)));
        let relay_fut = collect_peers(self.dht.get_peers(h_peer_relay(peer_id)));
        let (direct, via_relay) = tokio::join!(direct_fut, relay_fut);
        PeerRoutes { direct, via_relay }
    }

    /// List addresses currently in the global "any relay" registry.  Useful
    /// for a fresh client to find a relay to attach to.  Returned in DHT
    /// response order — caller should ping / measure RTT to pick.
    pub async fn lookup_relays(&self) -> Vec<SocketAddr> {
        collect_peers(self.dht.get_peers(h_relays())).await
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.cancel.cancel();
        // The mainline actor thread shuts down when the last `Dht` clone is
        // dropped; happens implicitly as `dht` is dropped here.
    }
}

async fn refresh_loop(
    dht: AsyncDht,
    state: Arc<Mutex<RefreshState>>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
            _ = cancel.cancelled() => break,
        }
        let items: Vec<RefreshItem> = state.lock().await.items.clone();
        for item in items {
            if cancel.is_cancelled() {
                return;
            }
            if let Err(e) = dht.announce_peer(item.info_hash, Some(item.port)).await {
                warn!(?e, info_hash = ?item.info_hash, port = item.port, "dht re-announce failed");
            }
        }
    }
}

async fn collect_peers(mut stream: GetStream<Vec<SocketAddrV4>>) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        for addr in chunk {
            out.push(SocketAddr::V4(addr));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PrivateKey;

    #[test]
    fn info_hash_stable_for_same_peer_id() {
        let key = PrivateKey::generate();
        let pid = key.public_key().peer_id();
        assert_eq!(h_peer_direct(pid), h_peer_direct(pid));
        assert_eq!(h_peer_relay(pid), h_peer_relay(pid));
        assert_ne!(h_peer_direct(pid), h_peer_relay(pid));
    }

    #[test]
    fn info_hash_differs_for_different_peer_ids() {
        let p1 = PrivateKey::generate().public_key().peer_id();
        let p2 = PrivateKey::generate().public_key().peer_id();
        assert_ne!(h_peer_direct(p1), h_peer_direct(p2));
        assert_ne!(h_peer_relay(p1), h_peer_relay(p2));
    }

    #[test]
    fn relay_info_hash_constant() {
        assert_eq!(h_relays(), h_relays());
    }
}
