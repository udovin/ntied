use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use super::transport::Transport;

/// State of a single transport path to the peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum PathState {
    /// Newly added; waiting for first valid recv to confirm.
    Probing = 0,
    /// Confirmed and primary — receives all outbound traffic.
    Active = 1,
    /// Confirmed but not primary — held as backup, only used when Active fails.
    Idle = 2,
    /// No recv for too long; close to removal.
    Failing = 3,
}

impl PathState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Probing,
            1 => Self::Active,
            2 => Self::Idle,
            _ => Self::Failing,
        }
    }
}

pub(crate) struct Path {
    pub(crate) transport: Arc<Transport>,
    /// Identifier used to route inbound packets to this path:
    /// for `Udp` it's the peer's direct address, for `Tunnel` it's
    /// the relay's address (the same `addr` set by the pump on `RawPacket`).
    pub(crate) addr_key: SocketAddr,
    state: AtomicU8,
    last_recv: Mutex<Option<Instant>>,
}

impl Path {
    pub(crate) fn new(
        transport: Arc<Transport>,
        addr_key: SocketAddr,
        state: PathState,
    ) -> Arc<Self> {
        Arc::new(Self {
            transport,
            addr_key,
            state: AtomicU8::new(state as u8),
            last_recv: Mutex::new(None),
        })
    }

    pub(crate) fn state(&self) -> PathState {
        PathState::from_u8(self.state.load(Ordering::Acquire))
    }

    #[allow(dead_code)]
    pub(crate) fn set_state(&self, s: PathState) {
        self.state.store(s as u8, Ordering::Release);
    }

    #[allow(dead_code)]
    pub(crate) fn record_recv(&self, now: Instant) {
        *self.last_recv.lock().unwrap() = Some(now);
    }

    #[allow(dead_code)]
    pub(crate) fn last_recv(&self) -> Option<Instant> {
        *self.last_recv.lock().unwrap()
    }
}

pub(crate) type Paths = Arc<RwLock<Vec<Arc<Path>>>>;

/// Snapshot the paths that should receive outbound traffic.
///
/// Strategy: if any path is `Active`, send only to `Active` and `Probing`.
/// Otherwise (no confirmed primary) send to `Probing` and `Idle` so we
/// keep some path alive while validation is pending or while the primary
/// is recovering.
fn select_send_paths(paths: &[Arc<Path>]) -> Vec<Arc<Path>> {
    let any_active = paths.iter().any(|p| p.state() == PathState::Active);
    paths
        .iter()
        .filter(|p| {
            let s = p.state();
            if any_active {
                matches!(s, PathState::Active | PathState::Probing)
            } else {
                matches!(s, PathState::Probing | PathState::Idle)
            }
        })
        .cloned()
        .collect()
}

pub(crate) async fn send_via_paths(paths: &Paths, packet: &[u8]) {
    let targets = {
        let guard = paths.read().unwrap();
        select_send_paths(&guard)
    };
    if targets.is_empty() {
        tracing::warn!("packet dropped: no eligible paths for send");
        return;
    }
    for path in targets {
        if let Err(err) = path.transport.send_packet(packet).await {
            tracing::warn!(addr = %path.addr_key, ?err, "send_packet failed");
        }
    }
}

/// Time without recv after which an Active path is downgraded to Failing
/// and a previously-demoted Idle path is promoted back to Active.
pub(crate) const PATH_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Update path state on a successfully AEAD-validated inbound packet.
///
/// 1. Find the path matching `pkt_addr` and stamp its `last_recv`.
/// 2. If it was `Probing`, promote to `Active` and demote any other Active
///    path of the OTHER kind (Direct↔Tunnel) to `Idle` so we keep it as a
///    backup but prefer the new winner.
pub(crate) fn record_recv_and_promote(
    paths: &Paths,
    pkt_addr: std::net::SocketAddr,
    now: std::time::Instant,
) {
    let guard = paths.read().unwrap();
    let Some(matched_idx) = guard.iter().position(|p| p.addr_key == pkt_addr) else {
        return;
    };
    let matched = guard[matched_idx].clone();
    let was_probing = matched.state() == PathState::Probing;
    matched.record_recv(now);
    if !was_probing {
        return;
    }
    matched.set_state(PathState::Active);
    tracing::info!(addr = %matched.addr_key, "path promoted: probing -> active");
    let matched_kind = transport_kind(&matched.transport);
    for (i, p) in guard.iter().enumerate() {
        if i == matched_idx {
            continue;
        }
        if p.state() == PathState::Active && transport_kind(&p.transport) != matched_kind {
            p.set_state(PathState::Idle);
            tracing::debug!(addr = %p.addr_key, "path demoted: active -> idle");
        }
    }
}

/// On a periodic tick, downgrade Active Direct paths that have gone silent
/// past `PATH_IDLE_TIMEOUT`, and promote any Idle Tunnel back to Active so
/// outbound traffic resumes via relay.
pub(crate) fn check_state_timers(paths: &Paths, now: std::time::Instant) {
    let guard = paths.read().unwrap();
    for path in guard.iter() {
        if path.state() != PathState::Active {
            continue;
        }
        if !matches!(&*path.transport, super::transport::Transport::Udp { .. }) {
            continue;
        }
        let stale = match path.last_recv() {
            Some(last) => now.duration_since(last) > PATH_IDLE_TIMEOUT,
            None => false,
        };
        if !stale {
            continue;
        }
        path.set_state(PathState::Failing);
        tracing::warn!(addr = %path.addr_key, "path demoted: active -> failing (idle timeout)");
        for backup in guard.iter() {
            if matches!(
                &*backup.transport,
                super::transport::Transport::Tunnel { .. }
            ) && backup.state() == PathState::Idle
            {
                backup.set_state(PathState::Active);
                tracing::info!(addr = %backup.addr_key, "path promoted: idle -> active (failover)");
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum TransportKind {
    Udp,
    Tunnel,
}

fn transport_kind(t: &super::transport::Transport) -> TransportKind {
    match t {
        super::transport::Transport::Udp { .. } => TransportKind::Udp,
        super::transport::Transport::Tunnel { .. } => TransportKind::Tunnel,
    }
}
