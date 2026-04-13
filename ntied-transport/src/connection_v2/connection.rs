use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::crypto::{
    AEAD_TAG_SIZE, EncryptionKey, EncryptionKeys, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE,
    KemCiphertext, KemPrivateKey, KemPublicKey, PUBLIC_KEY_SIZE, PrivateKey, PublicKey,
    SIGNATURE_SIZE, Signature, compute_transcript_hash,
};

use super::ack::{Ack, AckRange, ControlFrame, LossReport, RecvAckState, RecvResult, SendAckState};
use super::channel::manager::ChannelManager;
use super::channel::message::{MessageAssembler, MessageFragmenter};
use super::stream::manager::StreamManager;
use super::wire::frame::{
    AUTH_HEADER_SIZE, CHANNEL_HEADER_SIZE, Frame, REKEY_ACK_HEADER_SIZE, REKEY_HEADER_SIZE,
    STREAM_HEADER_SIZE, decode_frames, encode_ack, encode_auth_complete, encode_auth_header,
    encode_channel_close, encode_channel_header, encode_connection_close, encode_ping, encode_pong,
    encode_rekey_ack_header, encode_rekey_header, encode_stream_header, encode_window_update,
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

const DEFAULT_STREAM_BUF: usize = 65536;
const DEFAULT_CHANNEL_BUF: usize = 65536;
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Connection {
    state: State,
    is_initiator: bool,
    connection_id: ConnectionId,
    peer_connection_id: Option<ConnectionId>,

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
    pending_channel_closes: Vec<u64>,
    pending_close: Option<(u32, Vec<u8>)>,
    pending_auth_complete: bool,
}

impl Connection {
    /// Create an initiator-side connection.
    ///
    /// Generates an ephemeral KEM keypair. The first `send()` will produce
    /// an Init packet carrying the public key.
    pub fn open(local_id: ConnectionId, identity: PrivateKey) -> Self {
        let kem = KemPrivateKey::generate();
        let mut conn = Self::new(local_id, None, true, State::Init, identity);
        conn.kem_private = Some(kem);
        conn
    }

    /// Create a responder-side connection.
    ///
    /// Called by the Node after receiving an Init packet.
    /// `peer_kem_pk` is the ephemeral public key from the Init packet.
    pub fn accept(
        local_id: ConnectionId,
        peer_id: ConnectionId,
        peer_kem_pk: KemPublicKey,
        identity: PrivateKey,
    ) -> Self {
        let mut conn = Self::new(local_id, Some(peer_id), false, State::SendInitAck, identity);
        conn.kem_peer_pk = Some(peer_kem_pk);
        conn
    }

    fn new(
        local_id: ConnectionId,
        peer_id: Option<ConnectionId>,
        is_initiator: bool,
        state: State,
        identity: PrivateKey,
    ) -> Self {
        Self {
            state,
            is_initiator,
            connection_id: local_id,
            peer_connection_id: peer_id,
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
            streams: StreamManager::new(DEFAULT_STREAM_BUF, if is_initiator { 0 } else { 1 }),
            channels: ChannelManager::new(DEFAULT_CHANNEL_BUF),
            send_ack: SendAckState::new(),
            recv_ack: RecvAckState::new(),
            packet_counter: 0,
            next_ping_id: 0,
            pings_to_send: Vec::new(),
            pings_in_flight: HashMap::new(),
            ping_rtt: None,
            created_at: Instant::now(),
            last_recv_at: None,
            last_send_at: None,
            loss_detection_pending: false,
            ack_floor_by_counter: HashMap::new(),
            pending_pongs: Vec::new(),
            pending_window_updates: HashMap::new(),
            pending_channel_closes: Vec::new(),
            pending_close: None,
            pending_auth_complete: false,
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

    pub fn readable_streams(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams.readable()
    }

    pub fn writable_streams(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams.writable()
    }

    pub fn stream_read(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<(usize, bool), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.streams.read(stream_id, buf).map_err(|_| Error::Done)
    }

    pub fn stream_write(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.streams
            .write(stream_id, data, fin)
            .map_err(|_| Error::Done)
    }

    // -- Channel API ---------------------------------------------------------

    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels.readable_channels()
    }

    pub fn channel_send(
        &mut self,
        channel_id: u64,
        data: Vec<u8>,
        deadline: Instant,
    ) -> Result<u64, Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.channels
            .send(channel_id, data, deadline)
            .map_err(|_| Error::Done)
    }

    pub fn channel_recv(&mut self, channel_id: u64) -> Result<Vec<u8>, Error> {
        self.channels.poll(channel_id).ok_or(Error::Done)
    }

    pub fn channel_close(&mut self, channel_id: u64) -> Result<(), Error> {
        if self.state != State::Established {
            return Err(Error::InvalidState);
        }
        self.channels.close(channel_id);
        self.pending_channel_closes.push(channel_id);
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

    /// Queue a ping to measure latency.  The pong will update `ping_rtt()`.
    pub fn ping(&mut self, now: Instant) {
        let id = self.next_ping_id;
        self.next_ping_id = self.next_ping_id.wrapping_add(1);
        self.pings_to_send.push((id, now));
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
            let deadline = self.created_at + HANDSHAKE_TIMEOUT;
            earliest = Some(deadline);
        }

        // Idle timeout (Established/Closing).
        if self.state == State::Established || self.state == State::Closing {
            if let Some(last) = self.last_recv_at {
                let deadline = last + IDLE_TIMEOUT;
                earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
            }
        }

        // Loss detection timeout.
        if self.send_ack.in_flight_count() > 0 {
            if let Some(last_send) = self.last_send_at {
                let deadline = last_send + self.send_ack.loss_timeout();
                earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
            }
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
            if now.duration_since(self.created_at) >= HANDSHAKE_TIMEOUT {
                self.state = State::Closed;
                return;
            }
        }

        // Idle timeout.
        if self.state == State::Established || self.state == State::Closing {
            if let Some(last) = self.last_recv_at {
                if now.duration_since(last) >= IDLE_TIMEOUT {
                    self.state = State::Closed;
                    return;
                }
            }
        }

        // Loss detection: detect_losses is handled inside on_ack_received already,
        // but timeout-based loss (no ACK at all) needs explicit triggering.
        // We flag it so the next send() can probe.
        if self.send_ack.in_flight_count() > 0 {
            if let Some(last_send) = self.last_send_at {
                if now.duration_since(last_send) >= self.send_ack.loss_timeout() {
                    self.loss_detection_pending = true;
                }
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
        if self.send_keys[self.send_epoch as usize].is_none() {
            return Err(Error::InvalidState);
        }

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
            // 5. Window updates.
            for (stream_id, max_offset) in self.streams.window_updates() {
                self.pending_window_updates.insert(stream_id, max_offset);
            }
            for (stream_id, max_offset) in self.pending_window_updates.drain().collect::<Vec<_>>() {
                if plaintext.len() + 17 > max_plaintext {
                    break;
                }
                let mut tmp = [0u8; 17];
                let n = encode_window_update(&mut tmp, stream_id, max_offset);
                plaintext.extend_from_slice(&tmp[..n]);
                sent_frames.push(ControlFrame::WindowUpdate {
                    stream_id,
                    max_offset,
                });
            }

            // 6. Channel closes.
            for channel_id in self.pending_channel_closes.drain(..).collect::<Vec<_>>() {
                if plaintext.len() + 9 > max_plaintext {
                    break;
                }
                let mut tmp = [0u8; 9];
                let n = encode_channel_close(&mut tmp, channel_id);
                plaintext.extend_from_slice(&tmp[..n]);
                sent_frames.push(ControlFrame::ChannelClose { channel_id });
            }

            // 7. ConnectionClose.
            if let Some((error_code, ref reason)) = self.pending_close {
                let needed = 7 + reason.len();
                if plaintext.len() + needed <= max_plaintext {
                    let reason = reason.clone();
                    let mut tmp = vec![0u8; needed];
                    let n = encode_connection_close(&mut tmp, error_code, &reason);
                    plaintext.extend_from_slice(&tmp[..n]);
                    sent_frames.push(ControlFrame::ConnectionClose { error_code, reason });
                    self.pending_close = None;
                }
            }

            // 8. Stream data.
            while plaintext.len() + STREAM_HEADER_SIZE < max_plaintext {
                let avail = max_plaintext - plaintext.len() - STREAM_HEADER_SIZE;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((stream_id, offset, len, fin)) = self.streams.emit(&mut data_buf) {
                    let mut hdr = [0u8; STREAM_HEADER_SIZE];
                    encode_stream_header(&mut hdr, stream_id, offset, len as u16, fin);
                    plaintext.extend_from_slice(&hdr);
                    plaintext.extend_from_slice(&data_buf[..len]);
                    sent_streams.push((stream_id, offset, len));
                } else {
                    break;
                }
            }

            // 9. Channel data.
            while plaintext.len() + CHANNEL_HEADER_SIZE < max_plaintext {
                let avail = max_plaintext - plaintext.len() - CHANNEL_HEADER_SIZE;
                if avail == 0 {
                    break;
                }
                let mut data_buf = vec![0u8; avail];
                if let Some((ch_id, msg_id, offset, len, fin)) =
                    self.channels.emit(&mut data_buf, now)
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
        }

        // Nothing to send at all?
        if plaintext.is_empty() {
            return Err(Error::Done);
        }

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
                let loss = self.send_ack.on_ack_received(&ack, now);
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
                self.pending_pongs.push(id);
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
                    let _ = self.streams.recv(stream_id, offset, data, fin);
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
                    let _ = self
                        .channels
                        .recv(channel_id, message_id, offset, data, fin);
                }
            }

            Frame::WindowUpdate {
                stream_id,
                max_offset,
            } => {
                self.streams.update_send_max_data(stream_id, max_offset);
            }

            Frame::ChannelClose { channel_id } => {
                self.channels.close(channel_id);
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
                ControlFrame::ChannelClose { channel_id } => {
                    self.pending_channel_closes.push(channel_id);
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
