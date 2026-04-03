use std::time::Instant;

use crate::crypto::{
    KemCiphertext, KemPublicKey, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE, PUBLIC_KEY_SIZE,
    PublicKey, SIGNATURE_SIZE, Signature,
};
use crate::session::{DecryptedData, Session};

use super::ack::{RecvAckState, RecvResult, SendAckState};
use super::fragment::FragmentCollector;
use crate::wire::packet::{Data, MAX_PACKET_PAYLOAD};
use crate::wire::{
    Auth, AuthComplete, Frame, Pong, RekeyAck, Writer, decode_frames, encode_frames,
};

pub(crate) const MAX_FRAME_DATA: usize = 1100;

pub struct TransportCore {
    session: Session,
    local_connection_id: u64,
    remote_connection_id: u64,
    send_ack: SendAckState,
    recv_ack: RecvAckState,
    pending_frames: Vec<Frame>,
    pending_size: usize,
    ready_packets: Vec<Data>,
    urgent_pending: bool,
    established: bool,
    peer_public_key: Option<PublicKey>,
    got_connection_close: bool,
    auth_collector: FragmentCollector,
    rekey_collector: FragmentCollector,
    rekey_ack_collector: FragmentCollector,
}

impl TransportCore {
    pub fn new(
        session: Session,
        local_connection_id: u64,
        remote_connection_id: u64,
        auth_payload: Vec<u8>,
    ) -> Self {
        let mut core = Self {
            session,
            local_connection_id,
            remote_connection_id,
            send_ack: SendAckState::new(),
            recv_ack: RecvAckState::new(),
            pending_frames: Vec::new(),
            pending_size: 0,
            ready_packets: Vec::new(),
            urgent_pending: false,
            established: false,
            peer_public_key: None,
            got_connection_close: false,
            auth_collector: FragmentCollector::new(),
            rekey_collector: FragmentCollector::new(),
            rekey_ack_collector: FragmentCollector::new(),
        };
        core.queue_auth_fragments(auth_payload);
        core.urgent_pending = true;
        core
    }

    pub fn is_established(&self) -> bool {
        self.established
    }

    pub fn peer_public_key(&self) -> Option<&PublicKey> {
        self.peer_public_key.as_ref()
    }

    pub fn got_connection_close(&self) -> bool {
        self.got_connection_close
    }

    pub fn local_connection_id(&self) -> u64 {
        self.local_connection_id
    }

    pub fn remote_connection_id(&self) -> u64 {
        self.remote_connection_id
    }

    pub fn in_flight_count(&self) -> usize {
        self.send_ack.in_flight_count()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending_frames.is_empty() || !self.ready_packets.is_empty()
    }

    // ── Receive path ──

    /// Decrypt a data packet, process transport-level frames internally,
    /// and return remaining frames for the caller to handle.
    /// Urgent responses (auth, retransmit) are built into ready_packets immediately.
    /// Returns None if the packet was rejected (duplicate or decryption failure).
    pub fn receive_packet(&mut self, data: Data, now: Instant) -> Option<Vec<Frame>> {
        let counter = data.counter;
        if self.recv_ack.should_accept(counter) != RecvResult::Accepted {
            return None;
        }

        let decrypted = self.session.decrypt(data)?;
        self.recv_ack.commit(counter, now);
        let frames = decode_frames(&decrypted.payload).ok()?;

        let mut passthrough = Vec::new();
        for frame in frames {
            match frame {
                Frame::Ack(ack) => {
                    let lost = self.send_ack.on_ack_received(&ack, now);
                    if !lost.is_empty() {
                        self.urgent_pending = true;
                        for f in lost {
                            self.push_frame(f);
                        }
                    }
                }
                Frame::Ping(ping) => {
                    self.push_frame(Frame::Pong(Pong {
                        ping_id: ping.ping_id,
                    }));
                }
                Frame::Pong(_) => {}
                Frame::Auth(f) => {
                    if let Some(payload) = self.auth_collector.add_fragment(
                        f.fragment_index,
                        f.fragment_total,
                        &f.data,
                    ) {
                        if let Some((pk, sig)) = parse_auth_payload(&payload) {
                            if self.session.on_auth_data(&pk, &sig) {
                                self.peer_public_key = Some(pk);
                                self.established = true;
                                self.urgent_pending = true;
                                self.push_frame(Frame::AuthComplete(AuthComplete));
                            }
                        }
                    }
                }
                Frame::AuthComplete(_) => {}
                Frame::Rekey(f) => {
                    if let Some(payload) = self.rekey_collector.add_fragment(
                        f.fragment_index,
                        f.fragment_total,
                        &f.data,
                    ) {
                        if let Some(peer_pk) = parse_kem_public_key(&payload) {
                            if let Some(ct) = self.session.on_rekey_data(&peer_pk) {
                                self.urgent_pending = true;
                                self.send_rekey_ack(ct);
                            }
                        }
                    }
                }
                Frame::RekeyAck(f) => {
                    if let Some(payload) = self.rekey_ack_collector.add_fragment(
                        f.fragment_index,
                        f.fragment_total,
                        &f.data,
                    ) {
                        if let Some(ct) = parse_kem_ciphertext(&payload) {
                            self.session.on_rekey_ack_data(&ct);
                        }
                    }
                }
                Frame::ConnectionClose(_) => {
                    self.got_connection_close = true;
                }
                other => {
                    if self.established {
                        passthrough.push(other);
                    }
                }
            }
        }

        Some(passthrough)
    }

    // ── Send path ──

    /// Queue a frame to be sent. If pending frames fill a packet, build it immediately.
    pub fn queue_frame(&mut self, frame: Frame) {
        self.push_frame(frame);
    }

    /// Queue a connection close frame.
    pub fn queue_connection_close(&mut self, error_code: u32) {
        self.push_frame(Frame::ConnectionClose(crate::wire::ConnectionClose {
            error_code,
            reason: Vec::new(),
        }));
    }

    /// Queue a ping frame.
    pub fn queue_ping(&mut self, ping_id: u32) {
        self.push_frame(Frame::Ping(crate::wire::Ping { ping_id }));
    }

    /// Flush all pending frames + generate ack into ready packets.
    /// Call periodically on tick.
    pub fn flush(&mut self, now: Instant) {
        if let Some(ack) = self.recv_ack.generate_ack(now) {
            self.pending_frames.push(Frame::Ack(ack));
        }

        if self.pending_frames.is_empty() {
            return;
        }

        self.flush_pending(now);
    }

    /// Drain all ready packets for sending.
    /// If urgent frames are pending, flushes them first.
    pub fn send_packets(&mut self) -> Vec<Data> {
        if self.urgent_pending {
            self.flush_pending(Instant::now());
            self.urgent_pending = false;
        }
        std::mem::take(&mut self.ready_packets)
    }

    // ── Internal ──

    fn push_frame(&mut self, frame: Frame) {
        let size = encoded_frame_size(&frame);
        self.pending_frames.push(frame);
        self.pending_size += size;

        if self.pending_size >= MAX_PACKET_PAYLOAD {
            let now = Instant::now();
            self.flush_pending(now);
        }
    }

    fn flush_pending(&mut self, now: Instant) {
        let frames = std::mem::take(&mut self.pending_frames);
        self.pending_size = 0;

        let batches = pack_into_batches(frames, MAX_PACKET_PAYLOAD);
        for batch in batches {
            let payload = encode_frames(&batch);
            let data = self.session.encrypt(DecryptedData {
                receiver_connection_id: self.remote_connection_id,
                payload,
            });
            self.send_ack.on_packet_sent(data.counter, batch, now);
            self.ready_packets.push(data);
        }
    }

    fn send_rekey_ack(&mut self, ciphertext: KemCiphertext) {
        let ct_bytes = ciphertext.to_bytes().to_vec();
        let fragments = fragment_payload(&ct_bytes, MAX_FRAME_DATA);
        let total = fragments.len() as u8;
        for (i, data) in fragments.into_iter().enumerate() {
            self.push_frame(Frame::RekeyAck(RekeyAck {
                fragment_index: i as u8,
                fragment_total: total,
                data,
            }));
        }
    }

    fn queue_auth_fragments(&mut self, payload: Vec<u8>) {
        let fragments = fragment_payload(&payload, MAX_FRAME_DATA);
        let total = fragments.len() as u8;
        for (i, data) in fragments.into_iter().enumerate() {
            self.push_frame(Frame::Auth(Auth {
                fragment_index: i as u8,
                fragment_total: total,
                data,
            }));
        }
    }
}

fn fragment_payload(payload: &[u8], max_fragment: usize) -> Vec<Vec<u8>> {
    if payload.is_empty() {
        return vec![vec![]];
    }
    payload.chunks(max_fragment).map(|c| c.to_vec()).collect()
}

fn pack_into_batches(frames: Vec<Frame>, max_payload: usize) -> Vec<Vec<Frame>> {
    let mut batches: Vec<Vec<Frame>> = Vec::new();
    let mut current: Vec<Frame> = Vec::new();
    let mut current_size: usize = 0;

    for frame in frames {
        let size = encoded_frame_size(&frame);
        if !current.is_empty() && current_size + size > max_payload {
            batches.push(std::mem::take(&mut current));
            current_size = 0;
        }
        current_size += size;
        current.push(frame);
    }

    if !current.is_empty() {
        batches.push(current);
    }

    batches
}

fn encoded_frame_size(frame: &Frame) -> usize {
    let mut w = Writer::new();
    frame.encode(&mut w);
    w.len()
}

fn parse_auth_payload(payload: &[u8]) -> Option<(PublicKey, Signature)> {
    if payload.len() != PUBLIC_KEY_SIZE + SIGNATURE_SIZE {
        return None;
    }
    let pk = PublicKey::from_bytes(payload[..PUBLIC_KEY_SIZE].try_into().ok()?)?;
    let sig = Signature::from_bytes(payload[PUBLIC_KEY_SIZE..].try_into().ok()?)?;
    Some((pk, sig))
}

fn parse_kem_public_key(payload: &[u8]) -> Option<KemPublicKey> {
    if payload.len() != KEM_PUBLIC_KEY_SIZE {
        return None;
    }
    let bytes: &[u8; KEM_PUBLIC_KEY_SIZE] = payload.try_into().ok()?;
    Some(KemPublicKey::from_bytes(bytes))
}

fn parse_kem_ciphertext(payload: &[u8]) -> Option<KemCiphertext> {
    if payload.len() != KEM_CIPHERTEXT_SIZE {
        return None;
    }
    let bytes: &[u8; KEM_CIPHERTEXT_SIZE] = payload.try_into().ok()?;
    Some(KemCiphertext::from_bytes(bytes))
}
