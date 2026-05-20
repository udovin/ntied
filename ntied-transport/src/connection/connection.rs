use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::crypto::{
    AEAD_TAG_SIZE, EncryptionKey, EncryptionKeys, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE,
    KemCiphertext, KemPrivateKey, KemPublicKey, PUBLIC_KEY_SIZE, PrivateKey, PublicKey,
    SIGNATURE_SIZE, Signature, compute_transcript_hash,
};

use super::ack::{Ack, AckRange, AckReport, ControlFrame, LossReport, RecvAckState, RecvResult, SendAckState};
use crate::channel::manager::ChannelManager;
use crate::channel::message::{MessageAssembler, MessageFragmenter};
use crate::stream::manager::StreamManager;
use crate::wire::frame::{
    AUTH_HEADER_SIZE, CHANNEL_HEADER_SIZE, Frame, REKEY_ACK_HEADER_SIZE, REKEY_HEADER_SIZE,
    STREAM_HEADER_SIZE, decode_frames, encode_ack, encode_auth_complete, encode_auth_header,
    encode_channel_evict, encode_channel_fin, encode_channel_header, encode_channel_max_data,
    encode_channel_open, encode_connection_close, encode_max_channels,
    encode_ping, encode_pong, encode_rekey_ack_header, encode_rekey_header, encode_stream_header,
    encode_max_streams, encode_stream_max_data,
};
use crate::wire::packet::{
    DATA_HEADER_SIZE, DATA_TYPE_BASE, EPOCH_MASK, INIT_ACK_SIZE, INIT_SIZE, PacketHeader,
    encode_data_header, encode_init, encode_init_ack, parse_init_ack, peek_header,
};

const AUTH_PAYLOAD_SIZE: usize = PUBLIC_KEY_SIZE + SIGNATURE_SIZE;

/// Cursor that writes into a fixed `&mut [u8]` region without growing it.
/// Used for building packet plaintext directly into the network buffer,
/// avoiding intermediate `Vec` allocations.
struct PacketBuilder<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> PacketBuilder<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Bytes left until capacity.
    fn remaining(&self) -> usize {
        self.buf.len() - self.len
    }

    /// True if `n` more bytes can be written without overflow.
    fn fits(&self, n: usize) -> bool {
        self.len + n <= self.buf.len()
    }

    /// Reserve `n` bytes at the current position. Caller writes into the
    /// returned slice and then calls `commit(n)` (or fewer). Multiple
    /// `reserve` calls without `commit` return overlapping slices —
    /// typical pattern is reserve-write-commit.
    fn reserve(&mut self, n: usize) -> &mut [u8] {
        &mut self.buf[self.len..self.len + n]
    }

    fn commit(&mut self, n: usize) {
        self.len += n;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Nothing to send or read.
    Done,
    /// Operation not valid in current connection state.
    InvalidState,
    /// Received a malformed or unexpected packet.
    InvalidPacket,
    /// Buffer too small for the operation.
    BufferTooShort,
    /// KEM decapsulation or AEAD authentication failed.
    CryptoError,
    /// Auth signature verification failed.
    AuthFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ConnectionId(pub u64);

#[derive(Debug, Clone, Copy)]
pub struct RecvInfo {
    pub now: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SendInfo {
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Initiator: need to send Init packet.
    Init,
    /// Initiator: Init sent, waiting for InitAck.
    InitSent,
    /// Responder: received Init, need to send InitAck.
    SendInitAck,
    /// Both sides exchanging Auth frames (signed identity).
    Authenticating,
    /// Handshake + auth complete, full data path.
    Established,
    /// ConnectionClose sent, draining.
    Closing,
    /// Terminal state.
    Closed,
}

pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_REKEY_INTERVAL: Duration = Duration::from_secs(3600);

/// Configuration for a connection.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum streams per direction (local and peer independently). Default: 256.
    pub max_streams: usize,
    /// Maximum channels per direction. Default: 256.
    pub max_channels: usize,
    /// Per-stream buffer size in bytes. Default: 65536.
    pub stream_buf_size: usize,
    /// Per-channel buffer size in bytes. Default: 65536.
    pub channel_buf_size: usize,
    /// Maximum concurrently in-flight messages per channel.  Sender may
    /// allocate at most this many message ids ahead of receiver's
    /// terminations.  Default: 1024.
    pub channel_max_messages: usize,
    /// Keepalive ping interval. `None` disables keepalive. Default: `Some(5s)`.
    pub keepalive: Option<Duration>,
    /// Connection is closed if no packets received within this duration. Default: 30s.
    pub idle_timeout: Duration,
    /// Connection is closed if handshake not completed within this duration. Default: 10s.
    pub handshake_timeout: Duration,
    /// How often the connection initiates a rekey to rotate session keys.
    /// `None` disables periodic rekey (the state machine still responds to
    /// peer-initiated rekeys). Default: `Some(1h)`.
    pub rekey_interval: Option<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_streams: 256,
            max_channels: 256,
            stream_buf_size: 65536,
            channel_buf_size: 65536,
            channel_max_messages: 1024,
            keepalive: Some(Duration::from_secs(5)),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            rekey_interval: Some(DEFAULT_REKEY_INTERVAL),
        }
    }
}

pub struct Connection {
    state: State,
    is_initiator: bool,
    connection_id: ConnectionId,
    peer_connection_id: Option<ConnectionId>,
    config: Config,

    // Identity key for signing.
    identity: PrivateKey,

    // KEM handshake state (consumed during handshake).
    kem_private: Option<KemPrivateKey>,
    kem_peer_pk: Option<KemPublicKey>,

    // Encryption keys indexed by epoch (2-bit, 0..3).
    pub(super) send_epoch: u8,
    pub(super) send_keys: [Option<EncryptionKey>; 4],
    pub(super) recv_keys: [Option<EncryptionKey>; 4],

    // Auth state.
    transcript_hash: Option<[u8; 32]>,
    auth_send: Option<MessageFragmenter>,
    auth_recv: Option<MessageAssembler>,
    peer_authenticated: bool,
    auth_complete_sent: bool,
    auth_complete_received: bool,
    peer_public_key: Option<PublicKey>,

    // Rekey state.
    pub(super) rekey_kem: Option<KemPrivateKey>,
    pub(super) rekey_send: Option<MessageFragmenter>,
    pub(super) rekey_recv: Option<MessageAssembler>,
    /// recv_ack floor at the time of the last epoch transition.
    /// When floor advances past this, we clean N-1 keys.
    prev_epoch: Option<u8>,
    prev_epoch_floor: u64,

    streams: StreamManager,
    channels: ChannelManager,
    pub(super) send_ack: SendAckState,
    recv_ack: RecvAckState,

    packet_counter: u64,

    // Ping/pong for latency measurement.
    next_ping_id: u32,
    /// Pings queued but not yet sent.
    pings_to_send: Vec<(u32, Instant)>,
    /// In-flight pings awaiting pong: id → sent_at.
    pings_in_flight: HashMap<u32, Instant>,
    ping_rtt: Option<Duration>,
    /// Next keepalive ping time. Updated after each ping sent.
    next_ping_at: Option<Instant>,
    /// Next time to initiate a rekey. `None` if rekey is disabled or the
    /// connection has not reached Established yet. Updated whenever we
    /// initiate a rekey from the timer.
    next_rekey_at: Option<Instant>,

    // Timers.
    created_at: Instant,
    last_recv_at: Option<Instant>,
    last_send_at: Option<Instant>,
    loss_detection_pending: bool,

    // ACK-of-ACK: maps our packet counter → recv floor at time of sending.
    // When peer ACKs our packet, we advance recv_ack floor.
    ack_floor_by_counter: HashMap<u64, u64>,

    // Pending control frames for the next outgoing packet.
    pending_pongs: Vec<u32>,
    pending_stream_max_data: HashMap<u64, u64>,
    pending_close: Option<(u32, Vec<u8>)>,
    pending_auth_complete: bool,

    // Reusable scratch buffers for emit loop — avoid per-emit allocations.
    scratch_stream_max_data: Vec<(u64, u64)>,
    scratch_pending_opens: Vec<u64>,
    scratch_pending_fins: Vec<(u64, u64)>,
    scratch_channel_max_data: Vec<(u64, u64, u64)>,
    scratch_pending_evicts: Vec<(u64, u64, u64)>,
    // Reusable per-received-ack ranges decoded from frame bytes.
    scratch_ack_ranges: Vec<AckRange>,
    // Reusable buffers for AckReport / LossReport on every received ack.
    scratch_acked: AckReport,
    scratch_loss: LossReport,
}

impl Connection {
    /// Create an initiator-side connection with default config.
    pub fn open(local_id: ConnectionId, identity: PrivateKey) -> Self {
        Self::open_with_config(local_id, identity, Config::default())
    }

    /// Create an initiator-side connection with custom config.
    pub fn open_with_config(local_id: ConnectionId, identity: PrivateKey, config: Config) -> Self {
        let kem = KemPrivateKey::generate();
        let mut conn = Self::new(local_id, None, true, State::Init, identity, config);
        conn.kem_private = Some(kem);
        tracing::debug!(cid = local_id.0, role = "initiator", "connection opened");
        conn
    }

    /// Create a responder-side connection with default config.
    pub fn accept(
        local_id: ConnectionId,
        peer_id: ConnectionId,
        peer_kem_pk: KemPublicKey,
        identity: PrivateKey,
    ) -> Self {
        Self::accept_with_config(local_id, peer_id, peer_kem_pk, identity, Config::default())
    }

    /// Create a responder-side connection with custom config.
    pub fn accept_with_config(
        local_id: ConnectionId,
        peer_id: ConnectionId,
        peer_kem_pk: KemPublicKey,
        identity: PrivateKey,
        config: Config,
    ) -> Self {
        let mut conn = Self::new(local_id, Some(peer_id), false, State::SendInitAck, identity, config);
        conn.kem_peer_pk = Some(peer_kem_pk);
        tracing::debug!(
            cid = local_id.0,
            peer_cid = peer_id.0,
            role = "responder",
            "connection accepted",
        );
        conn
    }

    fn new(
        local_id: ConnectionId,
        peer_id: Option<ConnectionId>,
        is_initiator: bool,
        state: State,
        identity: PrivateKey,
        config: Config,
    ) -> Self {
        let streams = StreamManager::new(config.stream_buf_size, is_initiator, config.max_streams);
        let channels = ChannelManager::new(
            config.channel_buf_size as u64,
            config.channel_max_messages as u64,
            config.channel_buf_size as u64,
            config.channel_max_messages as u64,
            is_initiator,
            config.max_channels,
        );
        Self {
            state,
            is_initiator,
            connection_id: local_id,
            peer_connection_id: peer_id,
            config,
            identity,
            kem_private: None,
            kem_peer_pk: None,
            send_epoch: 0,
            send_keys: [None, None, None, None],
            recv_keys: [None, None, None, None],
            transcript_hash: None,
            auth_send: None,
            auth_recv: None,
            peer_authenticated: false,
            auth_complete_sent: false,
            auth_complete_received: false,
            peer_public_key: None,
            rekey_kem: None,
            rekey_send: None,
            rekey_recv: None,
            prev_epoch: None,
            prev_epoch_floor: 0,
            streams,
            channels,
            send_ack: SendAckState::new(),
            recv_ack: RecvAckState::new(),
            packet_counter: 0,
            next_ping_id: 0,
            pings_to_send: Vec::new(),
            pings_in_flight: HashMap::new(),
            ping_rtt: None,
            next_ping_at: None,
            next_rekey_at: None,
            created_at: Instant::now(),
            last_recv_at: None,
            last_send_at: None,
            loss_detection_pending: false,
            ack_floor_by_counter: HashMap::new(),
            pending_pongs: Vec::new(),
            pending_stream_max_data: HashMap::new(),
            pending_close: None,
            pending_auth_complete: false,
            scratch_stream_max_data: Vec::new(),
            scratch_pending_opens: Vec::new(),
            scratch_pending_fins: Vec::new(),
            scratch_channel_max_data: Vec::new(),
            scratch_pending_evicts: Vec::new(),
            scratch_ack_ranges: Vec::new(),
            scratch_acked: AckReport {
                streams: Vec::new(),
                channels: Vec::new(),
                frames: Vec::new(),
            },
            scratch_loss: LossReport {
                streams: Vec::new(),
                channels: Vec::new(),
                frames: Vec::new(),
                auth: Vec::new(),
                rekey: Vec::new(),
            },
        }
    }

    // -- Packet I/O ----------------------------------------------------------

    /// Process a received packet.
    /// Process a received packet.  `buf` is required to be mutable so that
    /// data packets can be decrypted in place (no Vec allocation).
    pub fn recv(&mut self, buf: &mut [u8], info: RecvInfo) -> Result<usize, Error> {
        let header = peek_header(buf).map_err(|_| Error::InvalidPacket)?;

        match header {
            PacketHeader::InitAck { .. } => self.recv_init_ack(buf, info.now),

            PacketHeader::Init { .. } => Err(Error::InvalidPacket),

            PacketHeader::Data { .. } => match self.state {
                State::Authenticating | State::Established | State::Closing => {
                    self.recv_data(buf, info.now)
                }
                _ => Err(Error::InvalidState),
            },
        }
    }

    /// Build and send the next outgoing packet.
    pub fn send(&mut self, buf: &mut [u8], now: Instant) -> Result<(usize, SendInfo), Error> {
        match self.state {
            State::Init => self.send_init(buf, now),
            State::SendInitAck => self.send_init_ack(buf, now),
            State::Authenticating | State::Established | State::Closing => self.send_data(buf, now),
            State::InitSent | State::Closed => Err(Error::Done),
        }
    }

    // -- Stream API ----------------------------------------------------------

    /// Drain stream IDs whose state changed since last call.
    /// Drain stream IDs whose state changed since last call into `out`.
    /// Caller's buffer is appended to.
    pub fn drain_updated_streams(&mut self, out: &mut Vec<u64>) {
        self.streams.drain_updated(out);
    }

    /// True if the stream exists and our send side is still open.
    /// False for unknown ids or streams we've already FIN'd. Used by
    /// the node-level accept loop to filter out stale ids surfaced by
    /// `drain_updated_streams` after `Stream::drop` — our own closed
    /// streams must not be handed out as fresh peer-initiated streams.
    pub fn is_stream_writable(&self, stream_id: u64) -> bool {
        self.streams.is_writable(stream_id)
    }

    pub fn readable_streams(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams.readable()
    }

    pub fn writable_streams(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams.writable()
    }

    /// Read from a stream's local recv buffer.
    ///
    /// Allowed in any connection state: even after the peer has sent
    /// `ConnectionClose`, app may drain data that was already received and
    /// buffered.  Unknown streams return `Done` (they were either never opened
    /// or auto-cleaned up).
    pub fn stream_read(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<(usize, bool), Error> {
        let result = self.streams.read(stream_id, buf).map_err(|_| Error::Done);
        if let Ok((n, fin)) = &result {
            if *n > 0 || *fin {
                tracing::trace!(cid = self.connection_id.0, sid = stream_id, n, fin, "stream_read");
            }
        }
        result
    }

    pub fn stream_write(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        let result = self.streams
            .write(stream_id, data, fin)
            .map_err(|_| Error::Done);
        if let Ok(n) = &result {
            tracing::trace!(
                cid = self.connection_id.0,
                sid = stream_id,
                written = n,
                fin,
                free = self.streams.writable().count(),
                "stream_write",
            );
        }
        result
    }

    // -- Channel API ---------------------------------------------------------

    /// Create a channel and queue a ChannelOpen frame.
    /// The channel is lazily created in connection and the peer
    /// is notified via ChannelOpen. Returns the channel_id.
    pub fn open_channel(&mut self, channel_id: u64) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        let result = self
            .channels
            .on_local_open(channel_id)
            .map_err(|_| Error::Done);
        if result.is_ok() {
            tracing::debug!(
                cid = self.connection_id.0,
                chid = channel_id,
                side = "local",
                "channel opened",
            );
        }
        result
    }

    /// Drain channel IDs whose state changed since last call into `out`.
    /// Caller's buffer is appended to (existing contents preserved).
    pub fn drain_updated_channels(&mut self, out: &mut Vec<u64>) {
        self.channels.drain_updated(out);
    }

    /// True if the channel exists and our send side is still open.
    /// False for unknown ids or channels we've already FIN'd. Used by
    /// the node-level accept loop to filter out stale ids surfaced by
    /// `drain_updated_channels` after `Channel::drop` — our own closed
    /// channels must not be handed out as fresh peer-initiated channels.
    pub fn is_channel_writable(&self, channel_id: u64) -> bool {
        self.channels.is_writable(channel_id)
    }

    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels.readable_channels()
    }

    pub fn channel_send(
        &mut self,
        channel_id: u64,
        data: Vec<u8>,
        reliable: bool,
    ) -> Result<u64, Error> {
        if self.state != State::Established {
            tracing::warn!(
                cid = self.connection_id.0,
                chid = channel_id,
                state = ?self.state,
                "channel_send rejected: not established",
            );
            return Err(Error::InvalidState);
        }
        self.channels.send(channel_id, data, reliable).map_err(|err| {
            tracing::warn!(
                cid = self.connection_id.0,
                chid = channel_id,
                ?err,
                "channel_send rejected by manager",
            );
            Error::Done
        })
    }

    pub fn channel_recv(&mut self, channel_id: u64) -> Result<Vec<u8>, Error> {
        self.channels.poll(channel_id).ok_or(Error::Done)
    }

    /// Half-close: signal that we will not send more messages on this channel.
    /// In-flight sends drain; the channel is removed automatically once both
    /// sides have signalled fin and drained.
    pub fn channel_close(&mut self, channel_id: u64) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.channels.close_send(channel_id);
        tracing::debug!(
            cid = self.connection_id.0,
            chid = channel_id,
            side = "local",
            "channel close_send",
        );
        Ok(())
    }

    /// Release any Ready (delivered to transport, not yet polled by app)
    /// messages on the channel.  Used when the application drops its
    /// handle without polling: keeps the receive-side flow control honest
    /// so the channel can eventually be cleaned up.
    pub fn channel_drain_recv(&mut self, channel_id: u64) {
        self.channels.drain_delivery_queue(channel_id);
    }

    /// Raise the per-direction max-streams budget for this connection at
    /// runtime.  Increase only -- monotonic on the wire.  A `MaxStreams`
    /// frame is queued so the peer learns of the new cap on next send.
    pub fn set_max_streams(&mut self, new_max: usize) {
        self.streams.set_max_streams(new_max);
    }

    /// Raise the per-direction max-channels budget for this connection at
    /// runtime.  Increase only -- monotonic on the wire.  A `MaxChannels`
    /// frame is queued so the peer learns of the new cap on next send.
    pub fn set_max_channels(&mut self, new_max: usize) {
        self.channels.set_max_channels(new_max);
    }

    /// Resize a channel's local send-buffer cap.  Effective for future
    /// `channel_send` calls.  No-op for unknown channel.
    pub fn set_channel_send_buf_cap(&mut self, channel_id: u64, cap: u64) {
        self.channels.set_send_buf_cap(channel_id, cap);
    }

    /// Resize a channel's local send-side message-count cap.  Symmetric to
    /// `set_channel_send_buf_cap` but in messages instead of bytes.  No-op
    /// for unknown channel.
    pub fn set_channel_send_msg_cap(&mut self, channel_id: u64, cap: u64) {
        self.channels.set_send_msg_cap(channel_id, cap);
    }

    /// Resize a channel's receive-buffer cap.  Grow takes effect via the
    /// next `ChannelMaxData` update; shrink does not revoke already-granted
    /// credit (wire monotonicity).  No-op for unknown channel.
    pub fn set_channel_recv_buf_cap(&mut self, channel_id: u64, cap: u64) {
        self.channels.set_recv_buf_cap(channel_id, cap);
    }

    /// Resize a channel's in-flight message-count cap (receiver side).  Grow
    /// advertises more credit via the next `ChannelMaxData`; shrink does not
    /// revoke already-granted credit (wire monotonicity).  No-op for unknown
    /// channel.
    pub fn set_channel_recv_msg_cap(&mut self, channel_id: u64, cap: u64) {
        self.channels.set_recv_msg_cap(channel_id, cap);
    }

    /// Resize a stream's local send-buffer capacity.  Shrinking below the
    /// currently buffered data is allowed: new writes return 0 until acks
    /// drain enough to fit under the new limit.  No-op for unknown stream.
    pub fn set_stream_send_buf_cap(&mut self, stream_id: u64, cap: usize) {
        self.streams.set_send_buf_cap(stream_id, cap);
    }

    /// Resize a stream's receive window cap.  Grow advertises more credit
    /// via the next `StreamMaxData`; shrink does not revoke already-granted
    /// credit (wire monotonicity).  No-op for unknown stream.
    pub fn set_stream_recv_buf_cap(&mut self, stream_id: u64, cap: usize) {
        self.streams.set_recv_buf_cap(stream_id, cap);
    }

    // -- Connection lifecycle ------------------------------------------------

    /// Initiate a graceful close.
    pub fn close(&mut self, error_code: u32, reason: &[u8]) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.pending_close = Some((error_code, reason.to_vec()));
        self.state = State::Closing;
        tracing::info!(
            cid = self.connection_id.0,
            code = error_code,
            kind = "graceful",
            "close requested",
        );
        Ok(())
    }

    /// Immediately close due to a protocol error.
    /// Skips data drain: ConnectionClose is sent as soon as possible.
    fn close_with_error(&mut self, error_code: u32, reason: &[u8]) {
        if self.state == State::Closed || self.state == State::Closing {
            return;
        }
        self.pending_close = Some((error_code, reason.to_vec()));
        self.state = State::Closing;
        tracing::warn!(
            cid = self.connection_id.0,
            code = error_code,
            kind = "error",
            "close on protocol error",
        );
    }

    pub fn is_established(&self) -> bool {
        self.state == State::Established
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    pub fn connection_id(&self) -> ConnectionId {
        self.connection_id
    }

    pub fn peer_connection_id(&self) -> Option<ConnectionId> {
        self.peer_connection_id
    }

    /// Peer's verified public key (available after auth completes).
    pub fn peer_public_key(&self) -> Option<&PublicKey> {
        self.peer_public_key.as_ref()
    }

    /// Last measured ping round-trip time.
    pub fn ping_rtt(&self) -> Option<Duration> {
        self.ping_rtt
    }

    /// Queue a ping for latency measurement. The pong will update `ping_rtt()`.
    /// Also called automatically by keepalive if configured.
    /// Limits queued and in-flight pings to prevent memory exhaustion.
    pub fn ping(&mut self, now: Instant) {
        // Don't queue if too many pings already pending or in-flight.
        if self.pings_to_send.len() >= 8 || self.pings_in_flight.len() >= 8 {
            return;
        }
        let id = self.next_ping_id;
        self.next_ping_id = self.next_ping_id.wrapping_add(1);
        self.pings_to_send.push((id, now));
    }

    /// Schedule the next keepalive ping based on config.
    fn schedule_next_ping(&mut self, now: Instant) {
        self.next_ping_at = self.config.keepalive.map(|interval| now + interval);
    }

    // -- Timers --------------------------------------------------------------

    /// Duration until the next timer fires.
    ///
    /// The caller should wait this long (or until a packet arrives), then
    /// call `on_timeout()`.  Returns `None` if the connection is closed.
    pub fn timeout(&self) -> Option<Duration> {
        if self.state == State::Closed {
            return None;
        }

        let now = Instant::now();
        let mut earliest: Option<Instant> = None;

        // Handshake timeout (Init, InitSent, SendInitAck, Authenticating).
        if self.state != State::Established && self.state != State::Closing {
            let deadline = self.created_at + self.config.handshake_timeout;
            earliest = Some(deadline);
        }

        // Idle timeout (Established/Closing).
        // last_recv_at is always set after handshake completes.
        if self.state == State::Established || self.state == State::Closing {
            let last = self.last_recv_at.expect("last_recv_at must be set in Established/Closing");
            let deadline = last + self.config.idle_timeout;
            earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
        }

        // Loss detection timeout.
        // If packets are in flight, we must have sent them → last_send_at is set.
        if self.send_ack.in_flight_count() > 0 {
            let last_send = self.last_send_at.expect("last_send_at must be set when in_flight > 0");
            let deadline = last_send + self.send_ack.loss_timeout();
            earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
        }

        // Keepalive ping timeout.
        if let Some(ping_at) = self.next_ping_at {
            earliest = Some(earliest.map_or(ping_at, |e| e.min(ping_at)));
        }

        // Periodic rekey.
        if let Some(rekey_at) = self.next_rekey_at {
            earliest = Some(earliest.map_or(rekey_at, |e| e.min(rekey_at)));
        }

        earliest.map(|t| t.saturating_duration_since(now))
    }

    /// Handle a timeout expiration.
    ///
    /// Called by the event loop when the duration from `timeout()` has elapsed.
    /// May mark packets as lost (triggering retransmission on next `send()`),
    /// or close the connection on handshake/idle timeout.
    pub fn on_timeout(&mut self, now: Instant) {
        if self.state == State::Closed {
            return;
        }

        // Handshake timeout.
        if self.state != State::Established && self.state != State::Closing {
            if now.duration_since(self.created_at) >= self.config.handshake_timeout {
                self.state = State::Closed;
                tracing::warn!(
                    cid = self.connection_id.0,
                    elapsed_ms = now.duration_since(self.created_at).as_millis() as u64,
                    "closed: handshake timeout",
                );
                return;
            }
        }

        // Idle timeout.
        if self.state == State::Established || self.state == State::Closing {
            let last = self.last_recv_at.expect("last_recv_at must be set in Established/Closing");
            if now.duration_since(last) >= self.config.idle_timeout {
                self.state = State::Closed;
                tracing::warn!(
                    cid = self.connection_id.0,
                    idle_ms = now.duration_since(last).as_millis() as u64,
                    "closed: idle timeout",
                );
                return;
            }
        }

        // Loss detection: detect_losses is handled inside on_ack_received already,
        // but timeout-based loss (no ACK at all) needs explicit triggering.
        // We flag it so the next send() can probe.
        if self.send_ack.in_flight_count() > 0 {
            let last_send = self.last_send_at.expect("last_send_at must be set when in_flight > 0");
            if now.duration_since(last_send) >= self.send_ack.loss_timeout() {
                self.loss_detection_pending = true;
            }
        }

        // Clean up stale pings that never received a pong.
        let idle = self.config.idle_timeout;
        self.pings_in_flight
            .retain(|_, sent_at| now.duration_since(*sent_at) < idle);

        // Keepalive ping.
        if let Some(ping_at) = self.next_ping_at {
            if now >= ping_at && self.state == State::Established {
                self.ping(now);
                self.schedule_next_ping(now);
            }
        }

        // Periodic rekey. Always reschedule once the deadline fires —
        // even if a rekey is already in progress (start_rekey will return
        // InvalidState in that case), so we don't busy-loop on the timer.
        if let Some(rekey_at) = self.next_rekey_at {
            if now >= rekey_at && self.state == State::Established {
                let _ = self.start_rekey();
                self.schedule_next_rekey(now);
            }
        }
    }

    // -- Internals: Handshake ------------------------------------------------

    fn send_init(&mut self, buf: &mut [u8], now: Instant) -> Result<(usize, SendInfo), Error> {
        if buf.len() < INIT_SIZE {
            return Err(Error::BufferTooShort);
        }
        let kem_pk = self.kem_private.as_ref().unwrap().public_key();
        let n = encode_init(buf, self.connection_id.0, &kem_pk);
        self.kem_peer_pk = Some(kem_pk);
        self.state = State::InitSent;
        tracing::debug!(cid = self.connection_id.0, "init sent");
        Ok((n, SendInfo { at: now }))
    }

    fn recv_init_ack(&mut self, buf: &[u8], _now: Instant) -> Result<usize, Error> {
        if self.state != State::InitSent {
            return Err(Error::InvalidState);
        }
        let pkt = parse_init_ack(buf).map_err(|_| Error::InvalidPacket)?;
        if pkt.initiator_connection_id != self.connection_id.0 {
            return Err(Error::InvalidPacket);
        }

        // KEM decapsulate.
        let kem_sk = self.kem_private.take().unwrap();
        let shared_secret = kem_sk
            .decapsulate(&pkt.kem_ciphertext)
            .ok_or(Error::CryptoError)?;

        // Derive encryption keys. Initiator: send=i2r, recv=r2i, epoch 0.
        let init_pk = self.kem_peer_pk.as_ref().unwrap();
        let transcript_hash = compute_transcript_hash(init_pk, &pkt.kem_ciphertext);
        let keys = EncryptionKeys::new(&shared_secret, init_pk, &pkt.kem_ciphertext);
        let (i2r, r2i) = keys.into_keys();
        self.send_keys[0] = Some(i2r);
        self.recv_keys[0] = Some(r2i);
        self.send_epoch = 0;

        self.kem_peer_pk = None;
        self.peer_connection_id = Some(ConnectionId(pkt.responder_connection_id));

        // Prepare auth: sign transcript and start fragmenter.
        self.begin_auth(transcript_hash);
        self.state = State::Authenticating;
        tracing::debug!(
            cid = self.connection_id.0,
            peer_cid = pkt.responder_connection_id,
            "init_ack received, authenticating",
        );
        Ok(buf.len())
    }

    fn send_init_ack(&mut self, buf: &mut [u8], now: Instant) -> Result<(usize, SendInfo), Error> {
        if buf.len() < INIT_ACK_SIZE {
            return Err(Error::BufferTooShort);
        }
        let peer_id = self.peer_connection_id.unwrap();
        let peer_pk = self.kem_peer_pk.take().unwrap();

        // KEM encapsulate.
        let resp_kem = KemPrivateKey::generate();
        let (ct, shared_secret) = resp_kem.encapsulate(&peer_pk).ok_or(Error::CryptoError)?;

        let n = encode_init_ack(buf, self.connection_id.0, peer_id.0, &ct);

        // Derive encryption keys. Responder: send=r2i, recv=i2r, epoch 0.
        let transcript_hash = compute_transcript_hash(&peer_pk, &ct);
        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ct);
        let (i2r, r2i) = keys.into_keys();
        self.send_keys[0] = Some(r2i);
        self.recv_keys[0] = Some(i2r);
        self.send_epoch = 0;

        // Prepare auth.
        self.begin_auth(transcript_hash);
        self.state = State::Authenticating;
        tracing::debug!(
            cid = self.connection_id.0,
            peer_cid = peer_id.0,
            "init_ack sent, authenticating",
        );
        Ok((n, SendInfo { at: now }))
    }

    /// Build auth payload (public_key || signature) and prepare fragmenter + assembler.
    fn begin_auth(&mut self, transcript_hash: [u8; 32]) {
        let pk_bytes = self.identity.public_key().to_bytes();
        let sig = self.identity.sign(&transcript_hash);
        let sig_bytes = sig.to_bytes();

        let mut payload = Vec::with_capacity(AUTH_PAYLOAD_SIZE);
        payload.extend_from_slice(&pk_bytes);
        payload.extend_from_slice(&sig_bytes);

        self.auth_send = Some(MessageFragmenter::new(payload));
        self.auth_recv = Some(MessageAssembler::new());
        self.transcript_hash = Some(transcript_hash);
    }

    /// Check if auth is complete on both sides and transition to Established.
    fn try_finish_auth(&mut self, now: Instant) {
        if self.peer_authenticated && self.auth_complete_sent && self.auth_complete_received {
            self.state = State::Established;
            // Clean up auth state.
            self.auth_send = None;
            self.auth_recv = None;
            self.transcript_hash = None;
            // Arm keepalive and periodic rekey timers.
            self.schedule_next_ping(now);
            self.schedule_next_rekey(now);
            let peer_short = self
                .peer_public_key
                .as_ref()
                .map(|pk| pk.peer_id().short())
                .unwrap_or_default();
            tracing::info!(
                cid = self.connection_id.0,
                peer = %peer_short,
                "handshake done, established",
            );
        }
    }

    fn schedule_next_rekey(&mut self, now: Instant) {
        self.next_rekey_at = self.config.rekey_interval.map(|interval| now + interval);
    }

    /// Verify peer's auth payload.
    fn verify_auth_payload(&mut self) -> Result<(), Error> {
        let assembler = self.auth_recv.take().unwrap();
        let payload = assembler.take();

        if payload.len() != AUTH_PAYLOAD_SIZE {
            return Err(Error::AuthFailed);
        }

        let pk_bytes: &[u8; PUBLIC_KEY_SIZE] = payload[..PUBLIC_KEY_SIZE]
            .try_into()
            .map_err(|_| Error::AuthFailed)?;
        let sig_bytes: &[u8; SIGNATURE_SIZE] = payload[PUBLIC_KEY_SIZE..]
            .try_into()
            .map_err(|_| Error::AuthFailed)?;

        let peer_pk = PublicKey::from_bytes(pk_bytes).ok_or(Error::AuthFailed)?;
        let signature = Signature::from_bytes(sig_bytes).ok_or(Error::AuthFailed)?;

        let transcript_hash = self.transcript_hash.as_ref().unwrap();
        if !peer_pk.verify(transcript_hash, &signature) {
            return Err(Error::AuthFailed);
        }

        self.peer_public_key = Some(peer_pk);
        self.peer_authenticated = true;
        self.pending_auth_complete = true;
        Ok(())
    }

    // -- Internals: Data path ------------------------------------------------

    fn send_data(&mut self, buf: &mut [u8], now: Instant) -> Result<(usize, SendInfo), Error> {
        let peer_id = self.peer_connection_id.ok_or(Error::InvalidState)?;
        // send_keys are always set before entering Authenticating/Established/Closing.
        debug_assert!(self.send_keys[self.send_epoch as usize].is_some());

        // Timeout-based loss detection: if on_timeout flagged losses,
        // run detection now so retransmits are queued before we build the packet.
        if self.loss_detection_pending {
            self.loss_detection_pending = false;
            let mut loss = std::mem::take(&mut self.scratch_loss);
            self.send_ack.detect_timeout_losses_into(now, &mut loss);
            self.handle_loss_ref(&mut loss);
            self.scratch_loss = loss;
        }

        if buf.len() < DATA_HEADER_SIZE + 12 + AEAD_TAG_SIZE {
            return Err(Error::BufferTooShort);
        }

        let mut sent_streams: Vec<(u64, u64, usize)> = Vec::new();
        let mut sent_channels: Vec<(u64, u64, u64, usize)> = Vec::new();
        let mut sent_frames: Vec<ControlFrame> = Vec::new();
        let mut sent_auth: Vec<(u64, usize)> = Vec::new();
        let mut sent_rekey: Vec<(u64, usize)> = Vec::new();
        let mut pending_ack_floor: Option<u64> = None;
        let mut ack_len: usize = 0;

        // Build plaintext directly into buf[DATA_HEADER_SIZE .. buf.len() - AEAD_TAG_SIZE].
        // No intermediate Vec allocation; encrypt in place after.
        let plaintext_len = {
            let payload_end = buf.len() - AEAD_TAG_SIZE;
            let region = &mut buf[DATA_HEADER_SIZE..payload_end];
            let mut b = PacketBuilder::new(region);

            // 1. ACK frame.
            if let Some((ack, ack_floor)) = self.recv_ack.generate_ack(now) {
                pending_ack_floor = Some(ack_floor);
                let ack_ranges: Vec<(u64, u64)> =
                    ack.ranges.iter().map(|r| (r.gap, r.length)).collect();
                let max_ack = 12 + ack_ranges.len() * 16;
                if b.fits(max_ack) {
                    let dst = b.reserve(max_ack);
                    let n = encode_ack(dst, ack.largest_ack, ack.ack_delay, &ack_ranges);
                    b.commit(n);
                    ack_len = n;
                }
            }

            // 2. Pending pings.
            let pings: Vec<_> = self.pings_to_send.drain(..).collect();
            for (id, sent_at) in pings {
                if !b.fits(5) {
                    break;
                }
                let dst = b.reserve(5);
                encode_ping(dst, id);
                b.commit(5);
                self.pings_in_flight.insert(id, sent_at);
                sent_frames.push(ControlFrame::Ping { id });
            }

            // 3. Pending pongs.
            let pongs: Vec<_> = self.pending_pongs.drain(..).collect();
            for id in pongs {
                if !b.fits(5) {
                    break;
                }
                let dst = b.reserve(5);
                let n = encode_pong(dst, id);
                b.commit(n);
                sent_frames.push(ControlFrame::Pong { id });
            }

            // 3. AuthComplete frame (sent after we verified peer's auth).
            if self.pending_auth_complete && b.fits(1) {
                let dst = b.reserve(1);
                encode_auth_complete(dst);
                b.commit(1);
                self.pending_auth_complete = false;
                self.auth_complete_sent = true;
                self.try_finish_auth(now);
            }

            // 4. Auth frames (during Authenticating state).
            if let Some(ref mut frag) = self.auth_send {
                while b.fits(AUTH_HEADER_SIZE + 1) {
                    let avail = b.remaining() - AUTH_HEADER_SIZE;
                    let slot = b.reserve(AUTH_HEADER_SIZE + avail);
                    let (hdr_dst, data_dst) = slot.split_at_mut(AUTH_HEADER_SIZE);
                    if let Some((offset, len, fin)) = frag.emit(data_dst) {
                        encode_auth_header(hdr_dst, offset, len as u16, fin);
                        b.commit(AUTH_HEADER_SIZE + len);
                        sent_auth.push((offset, len));
                    } else {
                        break;
                    }
                }
            }

            // 5. Rekey frames.
            if let Some(ref mut frag) = self.rekey_send {
                let is_initiator = self.rekey_kem.is_some();
                let hdr_size = if is_initiator {
                    REKEY_HEADER_SIZE
                } else {
                    REKEY_ACK_HEADER_SIZE
                };
                while b.fits(hdr_size + 1) {
                    let avail = b.remaining() - hdr_size;
                    let slot = b.reserve(hdr_size + avail);
                    let (hdr_dst, data_dst) = slot.split_at_mut(hdr_size);
                    if let Some((offset, len, fin)) = frag.emit(data_dst) {
                        if is_initiator {
                            encode_rekey_header(hdr_dst, offset, len as u16, fin);
                        } else {
                            encode_rekey_ack_header(hdr_dst, offset, len as u16, fin);
                        }
                        b.commit(hdr_size + len);
                        sent_rekey.push((offset, len));
                    } else {
                        break;
                    }
                }
            }
            // Clean up drained rekey fragmenter.
            if self
                .rekey_send
                .as_ref()
                .map_or(false, |f| f.is_done() && !f.has_retransmits())
            {
                self.rekey_send = None;
            }

            // Only in Established/Closing: stream & channel data + control frames.
            if self.state == State::Established || self.state == State::Closing {
                // 5. Per-stream MaxData updates.
                let mut wu = std::mem::take(&mut self.scratch_stream_max_data);
                wu.clear();
                self.streams.max_data_updates(&mut wu);
                if !wu.is_empty() {
                    tracing::trace!(
                        cid = self.connection_id.0,
                        count = wu.len(),
                        "stream max_data updates generated",
                    );
                }
                for (stream_id, max_data) in wu.drain(..) {
                    self.pending_stream_max_data.insert(stream_id, max_data);
                }
                wu.extend(self.pending_stream_max_data.drain());
                let mut idx = 0;
                while idx < wu.len() {
                    if !b.fits(17) {
                        for &(sid, md) in &wu[idx..] {
                            self.pending_stream_max_data.insert(sid, md);
                        }
                        break;
                    }
                    let (stream_id, max_data) = wu[idx];
                    let dst = b.reserve(17);
                    let n = encode_stream_max_data(dst, stream_id, max_data);
                    b.commit(n);
                    sent_frames.push(ControlFrame::StreamMaxData { stream_id, max_data });
                    idx += 1;
                }
                self.scratch_stream_max_data = wu;

                // 5a. MAX_STREAMS update.
                if let Some(count) = self.streams.drain_max_streams_update() {
                    if b.fits(9) {
                        let dst = b.reserve(9);
                        let n = encode_max_streams(dst, count);
                        b.commit(n);
                        sent_frames.push(ControlFrame::MaxStreams { count });
                    } else {
                        self.streams.requeue_max_streams_update();
                    }
                }

                // 6a. Channel opens.
                let mut opens = std::mem::take(&mut self.scratch_pending_opens);
                opens.clear();
                self.channels.drain_pending_opens(&mut opens);
                let mut idx = 0;
                while idx < opens.len() {
                    if !b.fits(9) {
                        for &cid in &opens[idx..] {
                            self.channels.requeue_open(cid);
                        }
                        break;
                    }
                    let channel_id = opens[idx];
                    let dst = b.reserve(9);
                    let n = encode_channel_open(dst, channel_id);
                    b.commit(n);
                    sent_frames.push(ControlFrame::ChannelOpen { channel_id });
                    idx += 1;
                }
                self.scratch_pending_opens = opens;

                // 6b. Channel fins.
                let mut fins = std::mem::take(&mut self.scratch_pending_fins);
                fins.clear();
                self.channels.drain_pending_fins(&mut fins);
                let mut idx = 0;
                while idx < fins.len() {
                    if !b.fits(17) {
                        for &(cid, mid) in &fins[idx..] {
                            self.channels.requeue_fin(cid, mid);
                        }
                        break;
                    }
                    let (channel_id, last_message_id) = fins[idx];
                    let dst = b.reserve(17);
                    let n = encode_channel_fin(dst, channel_id, last_message_id);
                    b.commit(n);
                    sent_frames.push(ControlFrame::ChannelFin {
                        channel_id,
                        last_message_id,
                    });
                    idx += 1;
                }
                self.scratch_pending_fins = fins;

                // 6c. MAX_CHANNELS update.
                if let Some(count) = self.channels.drain_max_channels_update() {
                    if b.fits(9) {
                        let dst = b.reserve(9);
                        let n = encode_max_channels(dst, count);
                        b.commit(n);
                        sent_frames.push(ControlFrame::MaxChannels { count });
                    } else {
                        self.channels.requeue_max_channels_update();
                    }
                }

                // 6d. Per-channel MaxData updates (carries both max_data
                //     and max_messages).
                let mut mds = std::mem::take(&mut self.scratch_channel_max_data);
                mds.clear();
                self.channels.drain_max_data_updates(&mut mds);
                let mut idx = 0;
                while idx < mds.len() {
                    if !b.fits(25) {
                        for &(cid, _, _) in &mds[idx..] {
                            self.channels.requeue_max_data_update(cid);
                        }
                        break;
                    }
                    let (channel_id, max_data, max_messages) = mds[idx];
                    let dst = b.reserve(25);
                    let n = encode_channel_max_data(dst, channel_id, max_data, max_messages);
                    b.commit(n);
                    sent_frames.push(ControlFrame::ChannelMaxData {
                        channel_id,
                        max_data,
                        max_messages,
                    });
                    idx += 1;
                }
                self.scratch_channel_max_data = mds;

                // 6e. ChannelEvict frames.
                let mut evs = std::mem::take(&mut self.scratch_pending_evicts);
                evs.clear();
                self.channels.drain_pending_evicts(&mut evs);
                let mut idx = 0;
                while idx < evs.len() {
                    if !b.fits(25) {
                        for &(cid, mid, size) in &evs[idx..] {
                            self.channels.requeue_evict(cid, mid, size);
                        }
                        break;
                    }
                    let (channel_id, message_id, size) = evs[idx];
                    let dst = b.reserve(25);
                    let n = encode_channel_evict(dst, channel_id, message_id, size);
                    b.commit(n);
                    sent_frames.push(ControlFrame::ChannelEvict {
                        channel_id,
                        message_id,
                        size,
                    });
                    idx += 1;
                }
                self.scratch_pending_evicts = evs;

                // 7. Stream data.
                while b.fits(STREAM_HEADER_SIZE + 1) {
                    let avail = b.remaining() - STREAM_HEADER_SIZE;
                    let slot = b.reserve(STREAM_HEADER_SIZE + avail);
                    let (hdr_dst, data_dst) = slot.split_at_mut(STREAM_HEADER_SIZE);
                    if let Some((stream_id, offset, len, fin)) = self.streams.emit(data_dst) {
                        tracing::trace!(
                            cid = self.connection_id.0,
                            sid = stream_id,
                            offset,
                            len,
                            fin,
                            "emit stream data",
                        );
                        encode_stream_header(hdr_dst, stream_id, offset, len as u16, fin);
                        b.commit(STREAM_HEADER_SIZE + len);
                        // Phantom FIN byte for ack tracking.
                        sent_streams.push((stream_id, offset, len + (fin as usize)));
                    } else {
                        break;
                    }
                }

                // 8. Channel data.
                while b.fits(CHANNEL_HEADER_SIZE + 1) {
                    let avail = b.remaining() - CHANNEL_HEADER_SIZE;
                    let slot = b.reserve(CHANNEL_HEADER_SIZE + avail);
                    let (hdr_dst, data_dst) = slot.split_at_mut(CHANNEL_HEADER_SIZE);
                    if let Some((ch_id, msg_id, offset, len, fin)) = self.channels.emit(data_dst) {
                        tracing::trace!(
                            cid = self.connection_id.0,
                            chid = ch_id,
                            mid = msg_id,
                            offset,
                            len,
                            fin,
                            "emit channel data",
                        );
                        encode_channel_header(hdr_dst, ch_id, msg_id, offset, len as u16, fin);
                        b.commit(CHANNEL_HEADER_SIZE + len);
                        sent_channels.push((ch_id, msg_id, offset, len));
                    } else {
                        break;
                    }
                }

                // 9. ConnectionClose.
                let send_close = if let Some((error_code, _)) = &self.pending_close {
                    *error_code != 0
                        || (sent_streams.is_empty()
                            && sent_channels.is_empty()
                            && !self.streams.has_pending()
                            && !self.channels.has_pending()
                            && self.send_ack.in_flight_count() == 0)
                } else {
                    false
                };
                if send_close {
                    let (error_code, reason) = self.pending_close.take().unwrap();
                    let needed = 7 + reason.len();
                    if b.fits(needed) {
                        let dst = b.reserve(needed);
                        let n = encode_connection_close(dst, error_code, &reason);
                        b.commit(n);
                        sent_frames.push(ControlFrame::ConnectionClose { error_code, reason });
                    } else {
                        self.pending_close = Some((error_code, reason));
                    }
                }
            }

            b.len()
        };

        // Nothing to send at all?
        if plaintext_len == 0 {
            return Err(Error::Done);
        }
        tracing::trace!(
            cid = self.connection_id.0,
            counter = self.packet_counter,
            epoch = self.send_epoch,
            plaintext_len,
            ack_len,
            "emit data packet",
        );

        let ack_only = plaintext_len <= ack_len;

        // 10. Write data packet header with current epoch.
        let counter = self.packet_counter;
        self.packet_counter += 1;
        let hdr_len = encode_data_header(buf, self.send_epoch, peer_id.0, counter);
        debug_assert_eq!(hdr_len, DATA_HEADER_SIZE);

        // 11. Encrypt in place.  AAD = packet header; msg = buf[hdr_len..hdr_len+plaintext_len];
        // tag goes into buf[hdr_len+plaintext_len..hdr_len+plaintext_len+AEAD_TAG_SIZE].
        let send_key = self.send_keys[self.send_epoch as usize].as_ref().unwrap();
        let (header, payload_region) = buf.split_at_mut(hdr_len);
        let written = send_key.encrypt_in_place(
            counter,
            header,
            &mut payload_region[..plaintext_len + AEAD_TAG_SIZE],
            plaintext_len,
        );
        let total = hdr_len + written;

        // 12. Record for ACK/loss tracking.
        // ACK-only packets are not ack-eliciting — don't track them for loss.
        if !ack_only {
            self.send_ack
                .on_packet_sent(counter, sent_streams, sent_channels, sent_frames, sent_auth, sent_rekey, now);
        }
        // Record ACK-of-ACK mapping: when peer ACKs this counter,
        // we can advance recv_ack floor.
        if let Some(ack_floor) = pending_ack_floor {
            self.ack_floor_by_counter.insert(counter, ack_floor);
            // Bound: remove entries for packets no longer in flight.
            if self.ack_floor_by_counter.len() > 256 {
                let in_flight = &self.send_ack;
                self.ack_floor_by_counter
                    .retain(|&c, _| in_flight.is_in_flight(c));
            }
        }
        self.last_send_at = Some(now);

        Ok((total, SendInfo { at: now }))
    }

    fn recv_data(&mut self, buf: &mut [u8], now: Instant) -> Result<usize, Error> {
        let buf_len = buf.len();
        if buf_len < DATA_HEADER_SIZE {
            return Err(Error::InvalidPacket);
        }

        // Parse data header inline (avoid parse_data_packet which gives an
        // immutable view; we need &mut for in-place decryption).
        let packet_type = buf[0];
        if packet_type < DATA_TYPE_BASE || packet_type > DATA_TYPE_BASE + EPOCH_MASK {
            return Err(Error::InvalidPacket);
        }
        let epoch = packet_type - DATA_TYPE_BASE;
        let counter = u64::from_be_bytes(buf[9..17].try_into().unwrap());

        // Duplicate check before decryption.
        if self.recv_ack.should_accept(counter) == RecvResult::Duplicate {
            return Ok(buf_len);
        }

        // Split into header (AAD) and payload, decrypt payload in place.
        let (header, payload) = buf.split_at_mut(DATA_HEADER_SIZE);
        let recv_key = self.recv_keys[epoch as usize]
            .as_ref()
            .ok_or(Error::CryptoError)?;
        let plaintext_len = recv_key
            .decrypt_in_place(counter, header, payload)
            .ok_or(Error::CryptoError)?;
        let plaintext = &payload[..plaintext_len];

        self.last_recv_at = Some(now);

        // If peer is sending on a newer epoch, advance our send_epoch to match.
        if epoch != self.send_epoch && self.send_keys[epoch as usize].is_some() {
            let old_epoch = self.send_epoch;
            self.send_epoch = epoch;
            self.on_epoch_change(old_epoch, epoch);
        }

        // Decode and route frames, tracking whether any are ack-eliciting.
        let mut ack_eliciting = false;
        for frame_result in decode_frames(plaintext) {
            let frame = frame_result.map_err(|_| Error::InvalidPacket)?;
            if !matches!(frame, Frame::Ack { .. }) {
                ack_eliciting = true;
            }
            self.process_frame(frame, now)?;
        }

        // Only commit (trigger ACK generation) for ack-eliciting packets.
        // ACK-only packets don't require an ACK response — this prevents
        // infinite ACK loops.
        if ack_eliciting {
            self.recv_ack.commit(counter, now);
        }

        Ok(buf_len)
    }

    fn process_frame(&mut self, frame: Frame<'_>, now: Instant) -> Result<(), Error> {
        match frame {
            Frame::Ack {
                largest,
                delay,
                ranges,
            } => {
                // Reuse scratch buffers for ranges + ack/loss reports — these
                // are per-packet and otherwise cost 9 fresh allocations each.
                let mut ranges_buf = std::mem::take(&mut self.scratch_ack_ranges);
                ranges_buf.clear();
                parse_ack_ranges_into(ranges, &mut ranges_buf);
                let ack = Ack {
                    largest_ack: largest,
                    ack_delay: delay,
                    ranges: ranges_buf,
                };
                let mut acked = std::mem::take(&mut self.scratch_acked);
                let mut loss = std::mem::take(&mut self.scratch_loss);
                self.send_ack.on_ack_received_into(&ack, now, &mut acked, &mut loss);
                self.handle_ack_ref(&acked);
                self.handle_loss_ref(&mut loss);
                // Put back.
                self.scratch_ack_ranges = ack.ranges;
                self.scratch_acked = acked;
                self.scratch_loss = loss;

                // ACK-of-ACK: peer confirmed receipt of our packets.
                // Advance recv_ack floor for the highest ack_floor we sent.
                let mut best_floor = 0u64;
                self.ack_floor_by_counter.retain(|&counter, &mut floor| {
                    if counter <= ack.largest_ack {
                        best_floor = best_floor.max(floor);
                        false // remove — acked
                    } else {
                        true // keep — still in-flight
                    }
                });
                if best_floor > 0 {
                    self.recv_ack.advance_floor(best_floor);
                    self.try_clean_prev_epoch();
                }
            }

            Frame::Ping { id } => {
                // Limit pending pongs to prevent memory exhaustion from malicious peers.
                if self.pending_pongs.len() < 32 {
                    self.pending_pongs.push(id);
                }
            }

            Frame::Pong { id } => {
                if let Some(sent_at) = self.pings_in_flight.remove(&id) {
                    self.ping_rtt = Some(now.duration_since(sent_at));
                }
            }

            Frame::Auth { offset, fin, data } => {
                // Auth payload has a fixed size; reject fragments that would
                // overrun it.  Validation is at the call site since the
                // assembler itself is size-agnostic.
                let end = offset.saturating_add(data.len() as u64);
                if end > AUTH_PAYLOAD_SIZE as u64 {
                    return Err(Error::AuthFailed);
                }
                if let Some(ref mut assembler) = self.auth_recv {
                    let _ = assembler.write(offset, data, fin);
                    if assembler.is_complete() {
                        self.verify_auth_payload()?;
                        self.try_finish_auth(now);
                    }
                }
            }

            Frame::AuthComplete => {
                self.auth_complete_received = true;
                self.try_finish_auth(now);
            }

            Frame::Stream {
                stream_id,
                offset,
                fin,
                data,
            } => {
                if self.state == State::Established || self.state == State::Closing {
                    tracing::trace!(
                        cid = self.connection_id.0,
                        sid = stream_id,
                        offset,
                        len = data.len(),
                        fin,
                        "recv stream data",
                    );
                    if let Err(e) = self.streams.recv(stream_id, offset, data, fin) {
                        match e {
                            crate::stream::manager::StreamError::TooManyStreams => {
                                self.close_with_error(1, b"too many streams");
                            }
                            crate::stream::manager::StreamError::FlowControl => {
                                self.close_with_error(3, b"stream flow violation");
                            }
                            crate::stream::manager::StreamError::FinalSizeMismatch => {
                                self.close_with_error(3, b"stream final size mismatch");
                            }
                            _ => {}
                        }
                    }
                }
            }

            Frame::Channel {
                channel_id,
                message_id,
                offset,
                fin,
                data,
            } => {
                if self.state == State::Established || self.state == State::Closing {
                    tracing::trace!(
                        cid = self.connection_id.0,
                        chid = channel_id,
                        mid = message_id,
                        offset,
                        len = data.len(),
                        fin,
                        "recv channel data",
                    );
                    if let Err(e) = self.channels.recv(channel_id, message_id, offset, data, fin) {
                        match e {
                            crate::channel::manager::ChannelError::TooManyChannels => {
                                self.close_with_error(2, b"too many channels");
                            }
                            crate::channel::manager::ChannelError::ProtocolViolation => {
                                self.close_with_error(3, b"channel flow violation");
                            }
                            _ => {}
                        }
                    }
                }
            }

            Frame::StreamMaxData {
                stream_id,
                max_data,
            } => {
                tracing::trace!(
                    cid = self.connection_id.0,
                    sid = stream_id,
                    max_data,
                    "recv StreamMaxData",
                );
                self.streams.update_send_max_data(stream_id, max_data);
            }

            Frame::MaxStreams { count } => {
                tracing::trace!(cid = self.connection_id.0, count, "recv MaxStreams");
                self.streams.update_send_max_streams(count);
            }

            Frame::MaxChannels { count } => {
                tracing::trace!(cid = self.connection_id.0, count, "recv MaxChannels");
                self.channels.update_send_max_channels(count);
            }

            Frame::ChannelOpen { channel_id } => {
                if self.state == State::Established || self.state == State::Closing {
                    match self.channels.on_peer_open(channel_id) {
                        Ok(()) => {
                            tracing::debug!(
                                cid = self.connection_id.0,
                                chid = channel_id,
                                side = "peer",
                                "channel opened",
                            );
                        }
                        Err(e) => {
                            if matches!(e, crate::channel::manager::ChannelError::TooManyChannels) {
                                self.close_with_error(2, b"too many channels");
                            }
                        }
                    }
                }
            }

            Frame::ChannelFin { channel_id, last_message_id } => {
                if self.state == State::Established || self.state == State::Closing {
                    match self.channels.on_peer_fin(channel_id, last_message_id) {
                        Ok(()) => {
                            tracing::debug!(
                                cid = self.connection_id.0,
                                chid = channel_id,
                                last_mid = last_message_id,
                                side = "peer",
                                "channel fin",
                            );
                        }
                        Err(e) => {
                            if matches!(e, crate::channel::manager::ChannelError::ProtocolViolation) {
                                self.close_with_error(3, b"channel fin violation");
                            }
                        }
                    }
                }
            }

            Frame::ChannelMaxData {
                channel_id,
                max_data,
                max_messages,
            } => {
                if self.state == State::Established || self.state == State::Closing {
                    tracing::trace!(
                        cid = self.connection_id.0,
                        chid = channel_id,
                        max_data,
                        max_messages,
                        "recv ChannelMaxData",
                    );
                    self.channels
                        .on_peer_max_data(channel_id, max_data, max_messages);
                }
            }

            Frame::ChannelEvict { channel_id, message_id, size } => {
                if self.state == State::Established || self.state == State::Closing {
                    match self.channels.on_peer_evict(channel_id, message_id, size) {
                        Ok(()) => {
                            tracing::debug!(
                                cid = self.connection_id.0,
                                chid = channel_id,
                                mid = message_id,
                                size,
                                side = "peer",
                                "channel evict",
                            );
                        }
                        Err(e) => match e {
                            crate::channel::manager::ChannelError::ProtocolViolation => {
                                self.close_with_error(3, b"channel evict violation");
                            }
                            crate::channel::manager::ChannelError::TooManyChannels => {
                                self.close_with_error(2, b"too many channels");
                            }
                            _ => {}
                        },
                    }
                }
            }

            Frame::Rekey { offset, fin, data } => {
                self.on_rekey_frame(offset, data, fin)?;
            }

            Frame::RekeyAck { offset, fin, data } => {
                self.on_rekey_ack_frame(offset, data, fin)?;
            }

            Frame::ConnectionClose { error_code, .. } => {
                tracing::info!(
                    cid = self.connection_id.0,
                    code = error_code,
                    "closed: peer sent ConnectionClose",
                );
                self.state = State::Closed;
            }
        }

        Ok(())
    }

    // -- Internals: Rekey ----------------------------------------------------

    /// Initiate a key rotation.
    ///
    /// Generates a new ephemeral KEM keypair and starts sending its public key
    /// via Rekey frames. The peer will respond with RekeyAck carrying a ciphertext.
    /// Both sides derive new keys for the next epoch.
    /// Called when send_epoch changes.  Cleans up old epoch keys:
    /// - N-2: immediately (definitely stale)
    /// - N-1: tracked for deferred cleanup when ACK-of-ACK confirms
    fn on_epoch_change(&mut self, old_epoch: u8, new_epoch: u8) {
        // Clean N-2 immediately.
        let n_minus_2 = (new_epoch.wrapping_sub(2)) & 0x03;
        self.send_keys[n_minus_2 as usize] = None;
        self.recv_keys[n_minus_2 as usize] = None;

        // Track N-1 for deferred cleanup.
        self.prev_epoch = Some(old_epoch);
        self.prev_epoch_floor = self.recv_ack.floor();
    }

    /// Check if N-1 epoch keys should be cleaned (ACK-of-ACK confirmed).
    fn try_clean_prev_epoch(&mut self) {
        if let Some(prev) = self.prev_epoch {
            if self.recv_ack.floor() > self.prev_epoch_floor {
                // Peer confirmed our ACK → safe to clean prev epoch.
                self.send_keys[prev as usize] = None;
                self.recv_keys[prev as usize] = None;
                self.prev_epoch = None;
            }
        }
    }

    pub fn start_rekey(&mut self) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        if self.rekey_send.is_some() || self.rekey_recv.is_some() {
            // Rekey already in progress.
            return Err(Error::InvalidState);
        }
        let kem = KemPrivateKey::generate();
        let pk_bytes = kem.public_key().to_bytes();
        self.rekey_kem = Some(kem);
        self.rekey_send = Some(MessageFragmenter::new(pk_bytes.to_vec()));
        self.rekey_recv = Some(MessageAssembler::new());
        tracing::debug!(
            cid = self.connection_id.0,
            epoch = self.send_epoch,
            "rekey initiated (initiator)",
        );
        Ok(())
    }

    /// Handle a received Rekey frame (peer is initiating rekey).
    fn on_rekey_frame(&mut self, offset: u64, data: &[u8], fin: bool) -> Result<(), Error> {
        if self.state != State::Established {
            return Ok(());
        }
        // As responder we expect peer's KEM public key — reject overruns.
        let end = offset.saturating_add(data.len() as u64);
        if end > KEM_PUBLIC_KEY_SIZE as u64 {
            return Err(Error::CryptoError);
        }
        // Collision: both sides initiated rekey simultaneously.
        // Tie-break: the connection initiator wins, the responder yields.
        if self.rekey_kem.is_some() {
            if self.is_initiator {
                // We win — ignore peer's Rekey, they'll accept our RekeyAck.
                return Ok(());
            }
            // We yield — drop our rekey and become responder.
            self.rekey_kem = None;
            self.rekey_send = None;
            self.rekey_recv = None;
        }
        // Create assembler for peer's public key if needed.
        if self.rekey_recv.is_none() {
            self.rekey_recv = Some(MessageAssembler::new());
        }
        if let Some(ref mut assembler) = self.rekey_recv {
            let _ = assembler.write(offset, data, fin);
            if assembler.is_complete() {
                self.complete_rekey_as_responder()?;
            }
        }
        Ok(())
    }

    /// Handle a received RekeyAck frame (we initiated, peer responded).
    fn on_rekey_ack_frame(&mut self, offset: u64, data: &[u8], fin: bool) -> Result<(), Error> {
        if self.rekey_kem.is_none() {
            // Not initiating a rekey — ignore.
            return Ok(());
        }
        // As initiator we expect peer's KEM ciphertext.
        let end = offset.saturating_add(data.len() as u64);
        if end > KEM_CIPHERTEXT_SIZE as u64 {
            return Err(Error::CryptoError);
        }
        if let Some(ref mut assembler) = self.rekey_recv {
            let _ = assembler.write(offset, data, fin);
            if assembler.is_complete() {
                self.complete_rekey_as_initiator()?;
            }
        }
        Ok(())
    }

    /// Responder: received peer's KEM public key → encapsulate → send RekeyAck.
    ///
    /// Installs new keys but does NOT switch send_epoch yet.
    /// The epoch is advanced when we receive a packet on the new epoch from
    /// the initiator (confirming they have the new keys too).
    fn complete_rekey_as_responder(&mut self) -> Result<(), Error> {
        let assembler = self.rekey_recv.take().unwrap();
        let pk_bytes = assembler.take();
        if pk_bytes.len() != KEM_PUBLIC_KEY_SIZE {
            self.rekey_send = None;
            return Err(Error::CryptoError);
        }
        let peer_pk = KemPublicKey::from_bytes(pk_bytes[..KEM_PUBLIC_KEY_SIZE].try_into().unwrap());

        let resp_kem = KemPrivateKey::generate();
        let (ct, shared_secret) = match resp_kem.encapsulate(&peer_pk) {
            Some(result) => result,
            None => {
                self.rekey_send = None;
                return Err(Error::CryptoError);
            }
        };

        // Derive new keys for next epoch.
        let next_epoch = (self.send_epoch + 1) & 0x03;
        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ct);
        let (i2r, r2i) = keys.into_keys();
        // Responder: send=r2i, recv=i2r.
        self.send_keys[next_epoch as usize] = Some(r2i);
        self.recv_keys[next_epoch as usize] = Some(i2r);

        // Send RekeyAck with ciphertext (still on current epoch).
        self.rekey_send = Some(MessageFragmenter::new(ct.to_bytes().to_vec()));

        // Don't switch send_epoch yet: wait until initiator sends on new epoch.
        tracing::debug!(
            cid = self.connection_id.0,
            current = self.send_epoch,
            next = next_epoch,
            "rekey_ack queued, awaiting peer to switch",
        );
        Ok(())
    }

    /// Initiator: received peer's ciphertext → decapsulate → install new keys.
    fn complete_rekey_as_initiator(&mut self) -> Result<(), Error> {
        let assembler = self.rekey_recv.take().unwrap();
        let ct_bytes = assembler.take();
        if ct_bytes.len() != KEM_CIPHERTEXT_SIZE {
            self.rekey_kem = None;
            self.rekey_send = None;
            return Err(Error::CryptoError);
        }
        let ct = KemCiphertext::from_bytes(ct_bytes[..KEM_CIPHERTEXT_SIZE].try_into().unwrap());

        let kem = self.rekey_kem.take().unwrap();
        let peer_pk = kem.public_key();
        let shared_secret = match kem.decapsulate(&ct) {
            Some(ss) => ss,
            None => {
                self.rekey_send = None;
                return Err(Error::CryptoError);
            }
        };

        // Derive new keys for next epoch.
        let next_epoch = (self.send_epoch + 1) & 0x03;
        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ct);
        let (i2r, r2i) = keys.into_keys();
        // Initiator sends with i2r, receives with r2i.
        self.send_keys[next_epoch as usize] = Some(i2r);
        self.recv_keys[next_epoch as usize] = Some(r2i);

        // Switch to new epoch and clean old keys.
        let old_epoch = self.send_epoch;
        self.send_epoch = next_epoch;
        self.on_epoch_change(old_epoch, next_epoch);

        // Clean up.
        self.rekey_send = None;

        tracing::info!(
            cid = self.connection_id.0,
            from = old_epoch,
            to = next_epoch,
            "rekey done",
        );
        Ok(())
    }

    fn handle_ack_ref(&mut self, acked: &AckReport) {
        for &(stream_id, offset, len) in &acked.streams {
            tracing::trace!(
                cid = self.connection_id.0,
                sid = stream_id,
                offset,
                len,
                "ack stream data",
            );
            self.streams.ack(stream_id, offset, len);
        }
        for &(channel_id, message_id, offset, len) in &acked.channels {
            tracing::trace!(
                cid = self.connection_id.0,
                chid = channel_id,
                mid = message_id,
                offset,
                len,
                "ack channel data",
            );
            self.channels.ack(channel_id, message_id, offset, len);
        }
        // Control-frame acks: credit is granted unilaterally on cleanup,
        // not as a response to peer's close.  Nothing to do here.
    }

    fn handle_loss_ref(&mut self, loss: &mut LossReport) {
        for &(stream_id, offset, len) in &loss.streams {
            self.streams.loss(stream_id, offset, len);
        }
        loss.streams.clear();
        for &(channel_id, message_id, offset, len) in &loss.channels {
            self.channels.loss(channel_id, message_id, offset, len);
        }
        loss.channels.clear();
        for frame in loss.frames.drain(..) {
            match frame {
                ControlFrame::Pong { id } => self.pending_pongs.push(id),
                ControlFrame::StreamMaxData { stream_id, max_data } => {
                    self.pending_stream_max_data.insert(stream_id, max_data);
                }
                ControlFrame::ChannelOpen { channel_id } => {
                    self.channels.requeue_open(channel_id);
                }
                ControlFrame::ChannelFin { channel_id, last_message_id } => {
                    self.channels.requeue_fin(channel_id, last_message_id);
                }
                ControlFrame::MaxStreams { count: _ } => {
                    self.streams.requeue_max_streams_update();
                }
                ControlFrame::MaxChannels { count: _ } => {
                    self.channels.requeue_max_channels_update();
                }
                ControlFrame::ChannelMaxData { channel_id, max_data: _, max_messages: _ } => {
                    self.channels.requeue_max_data_update(channel_id);
                }
                ControlFrame::ChannelEvict { channel_id, message_id, size } => {
                    self.channels.requeue_evict(channel_id, message_id, size);
                }
                ControlFrame::ConnectionClose { error_code, reason } => {
                    self.pending_close = Some((error_code, reason));
                }
                ControlFrame::Ping { id } => {
                    let _ = id;
                }
                ControlFrame::AuthComplete => {
                    self.pending_auth_complete = true;
                }
            }
        }
        for &(offset, len) in &loss.auth {
            if let Some(ref mut frag) = self.auth_send {
                frag.loss(offset, len);
            }
        }
        loss.auth.clear();
        for &(offset, len) in &loss.rekey {
            if let Some(ref mut frag) = self.rekey_send {
                frag.loss(offset, len);
            }
        }
        loss.rekey.clear();
    }

}

fn parse_ack_ranges_into(data: &[u8], out: &mut Vec<AckRange>) {
    let mut pos = 0;
    while pos + 16 <= data.len() {
        let gap = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        let length = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap());
        out.push(AckRange { gap, length });
        pos += 16;
    }
}
