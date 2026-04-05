use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::{Mutex as TokioMutex, Notify};

use crate::connection::Connection as InnerConnection;
use crate::crypto::{KemPrivateKey, PeerId, PrivateKey, PublicKey};
use crate::wire::packet::Data;
use crate::wire::Frame;

use super::handle::DatagramChannel;

/// How to reach a peer: directly via UDP or through a relay.
#[derive(Debug, Clone)]
pub(crate) enum SendPath {
    Direct { addr: SocketAddr },
    Relayed { peer_id: PeerId },
}

/// State for an attached relay connection.
/// Holds the Connection handle to keep it alive (Drop will close it on relay change/shutdown).
pub(crate) struct RelayState {
    pub(crate) _connection: super::handle::Connection,
    pub(crate) datagram: DatagramChannel,
    pub(crate) relay_addr: SocketAddr,
}

pub(crate) struct Shared {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) identity: PrivateKey,
    pub(crate) shutdown: AtomicBool,
    pub(crate) state: TokioMutex<TransportState>,
    pub(crate) relay: TokioMutex<Option<RelayState>>,
    pub(crate) pending_close: std::sync::Mutex<Vec<u64>>,
    pub(crate) ping_counter: AtomicU32,
    pub(crate) accept_notify: Notify,
    pub(crate) established_notify: Notify,
    pub(crate) data_notify: Notify,
    pub(crate) stream_notify: Notify,
}

pub(crate) struct TransportState {
    pub(crate) connections: HashMap<u64, ConnEntry>,
    pub(crate) pending_connects: HashMap<u64, PendingConnect>,
    pub(crate) accept_queue: VecDeque<u64>,
    pub(crate) next_connection_id: u64,
}

pub(crate) struct ConnEntry {
    pub(crate) send_path: SendPath,
    pub(crate) relay_peer_id: Option<PeerId>,
    pub(crate) direct_addr: Option<SocketAddr>,
    pub(crate) last_direct_recv: Option<Instant>,
    pub(crate) conn: Box<InnerConnection>,
    pub(crate) last_recv: Instant,
    pub(crate) last_ping_sent: Instant,
    pub(crate) closed: bool,
    pub(crate) is_local_initiator: bool,
}

impl ConnEntry {
    /// Returns the direct address if known (either a direct connection or a
    /// relayed connection that has discovered a direct path).
    pub(crate) fn addr(&self) -> Option<SocketAddr> {
        match &self.send_path {
            SendPath::Direct { addr } => Some(*addr),
            SendPath::Relayed { .. } => self.direct_addr,
        }
    }

    pub(crate) fn is_established(&self) -> bool {
        self.conn.is_established()
    }

    pub(crate) fn got_connection_close(&self) -> bool {
        self.conn.got_connection_close()
    }

    pub(crate) fn queue_connection_close(&mut self, error_code: u32) {
        self.conn.queue_connection_close(error_code);
    }

    pub(crate) fn queue_ping(&mut self, ping_id: u32) {
        self.conn.queue_ping(ping_id);
    }

    pub(crate) fn queue_frame(&mut self, frame: Frame) {
        self.conn.queue_frame(frame);
    }

    pub(crate) fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        self.conn.poll_packets(now)
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.conn.has_pending()
    }

    pub(crate) fn peer_public_key(&self) -> Option<&PublicKey> {
        self.conn.peer_public_key()
    }

    pub(crate) fn local_connection_id(&self) -> u64 {
        self.conn.local_connection_id()
    }

    pub(crate) fn remote_connection_id(&self) -> u64 {
        self.conn.remote_connection_id()
    }

    pub(crate) fn has_pending_accept(&self) -> bool {
        self.conn.has_pending_accept()
    }
}

pub(crate) struct PendingConnect {
    pub(crate) ephemeral_key: Box<KemPrivateKey>,
    pub(crate) send_path: SendPath,
    pub(crate) relay_peer_id: Option<PeerId>,
}
