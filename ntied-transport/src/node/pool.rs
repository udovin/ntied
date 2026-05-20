//! Relay-connection pool with reconnect supervisor.
//!
//! Each `(SocketAddr → PoolEntry)` entry maintains a `current` live
//! [`RelayConnection`] (or `None` while reconnecting).  A supervisor task
//! per entry opens the underlying transport, watches its `closed` signal,
//! and either reconnects (`Attached` source) or exits (`Discovery` source).
//!
//! Sources can be promoted/demoted at runtime by `Node::attach_relay` /
//! `Node::detach_relay`.  The supervisor checks the source on each
//! reconnect iteration, so a demoted entry exits on its next disconnect.
//!
//! All small/read-only state (source, current slot, supervisor handle) uses
//! plain `std::sync::Mutex` — critical sections are guard-only reads or
//! single writes, never spanning an `.await`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::io;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::connection::{Connection, OwnedConnectionId};
use super::node::NodeCtx;
use super::relay::RelayConnection;

/// Initial reconnect backoff.  Doubles up to `RECONNECT_BACKOFF_MAX` on
/// repeated failure; resets on success.
const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelaySource {
    /// User-requested via `attach_relay`.  Supervisor reconnects forever.
    Attached,
    /// DHT-discovered or transient (ad-hoc `connect_relay_peer` on a fresh
    /// addr).  Supervisor exits on disconnect; top-up loop replaces.
    Discovery,
}

/// One pool entry per `relay_addr`.  Shared via `Arc` between the pool
/// HashMap, the supervisor task, and any `wait_for_connection` caller.
pub(crate) struct PoolEntry {
    pub(crate) addr: SocketAddr,
    /// Mutable so `attach_relay` / `detach_relay` can promote/demote.
    source: Mutex<RelaySource>,
    /// Wall-clock creation time — used by the discovery shed logic to keep
    /// freshly-discovered entries alive a grace period before considering
    /// them for removal.
    pub(crate) added_at: Instant,
    /// Cancelling drops `current`, exits the supervisor, and signals all
    /// `wait_for_connection` futures to fail with `Cancelled`.
    pub(crate) cancel: CancellationToken,
    /// Latest live relay connection; updated by supervisor.
    current: Mutex<Option<Arc<RelayConnection>>>,
    /// Notified after `current` transitions `None → Some`.
    connected: Notify,
    /// Single supervisor handle.  `None` between entry creation and first
    /// spawn, and after a `Discovery` supervisor has exited cleanly.  A
    /// stale (finished) handle indicates the supervisor is gone.
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl PoolEntry {
    pub(crate) fn new(addr: SocketAddr, source: RelaySource) -> Arc<Self> {
        Arc::new(Self {
            addr,
            source: Mutex::new(source),
            added_at: Instant::now(),
            cancel: CancellationToken::new(),
            current: Mutex::new(None),
            connected: Notify::new(),
            supervisor: Mutex::new(None),
        })
    }

    pub(crate) fn source(&self) -> RelaySource {
        *self.source.lock().unwrap()
    }

    pub(crate) fn set_source(&self, src: RelaySource) {
        *self.source.lock().unwrap() = src;
    }

    pub(crate) fn live_connection(&self) -> Option<Arc<RelayConnection>> {
        self.current.lock().unwrap().clone()
    }

    /// Number of live tunnels routed through the current relay connection.
    /// Returns 0 if disconnected.
    pub(crate) fn active_tunnels(&self) -> usize {
        match self.current.lock().unwrap().as_ref() {
            Some(c) => c.active_tunnels.load(Ordering::Relaxed),
            None => 0,
        }
    }

}

/// Spawn a supervisor for `entry` if none is currently running.  Idempotent.
pub(crate) fn ensure_supervisor(
    entry: &Arc<PoolEntry>,
    ctx: &NodeCtx,
    packet_buffer: usize,
) {
    let mut sup = entry.supervisor.lock().unwrap();
    if sup.as_ref().map_or(false, |h| !h.is_finished()) {
        return;
    }
    let entry_clone = entry.clone();
    let ctx_clone = ctx.clone();
    *sup = Some(tokio::spawn(relay_supervisor(
        entry_clone,
        ctx_clone,
        packet_buffer,
    )));
}

/// Open a raw `RelayConnection` to `addr` without touching the pool.
/// Used by the supervisor to (re)establish a connection.
async fn open_raw_relay(
    ctx: NodeCtx,
    addr: SocketAddr,
    packet_buffer: usize,
) -> io::Result<Arc<RelayConnection>> {
    let connection_id = ctx.next_connection_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel(packet_buffer);
    let owned = OwnedConnectionId::new(connection_id, &ctx.connection_map, tx);
    let conn = Connection::connect(
        owned,
        rx,
        ctx.socket.clone(),
        (*ctx.identity).clone(),
        ctx.cancel_token.child_token(),
        addr,
        ctx.config.clone(),
    )
    .await?;
    let tunnel = conn
        .open_channel()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("open_channel: {e:?}")))?;
    let control = conn
        .open_channel()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("open_channel: {e:?}")))?;
    Ok(RelayConnection::new(addr, conn, tunnel, control, ctx))
}

/// Long-running supervisor for one pool entry.
///
/// Loop:
///   1. Try to open a raw `RelayConnection`.
///   2. On success: stash in `entry.current`, notify waiters, await until
///      either the connection's `closed` token fires or the entry is
///      cancelled.
///   3. On disconnect: if source is `Attached` → reconnect immediately; if
///      `Discovery` → exit.
///   4. On open failure: if source is `Attached` → sleep `backoff` and
///      retry, doubling up to `RECONNECT_BACKOFF_MAX`; if `Discovery` →
///      exit so the top-up loop can replace.
async fn relay_supervisor(entry: Arc<PoolEntry>, ctx: NodeCtx, packet_buffer: usize) {
    let addr = entry.addr;
    let mut backoff = RECONNECT_BACKOFF_INITIAL;
    tracing::debug!(relay = %addr, source = ?entry.source(), "supervisor starting");
    loop {
        if entry.cancel.is_cancelled() {
            *entry.current.lock().unwrap() = None;
            tracing::debug!(relay = %addr, "supervisor cancelled");
            return;
        }
        match open_raw_relay(ctx.clone(), addr, packet_buffer).await {
            Ok(relay) => {
                tracing::info!(relay = %addr, source = ?entry.source(), "supervisor connected");
                backoff = RECONNECT_BACKOFF_INITIAL;
                let closed = relay.closed.clone();
                *entry.current.lock().unwrap() = Some(relay);
                entry.connected.notify_waiters();

                tokio::select! {
                    _ = closed.cancelled() => {
                        tracing::info!(relay = %addr, "supervisor: underlying connection closed");
                    }
                    _ = entry.cancel.cancelled() => {
                        *entry.current.lock().unwrap() = None;
                        tracing::debug!(relay = %addr, "supervisor cancelled while connected");
                        return;
                    }
                }
                *entry.current.lock().unwrap() = None;
                if matches!(entry.source(), RelaySource::Discovery) {
                    tracing::debug!(relay = %addr, "supervisor: Discovery disconnected, exiting");
                    return;
                }
                // Attached: reconnect immediately.
            }
            Err(e) => {
                tracing::warn!(relay = %addr, source = ?entry.source(), ?e, "supervisor open failed");
                if matches!(entry.source(), RelaySource::Discovery) {
                    tracing::debug!(relay = %addr, "supervisor: Discovery initial open failed, exiting");
                    return;
                }
                tracing::debug!(
                    relay = %addr,
                    backoff_ms = backoff.as_millis() as u64,
                    "supervisor backoff before retry",
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = entry.cancel.cancelled() => return,
                }
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
            }
        }
    }
}

/// Wait until `entry.current` is `Some`, or `timeout` elapses, or the entry
/// is cancelled.  Returns the live `Arc<RelayConnection>` on success.
pub(crate) async fn wait_for_connection(
    entry: &PoolEntry,
    timeout: Duration,
) -> io::Result<Arc<RelayConnection>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Subscribe BEFORE checking the slot — otherwise a notify between
        // check and await is missed and we sleep until timeout.
        let notified = entry.connected.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        if let Some(c) = entry.live_connection() {
            return Ok(c);
        }
        if entry.cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "pool entry cancelled",
            ));
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for relay connection",
            ));
        }
        let remaining = deadline - now;
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(remaining) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for relay connection",
                ));
            }
            _ = entry.cancel.cancelled() => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "pool entry cancelled",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn dummy_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1)
    }

    #[test]
    fn new_entry_state() {
        let e = PoolEntry::new(dummy_addr(), RelaySource::Attached);
        assert_eq!(e.source(), RelaySource::Attached);
        assert!(e.live_connection().is_none());
        assert_eq!(e.active_tunnels(), 0);
        assert!(!e.cancel.is_cancelled());
    }

    #[test]
    fn source_can_be_swapped() {
        let e = PoolEntry::new(dummy_addr(), RelaySource::Discovery);
        assert_eq!(e.source(), RelaySource::Discovery);
        e.set_source(RelaySource::Attached);
        assert_eq!(e.source(), RelaySource::Attached);
        e.set_source(RelaySource::Discovery);
        assert_eq!(e.source(), RelaySource::Discovery);
    }

    #[tokio::test]
    async fn wait_for_connection_times_out_when_disconnected() {
        let e = PoolEntry::new(dummy_addr(), RelaySource::Discovery);
        match wait_for_connection(&e, Duration::from_millis(20)).await {
            Ok(_) => panic!("expected timeout"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::TimedOut),
        }
    }

    #[tokio::test]
    async fn wait_for_connection_fails_on_cancel() {
        let e = PoolEntry::new(dummy_addr(), RelaySource::Discovery);
        let entry = e.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            entry.cancel.cancel();
        });
        match wait_for_connection(&e, Duration::from_secs(5)).await {
            Ok(_) => panic!("expected cancelled"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted),
        }
    }
}
