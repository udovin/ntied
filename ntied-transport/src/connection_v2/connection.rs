use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::crypto::{
    AEAD_TAG_SIZE, EncryptionKey, EncryptionKeys, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE,
    KemCiphertext, KemPrivateKey, KemPublicKey, PUBLIC_KEY_SIZE, PrivateKey, PublicKey,
    SIGNATURE_SIZE, Signature, compute_transcript_hash,
};

use super::ack::{Ack, AckRange, AckReport, ControlFrame, LossReport, RecvAckState, RecvResult, SendAckState};
use super::channel::manager::ChannelManager;
use super::channel::message::{MessageAssembler, MessageFragmenter};
use super::stream::manager::StreamManager;
use super::wire::frame::{
    AUTH_HEADER_SIZE, CHANNEL_HEADER_SIZE, Frame, REKEY_ACK_HEADER_SIZE, REKEY_HEADER_SIZE,
    STREAM_HEADER_SIZE, decode_frames, encode_ack, encode_auth_complete, encode_auth_header,
    encode_channel_fin, encode_channel_header, encode_channel_open, encode_connection_close,
    encode_max_channels,
    encode_ping, encode_pong, encode_rekey_ack_header, encode_rekey_header, encode_stream_header,
    encode_max_streams, encode_window_update,
};
use super::wire::packet::{
    DATA_HEADER_SIZE, INIT_ACK_SIZE, INIT_SIZE, PacketHeader, encode_data_header, encode_init,
    encode_init_ack, parse_data_packet, parse_init, parse_init_ack, peek_header,
};

const AUTH_PAYLOAD_SIZE: usize = PUBLIC_KEY_SIZE + SIGNATURE_SIZE;

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
    /// Keepalive ping interval. `None` disables keepalive. Default: `Some(5s)`.
    pub keepalive: Option<Duration>,
    /// Connection is closed if no packets received within this duration. Default: 30s.
    pub idle_timeout: Duration,
    /// Connection is closed if handshake not completed within this duration. Default: 10s.
    pub handshake_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_streams: 256,
            max_channels: 256,
            stream_buf_size: 65536,
            channel_buf_size: 65536,
            keepalive: Some(Duration::from_secs(5)),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
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
    rekey_peer_pk: Option<KemPublicKey>,
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
    pending_window_updates: HashMap<u64, u64>,
    pending_close: Option<(u32, Vec<u8>)>,
    pending_auth_complete: bool,

    // Reusable scratch buffers for emit loop — avoid per-emit allocations.
    scratch_window_updates: Vec<(u64, u64)>,
    scratch_pending_opens: Vec<u64>,
    scratch_pending_fins: Vec<(u64, u64)>,
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
        let channels = ChannelManager::new(config.channel_buf_size, is_initiator, config.max_channels);
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
            rekey_peer_pk: None,
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
            created_at: Instant::now(),
            last_recv_at: None,
            last_send_at: None,
            loss_detection_pending: false,
            ack_floor_by_counter: HashMap::new(),
            pending_pongs: Vec::new(),
            pending_window_updates: HashMap::new(),
            pending_close: None,
            pending_auth_complete: false,
            scratch_window_updates: Vec::new(),
            scratch_pending_opens: Vec::new(),
            scratch_pending_fins: Vec::new(),
        }
    }

    // -- Packet I/O ----------------------------------------------------------

    /// Process a received packet.
    pub fn recv(&mut self, buf: &[u8], info: RecvInfo) -> Result<usize, Error> {
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
                tracing::trace!(stream_id, n, fin, "stream_read");
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
            tracing::trace!(stream_id, written = n, fin, free = self.streams.writable().count(), "stream_write");
        }
        result
    }

    // -- Channel API ---------------------------------------------------------

    /// Create a channel and queue a ChannelOpen frame.
    /// The channel is lazily created in connection_v2 and the peer
    /// is notified via ChannelOpen. Returns the channel_id.
    pub fn open_channel(&mut self, channel_id: u64) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.channels
            .on_local_open(channel_id)
            .map_err(|_| Error::Done)
    }

    /// Drain channel IDs whose state changed since last call.
    /// Drain channel IDs whose state changed since last call into `out`.
    /// Caller's buffer is appended to.
    pub fn drain_updated_channels(&mut self, out: &mut Vec<u64>) {
        self.channels.drain_updated(out);
    }

    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels.readable_channels()
    }

    pub fn channel_send(&mut self, channel_id: u64, data: Vec<u8>) -> Result<u64, Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.channels
            .send(channel_id, data)
            .map_err(|_| Error::Done)
    }

    /// True if sending `data_len` bytes on `channel_id` would evict a message.
    /// Application can use this as a backpressure signal.
    pub fn channel_would_evict(&self, channel_id: u64, data_len: usize) -> bool {
        self.channels.would_evict(channel_id, data_len)
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
        Ok(())
    }

    // -- Connection lifecycle ------------------------------------------------

    /// Initiate a graceful close.
    pub fn close(&mut self, error_code: u32, reason: &[u8]) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.pending_close = Some((error_code, reason.to_vec()));
        self.state = State::Closing;
        Ok(())
    }

    /// Immediately close due to a protocol error.
    /// Skips data drain — ConnectionClose is sent as soon as possible.
    fn close_with_error(&mut self, error_code: u32, reason: &[u8]) {
        if self.state == State::Closed || self.state == State::Closing {
            return;
        }
        self.pending_close = Some((error_code, reason.to_vec()));
        self.state = State::Closing;
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
                return;
            }
        }

        // Idle timeout.
        if self.state == State::Established || self.state == State::Closing {
            let last = self.last_recv_at.expect("last_recv_at must be set in Established/Closing");
            if now.duration_since(last) >= self.config.idle_timeout {
                self.state = State::Closed;
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
        self.auth_recv = Some(MessageAssembler::new(AUTH_PAYLOAD_SIZE as u64));
        self.transcript_hash = Some(transcript_hash);
    }

    /// Check if auth is complete on both sides and transition to Established.
    fn try_finish_auth(&mut self) {
        if self.peer_authenticated && self.auth_complete_sent && self.auth_complete_received {
            self.state = State::Established;
            // Clean up auth state.
            self.auth_send = None;
            self.auth_recv = None;
            self.transcript_hash = None;
            // Start keepalive timer.
            self.schedule_next_ping(Instant::now());
        }
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
            let loss = self.send_ack.detect_timeout_losses(now);
            self.handle_loss(loss);
        }

        if buf.len() < DATA_HEADER_SIZE + 12 + AEAD_TAG_SIZE {
            return Err(Error::BufferTooShort);
        }

        let max_plaintext = buf.len() - DATA_HEADER_SIZE - AEAD_TAG_SIZE;
        let mut plaintext = Vec::with_capacity(max_plaintext.min(4096));
        let mut sent_streams: Vec<(u64, u64, usize)> = Vec::new();
        let mut sent_channels: Vec<(u64, u64, u64, usize)> = Vec::new();
        let mut sent_frames: Vec<ControlFrame> = Vec::new();
        let mut sent_auth: Vec<(u64, usize)> = Vec::new();
        let mut sent_rekey: Vec<(u64, usize)> = Vec::new();

        // 1. ACK frame — encode into plaintext now, but remember the length.
        //    If there's no other content to send, we'll truncate it.
        let mut pending_ack_floor: Option<u64> = None;
        let ack_len = if let Some((ack, ack_floor)) = self.recv_ack.generate_ack(now) {
            pending_ack_floor = Some(ack_floor);
            let ack_ranges: Vec<(u64, u64)> =
                ack.ranges.iter().map(|r| (r.gap, r.length)).collect();
            let max_ack = 12 + ack_ranges.len() * 16;
            let old_len = plaintext.len();
            plaintext.resize(old_len + max_ack, 0);
            let n = encode_ack(
                &mut plaintext[old_len..],
                ack.largest_ack,
                ack.ack_delay,
                &ack_ranges,
            );
            plaintext.truncate(old_len + n);
            n
        } else {
            0
        };

        // 2. Pending pings.
        for (id, sent_at) in self.pings_to_send.drain(..).collect::<Vec<_>>() {
            if plaintext.len() + 5 > max_plaintext {
                break;
            }
            let mut tmp = [0u8; 5];
            encode_ping(&mut tmp, id);
            plaintext.extend_from_slice(&tmp);
            self.pings_in_flight.insert(id, sent_at);
            sent_frames.push(ControlFrame::Ping { id });
        }

        // 3. Pending pongs.
        for id in self.pending_pongs.drain(..).collect::<Vec<_>>() {
            if plaintext.len() + 5 > max_plaintext {
                break;
            }
            let mut tmp = [0u8; 5];
            let n = encode_pong(&mut tmp, id);
            plaintext.extend_from_slice(&tmp[..n]);
            sent_frames.push(ControlFrame::Pong { id });
        }

        // 3. AuthComplete frame (sent after we verified peer's auth).
        if self.pending_auth_complete {
            if plaintext.len() + 1 <= max_plaintext {
                let mut tmp = [0u8; 1];
                encode_auth_complete(&mut tmp);
                plaintext.extend_from_slice(&tmp);
                self.pending_auth_complete = false;
                self.auth_complete_sent = true;
                self.try_finish_auth();
            }
        }

        // 4. Auth frames (during Authenticating state).
        if let Some(ref mut frag) = self.auth_send {
            while plaintext.len() + AUTH_HEADER_SIZE < max_plaintext {
                let avail = max_plaintext - plaintext.len() - AUTH_HEADER_SIZE;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((offset, len, fin)) = frag.emit(&mut data_buf) {
                    let mut hdr = [0u8; AUTH_HEADER_SIZE];
                    encode_auth_header(&mut hdr, offset, len as u16, fin);
                    plaintext.extend_from_slice(&hdr);
                    plaintext.extend_from_slice(&data_buf[..len]);
                    sent_auth.push((offset, len));
                } else {
                    break;
                }
            }
        }

        // 5. Rekey frames (Established only).
        if let Some(ref mut frag) = self.rekey_send {
            let is_initiator = self.rekey_kem.is_some();
            let hdr_size = if is_initiator {
                REKEY_HEADER_SIZE
            } else {
                REKEY_ACK_HEADER_SIZE
            };
            while plaintext.len() + hdr_size < max_plaintext {
                let avail = max_plaintext - plaintext.len() - hdr_size;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((offset, len, fin)) = frag.emit(&mut data_buf) {
                    let mut hdr = [0u8; 11]; // REKEY_HEADER_SIZE == AUTH_HEADER_SIZE == 11
                    if is_initiator {
                        encode_rekey_header(&mut hdr, offset, len as u16, fin);
                    } else {
                        encode_rekey_ack_header(&mut hdr, offset, len as u16, fin);
                    }
                    plaintext.extend_from_slice(&hdr);
                    plaintext.extend_from_slice(&data_buf[..len]);
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
            // 5. Window updates.  Drain new updates into pending HashMap, then
            // drain pending into reusable scratch and encode.
            let mut wu = std::mem::take(&mut self.scratch_window_updates);
            wu.clear();
            self.streams.window_updates(&mut wu);
            if !wu.is_empty() {
                tracing::trace!(count = wu.len(), "window_updates generated");
            }
            for (stream_id, max_offset) in wu.drain(..) {
                self.pending_window_updates.insert(stream_id, max_offset);
            }
            wu.extend(self.pending_window_updates.drain());
            let mut idx = 0;
            while idx < wu.len() {
                if plaintext.len() + 17 > max_plaintext {
                    for &(sid, off) in &wu[idx..] {
                        self.pending_window_updates.insert(sid, off);
                    }
                    break;
                }
                let (stream_id, max_offset) = wu[idx];
                let mut tmp = [0u8; 17];
                let n = encode_window_update(&mut tmp, stream_id, max_offset);
                plaintext.extend_from_slice(&tmp[..n]);
                sent_frames.push(ControlFrame::WindowUpdate { stream_id, max_offset });
                idx += 1;
            }
            self.scratch_window_updates = wu;

            // 5a. MAX_STREAMS update (reliable, single per-direction counter).
            if let Some(count) = self.streams.drain_max_streams_update() {
                if plaintext.len() + 9 <= max_plaintext {
                    let mut tmp = [0u8; 9];
                    let n = encode_max_streams(&mut tmp, count);
                    plaintext.extend_from_slice(&tmp[..n]);
                    sent_frames.push(ControlFrame::MaxStreams { count });
                } else {
                    // Couldn't fit in this packet — re-queue for next.
                    self.streams.requeue_max_streams_update();
                }
            }

            // 6a. Channel opens (reliable).  Drain into reusable scratch.
            let mut opens = std::mem::take(&mut self.scratch_pending_opens);
            opens.clear();
            self.channels.drain_pending_opens(&mut opens);
            let mut idx = 0;
            while idx < opens.len() {
                if plaintext.len() + 9 > max_plaintext {
                    for &cid in &opens[idx..] {
                        self.channels.requeue_open(cid);
                    }
                    break;
                }
                let channel_id = opens[idx];
                let mut tmp = [0u8; 9];
                let n = encode_channel_open(&mut tmp, channel_id);
                plaintext.extend_from_slice(&tmp[..n]);
                sent_frames.push(ControlFrame::ChannelOpen { channel_id });
                idx += 1;
            }
            self.scratch_pending_opens = opens;

            // 6b. Channel half-close fins (reliable).
            let mut fins = std::mem::take(&mut self.scratch_pending_fins);
            fins.clear();
            self.channels.drain_pending_fins(&mut fins);
            let mut idx = 0;
            while idx < fins.len() {
                if plaintext.len() + 17 > max_plaintext {
                    for &(cid, mid) in &fins[idx..] {
                        self.channels.requeue_fin(cid, mid);
                    }
                    break;
                }
                let (channel_id, last_message_id) = fins[idx];
                let mut tmp = [0u8; 17];
                let n = encode_channel_fin(&mut tmp, channel_id, last_message_id);
                plaintext.extend_from_slice(&tmp[..n]);
                sent_frames.push(ControlFrame::ChannelFin { channel_id, last_message_id });
                idx += 1;
            }
            self.scratch_pending_fins = fins;

            // 6c. MAX_CHANNELS update (reliable, single per-direction counter).
            if let Some(count) = self.channels.drain_max_channels_update() {
                if plaintext.len() + 9 <= max_plaintext {
                    let mut tmp = [0u8; 9];
                    let n = encode_max_channels(&mut tmp, count);
                    plaintext.extend_from_slice(&tmp[..n]);
                    sent_frames.push(ControlFrame::MaxChannels { count });
                } else {
                    self.channels.requeue_max_channels_update();
                }
            }

            // 7. Stream data.
            while plaintext.len() + STREAM_HEADER_SIZE < max_plaintext {
                let avail = max_plaintext - plaintext.len() - STREAM_HEADER_SIZE;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((stream_id, offset, len, fin)) = self.streams.emit(&mut data_buf) {
                    tracing::trace!(stream_id, offset, len, fin, "emit stream data");
                    let mut hdr = [0u8; STREAM_HEADER_SIZE];
                    encode_stream_header(&mut hdr, stream_id, offset, len as u16, fin);
                    plaintext.extend_from_slice(&hdr);
                    plaintext.extend_from_slice(&data_buf[..len]);
                    // Phantom FIN byte: tracking len includes +1 if FIN was set.
                    sent_streams.push((stream_id, offset, len + (fin as usize)));
                } else {
                    break;
                }
            }

            // 8. Channel data.
            while plaintext.len() + CHANNEL_HEADER_SIZE < max_plaintext {
                let avail = max_plaintext - plaintext.len() - CHANNEL_HEADER_SIZE;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((ch_id, msg_id, offset, len, fin)) =
                    self.channels.emit(&mut data_buf)
                {
                    let mut hdr = [0u8; CHANNEL_HEADER_SIZE];
                    encode_channel_header(&mut hdr, ch_id, msg_id, offset, len as u16, fin);
                    plaintext.extend_from_slice(&hdr);
                    plaintext.extend_from_slice(&data_buf[..len]);
                    sent_channels.push((ch_id, msg_id, offset, len));
                } else {
                    break;
                }
            }

            // 9. ConnectionClose.
            // Error close (non-zero error_code): send immediately.
            // Graceful close (error_code 0): wait until all data is drained and acknowledged.
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
                if plaintext.len() + needed <= max_plaintext {
                    let mut tmp = vec![0u8; needed];
                    let n = encode_connection_close(&mut tmp, error_code, &reason);
                    plaintext.extend_from_slice(&tmp[..n]);
                    sent_frames.push(ControlFrame::ConnectionClose { error_code, reason });
                } else {
                    self.pending_close = Some((error_code, reason));
                }
            }
        }

        // Nothing to send at all?
        if plaintext.is_empty() {
            return Err(Error::Done);
        }
        tracing::trace!(
            plaintext_len = plaintext.len(),
            ack_len,
            pending_window = self.pending_window_updates.len(),
            "send_data producing packet"
        );

        let ack_only = plaintext.len() <= ack_len;

        // 10. Write data packet header with current epoch.
        let counter = self.packet_counter;
        self.packet_counter += 1;
        let hdr_len = encode_data_header(buf, self.send_epoch, peer_id.0, counter);

        // 11. Encrypt plaintext. AAD = packet header.
        let aad = &buf[..hdr_len];
        let send_key = self.send_keys[self.send_epoch as usize].as_ref().unwrap();
        let ciphertext = send_key.encrypt(counter, aad, &plaintext);
        let total = hdr_len + ciphertext.len();
        buf[hdr_len..total].copy_from_slice(&ciphertext);

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

    fn recv_data(&mut self, buf: &[u8], now: Instant) -> Result<usize, Error> {
        let pkt = parse_data_packet(buf).map_err(|_| Error::InvalidPacket)?;

        // Duplicate check before decryption.
        if self.recv_ack.should_accept(pkt.counter) == RecvResult::Duplicate {
            return Ok(buf.len());
        }

        // Decrypt payload using epoch from packet header.
        let recv_key = self.recv_keys[pkt.epoch as usize]
            .as_ref()
            .ok_or(Error::CryptoError)?;
        let aad = &buf[..DATA_HEADER_SIZE];
        let plaintext = recv_key
            .decrypt(pkt.counter, aad, pkt.payload)
            .ok_or(Error::CryptoError)?;

        self.last_recv_at = Some(now);

        // If peer is sending on a newer epoch, advance our send_epoch to match.
        if pkt.epoch != self.send_epoch && self.send_keys[pkt.epoch as usize].is_some() {
            let old_epoch = self.send_epoch;
            self.send_epoch = pkt.epoch;
            self.on_epoch_change(old_epoch, pkt.epoch);
        }

        // Decode and route frames, tracking whether any are ack-eliciting.
        let mut ack_eliciting = false;
        for frame_result in decode_frames(&plaintext) {
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
            self.recv_ack.commit(pkt.counter, now);
        }

        Ok(buf.len())
    }

    fn process_frame(&mut self, frame: Frame<'_>, now: Instant) -> Result<(), Error> {
        match frame {
            Frame::Ack {
                largest,
                delay,
                ranges,
            } => {
                let ack = Ack {
                    largest_ack: largest,
                    ack_delay: delay,
                    ranges: parse_ack_ranges_from_bytes(ranges),
                };
                let (acked, loss) = self.send_ack.on_ack_received(&ack, now);
                self.handle_ack(acked);
                self.handle_loss(loss);

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
                if let Some(ref mut assembler) = self.auth_recv {
                    let _ = assembler.write(offset, data, fin);
                    if assembler.is_complete() {
                        self.verify_auth_payload()?;
                        self.try_finish_auth();
                    }
                }
            }

            Frame::AuthComplete => {
                self.auth_complete_received = true;
                self.try_finish_auth();
            }

            Frame::Stream {
                stream_id,
                offset,
                fin,
                data,
            } => {
                if self.state == State::Established || self.state == State::Closing {
                    if let Err(e) = self.streams.recv(stream_id, offset, data, fin) {
                        if matches!(e, super::stream::manager::StreamError::TooManyStreams) {
                            self.close_with_error(1, b"too many streams");
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
                    if let Err(e) = self.channels.recv(channel_id, message_id, offset, data, fin) {
                        if matches!(e, super::channel::manager::ChannelError::TooManyChannels) {
                            self.close_with_error(2, b"too many channels");
                        }
                    }
                }
            }

            Frame::WindowUpdate {
                stream_id,
                max_offset,
            } => {
                tracing::trace!(stream_id, max_offset, "recv WindowUpdate");
                self.streams.update_send_max_data(stream_id, max_offset);
            }

            Frame::MaxStreams { count } => {
                tracing::trace!(count, "recv MaxStreams");
                self.streams.update_send_max_streams(count);
            }

            Frame::MaxChannels { count } => {
                tracing::trace!(count, "recv MaxChannels");
                self.channels.update_send_max_channels(count);
            }

            Frame::ChannelOpen { channel_id } => {
                if self.state == State::Established || self.state == State::Closing {
                    if let Err(e) = self.channels.on_peer_open(channel_id) {
                        if matches!(e, super::channel::manager::ChannelError::TooManyChannels) {
                            self.close_with_error(2, b"too many channels");
                        }
                    }
                }
            }

            Frame::ChannelFin { channel_id, last_message_id } => {
                if self.state == State::Established || self.state == State::Closing {
                    self.channels.on_peer_fin(channel_id, last_message_id);
                }
            }

            Frame::Rekey { offset, fin, data } => {
                self.on_rekey_frame(offset, data, fin)?;
            }

            Frame::RekeyAck { offset, fin, data } => {
                self.on_rekey_ack_frame(offset, data, fin)?;
            }

            Frame::ConnectionClose { .. } => {
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
        self.rekey_recv = Some(MessageAssembler::new(KEM_CIPHERTEXT_SIZE as u64));
        Ok(())
    }

    /// Handle a received Rekey frame (peer is initiating rekey).
    fn on_rekey_frame(&mut self, offset: u64, data: &[u8], fin: bool) -> Result<(), Error> {
        if self.state != State::Established {
            return Ok(());
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
            self.rekey_recv = Some(MessageAssembler::new(KEM_PUBLIC_KEY_SIZE as u64));
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

        // Don't switch send_epoch yet — wait until initiator sends on new epoch.
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

        Ok(())
    }

    fn handle_ack(&mut self, acked: AckReport) {
        for (stream_id, offset, len) in &acked.streams {
            tracing::trace!(stream_id, offset, len, "ack stream data");
            self.streams.ack(*stream_id, *offset, *len);
        }
        for (channel_id, message_id, offset, len) in acked.channels {
            self.channels.ack(channel_id, message_id, offset, len);
        }
        // Control-frame acks: with credit-based MAX_CHANNELS, acks are
        // informational — credit is granted unilaterally on cleanup, not
        // as a response to peer's close.  Nothing to do here.
        let _ = acked.frames;
    }

    fn handle_loss(&mut self, loss: LossReport) {
        for (stream_id, offset, len) in loss.streams {
            self.streams.loss(stream_id, offset, len);
        }
        for (channel_id, message_id, offset, len) in loss.channels {
            self.channels.loss(channel_id, message_id, offset, len);
        }
        for frame in loss.frames {
            match frame {
                ControlFrame::Pong { id } => {
                    self.pending_pongs.push(id);
                }
                ControlFrame::WindowUpdate {
                    stream_id,
                    max_offset,
                } => {
                    self.pending_window_updates.insert(stream_id, max_offset);
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
        // Retransmit lost auth fragments.
        for (offset, len) in loss.auth {
            if let Some(ref mut frag) = self.auth_send {
                frag.loss(offset, len);
            }
        }
        // Retransmit lost rekey fragments.
        for (offset, len) in loss.rekey {
            if let Some(ref mut frag) = self.rekey_send {
                frag.loss(offset, len);
            }
        }
    }
}

/// Parse ACK ranges from raw wire bytes. Each range is 16 bytes: [gap:8][length:8].
fn parse_ack_ranges_from_bytes(data: &[u8]) -> Vec<AckRange> {
    let mut ranges = Vec::new();
    let mut pos = 0;
    while pos + 16 <= data.len() {
        let gap = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        let length = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap());
        ranges.push(AckRange { gap, length });
        pos += 16;
    }
    ranges
}
