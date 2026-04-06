use std::time::Instant;

use crate::channel::{ChannelError, ChannelManager};
use crate::crypto::{
    KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE, KemCiphertext, KemPublicKey, PUBLIC_KEY_SIZE,
    PublicKey, SIGNATURE_SIZE, Signature,
};
use crate::session::{DecryptedData, Session};

use super::ack::{RecvAckState, RecvResult, SendAckState};
use super::fragment::FragmentCollector;
use crate::wire::packet::{Data, MAX_PACKET_PAYLOAD};
use crate::wire::{
    Auth, AuthComplete, ConnectionClose, Frame, Ping, Pong, RekeyAck, Writer, decode_frames,
    encode_frames,
};

const MAX_FRAME_DATA: usize = 1100;

pub struct Connection {
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

    channels: ChannelManager,
}

impl Connection {
    pub fn new(
        session: Session,
        local_connection_id: u64,
        remote_connection_id: u64,
        is_initiator: bool,
        auth_payload: Vec<u8>,
    ) -> Self {
        let mut conn = Self {
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
            channels: ChannelManager::new(is_initiator),
        };
        conn.queue_auth_fragments(auth_payload);
        conn.urgent_pending = true;
        conn
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
        !self.pending_frames.is_empty()
            || !self.ready_packets.is_empty()
            || self.channels.has_pending_data()
    }

    pub fn has_pending_accept(&self) -> bool {
        self.channels.pending_accept_count() > 0
    }

    pub fn on_data_packet(&mut self, data: Data, now: Instant) {
        let counter = data.counter;
        if self.recv_ack.should_accept(counter) != RecvResult::Accepted {
            return;
        }

        let Some(decrypted) = self.session.decrypt(data) else {
            return;
        };
        self.recv_ack.commit(counter, now);
        let Ok(frames) = decode_frames(&decrypted.payload) else {
            return;
        };

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
                        match other {
                            Frame::ChannelOpen(open) => {
                                self.channels.on_channel_open(open);
                            }
                            Frame::StreamData(data) => {
                                self.channels.on_channel_data(data);
                            }
                            Frame::ChannelClose(close) => {
                                self.channels.on_channel_close(&close);
                            }
                            Frame::ChannelReset(reset) => {
                                self.channels.on_channel_reset(&reset);
                            }
                            Frame::WindowUpdate(update) => {
                                self.channels.on_window_update(&update);
                            }
                            Frame::DatagramFragment(frag) => {
                                self.channels.on_datagram_fragment(frag);
                            }
                            Frame::Datagram(_) => {}
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    pub fn queue_frame(&mut self, frame: Frame) {
        self.push_frame(frame);
    }

    pub fn queue_connection_close(&mut self, error_code: u32) {
        self.push_frame(Frame::ConnectionClose(ConnectionClose {
            error_code,
            reason: Vec::new(),
        }));
    }

    pub fn queue_ping(&mut self, ping_id: u32) {
        self.push_frame(Frame::Ping(Ping { ping_id }));
    }

    /// Flush all pending frames + channel data + ack into ready packets.
    pub fn flush(&mut self, now: Instant) {
        self.drain_channels();

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

    /// Convenience: flush + send_packets in one call.
    pub fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        self.flush(now);
        self.send_packets()
    }

    pub fn open_stream(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open(purpose);
        self.push_frame(Frame::ChannelOpen(open));
        id
    }

    pub fn open_datagram(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open_datagram(purpose);
        self.push_frame(Frame::ChannelOpen(open));
        id
    }

    pub fn accept_stream(&mut self) -> Option<(u32, u16)> {
        self.channels.accept_stream()
    }

    pub fn accept_datagram(&mut self) -> Option<(u32, u16)> {
        self.channels.accept_datagram()
    }

    pub fn write(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        self.channels.write(channel_id, data)
    }

    pub fn read(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        self.channels.read(channel_id)
    }

    pub fn write_datagram(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        self.channels.write_datagram(channel_id, data)
    }

    pub fn read_datagram(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        self.channels.read_datagram(channel_id)
    }

    pub fn is_channel_finished(&self, channel_id: u32) -> bool {
        self.channels.is_channel_finished(channel_id)
    }

    pub fn close_channel(&mut self, channel_id: u32) -> Result<(), ChannelError> {
        let close = self.channels.close(channel_id)?;
        self.push_frame(Frame::ChannelClose(close));
        Ok(())
    }

    fn drain_channels(&mut self) {
        for update in self.channels.poll_window_updates() {
            self.push_frame(Frame::WindowUpdate(update));
        }
        while let Some(data) = self.channels.poll_channel_data(MAX_FRAME_DATA) {
            self.push_frame(Frame::StreamData(data));
        }
        while let Some(frag) = self.channels.poll_datagram_fragment() {
            self.push_frame(Frame::DatagramFragment(frag));
        }
    }

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
