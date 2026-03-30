use std::time::Instant;

use crate::crypto::PublicKey;
use crate::channel::manager::{ChannelError, ChannelManager};
use crate::session::{DecryptedData, Session, SessionEvent};

use super::ack::{RecvAckState, RecvResult, SendAckState};
use super::fragment::FragmentCollector;
use crate::wire::packet::{Data, MAX_PACKET_PAYLOAD};
use crate::wire::{
    Auth, AuthComplete, ConnectionClose, Frame, Ping, Pong, RekeyAck, Writer, decode_frames,
    encode_frames,
};

const MAX_FRAME_DATA: usize = 1100;

pub struct PeerConnection {
    session: Session,
    local_connection_id: u64,
    remote_connection_id: u64,
    channels: ChannelManager,
    send_ack: SendAckState,
    recv_ack: RecvAckState,
    outgoing: Vec<Frame>,
    established: bool,
    peer_public_key: Option<PublicKey>,
    got_connection_close: bool,
    auth_collector: FragmentCollector,
    rekey_collector: FragmentCollector,
    rekey_ack_collector: FragmentCollector,
}

impl PeerConnection {
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
            channels: ChannelManager::new(is_initiator),
            send_ack: SendAckState::new(),
            recv_ack: RecvAckState::new(),
            outgoing: Vec::new(),
            established: false,
            peer_public_key: None,
            got_connection_close: false,
            auth_collector: FragmentCollector::new(),
            rekey_collector: FragmentCollector::new(),
            rekey_ack_collector: FragmentCollector::new(),
        };
        conn.queue_auth_fragments(auth_payload);
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

    pub fn queue_connection_close(&mut self, error_code: u32) {
        self.outgoing.push(Frame::ConnectionClose(ConnectionClose {
            error_code,
            reason: Vec::new(),
        }));
    }

    pub fn queue_ping(&mut self, ping_id: u32) {
        self.outgoing.push(Frame::Ping(Ping { ping_id }));
    }

    pub fn queue_frame(&mut self, frame: Frame) {
        self.outgoing.push(frame);
    }

    pub fn local_connection_id(&self) -> u64 {
        self.local_connection_id
    }

    pub fn remote_connection_id(&self) -> u64 {
        self.remote_connection_id
    }

    pub fn on_data_packet(&mut self, data: Data, now: Instant) -> Vec<Frame> {
        let recv_result = self.recv_ack.receive(data.counter, now);
        if recv_result != RecvResult::Accepted {
            return Vec::new();
        }

        let Some(decrypted) = self.session.decrypt(data) else {
            return Vec::new();
        };

        let Ok(frames) = decode_frames(&decrypted.payload) else {
            return Vec::new();
        };

        let mut unhandled = Vec::new();
        for frame in frames {
            if Self::is_connection_frame(&frame) {
                self.dispatch_frame(frame, now);
            } else {
                unhandled.push(frame);
            }
        }
        unhandled
    }

    fn is_connection_frame(frame: &Frame) -> bool {
        matches!(
            frame,
            Frame::Ack(_)
                | Frame::Ping(_)
                | Frame::Pong(_)
                | Frame::ChannelOpen(_)
                | Frame::StreamData(_)
                | Frame::ChannelClose(_)
                | Frame::ChannelReset(_)
                | Frame::WindowUpdate(_)
                | Frame::DatagramFragment(_)
                | Frame::Datagram(_)
                | Frame::Auth(_)
                | Frame::AuthComplete(_)
                | Frame::Rekey(_)
                | Frame::RekeyAck(_)
                | Frame::ConnectionClose(_)
        )
    }

    pub fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        let mut frames = std::mem::take(&mut self.outgoing);

        if let Some(ack) = self.recv_ack.generate_ack(now) {
            frames.push(Frame::Ack(ack));
        }

        while let Some(data) = self.channels.poll_channel_data(MAX_FRAME_DATA) {
            frames.push(Frame::StreamData(data));
        }

        while let Some(frag) = self.channels.poll_datagram_fragment() {
            frames.push(Frame::DatagramFragment(frag));
        }

        if frames.is_empty() {
            return Vec::new();
        }

        self.build_packets(frames, now)
    }

    pub fn has_pending(&self) -> bool {
        !self.outgoing.is_empty()
            || self.channels.has_pending_data()
            || self.recv_ack.largest().is_some()
    }

    pub fn open_stream(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open(purpose);
        self.outgoing.push(Frame::ChannelOpen(open));
        id
    }

    pub fn open_datagram(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open_datagram(purpose);
        self.outgoing.push(Frame::ChannelOpen(open));
        id
    }

    pub fn write_datagram(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        self.channels.write_datagram(channel_id, data)
    }

    pub fn read_datagram(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        self.channels.read_datagram(channel_id)
    }

    pub fn has_pending_accept(&self) -> bool {
        self.channels.pending_accept_count() > 0
    }

    pub fn accept_stream(&mut self) -> Option<(u32, u16)> {
        self.channels.accept()
    }

    pub fn write(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        self.channels.write(channel_id, data)
    }

    pub fn read(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        self.channels.read(channel_id)
    }

    pub fn is_channel_finished(&self, channel_id: u32) -> bool {
        self.channels.is_channel_finished(channel_id)
    }

    pub fn in_flight_count(&self) -> usize {
        self.send_ack.in_flight_count()
    }

    pub fn close_channel(&mut self, channel_id: u32) -> Result<(), ChannelError> {
        let close = self.channels.close(channel_id)?;
        self.outgoing.push(Frame::ChannelClose(close));
        Ok(())
    }

    fn dispatch_frame(&mut self, frame: Frame, _now: Instant) {
        match frame {
            Frame::Ack(ack) => {
                let lost = self.send_ack.on_ack_received(&ack, _now);
                self.outgoing.extend(lost);
            }
            Frame::Ping(ping) => {
                self.outgoing.push(Frame::Pong(Pong {
                    ping_id: ping.ping_id,
                }));
            }
            Frame::Pong(_) => {}
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
            Frame::Auth(f) => {
                if let Some(payload) =
                    self.auth_collector
                        .add_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    if let Some(event) = self.session.on_auth_data(&payload) {
                        self.handle_session_event(event);
                    }
                }
            }
            Frame::AuthComplete(_) => {}
            Frame::Rekey(f) => {
                if let Some(payload) =
                    self.rekey_collector
                        .add_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    if let Some(event) = self.session.on_rekey_data(&payload) {
                        self.handle_session_event(event);
                    }
                }
            }
            Frame::RekeyAck(f) => {
                if let Some(payload) =
                    self.rekey_ack_collector
                        .add_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    if let Some(event) = self.session.on_rekey_ack_data(&payload) {
                        self.handle_session_event(event);
                    }
                }
            }
            Frame::ConnectionClose(_) => {
                self.got_connection_close = true;
            }
            Frame::DatagramFragment(frag) => {
                self.channels.on_datagram_fragment(frag);
            }
            Frame::Datagram(_) => {}
            _ => {}
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::AuthCompleted(pk) => {
                self.peer_public_key = Some(pk);
                self.established = true;
                self.outgoing.push(Frame::AuthComplete(AuthComplete));
            }
            SessionEvent::SendRekeyAck(ct_bytes) => {
                let fragments = fragment_payload(&ct_bytes, MAX_FRAME_DATA);
                let total = fragments.len() as u8;
                for (i, data) in fragments.into_iter().enumerate() {
                    self.outgoing.push(Frame::RekeyAck(RekeyAck {
                        fragment_index: i as u8,
                        fragment_total: total,
                        data,
                    }));
                }
            }
            SessionEvent::KeysRotated => {}
        }
    }

    fn build_packets(&mut self, frames: Vec<Frame>, now: Instant) -> Vec<Data> {
        let batches = pack_into_batches(frames, MAX_PACKET_PAYLOAD);
        let mut packets = Vec::new();

        for batch in batches {
            let payload = encode_frames(&batch);
            let data = self.session.encrypt(DecryptedData {
                receiver_connection_id: self.remote_connection_id,
                payload,
            });
            self.send_ack.on_packet_sent(data.counter, batch, now);
            packets.push(data);
        }

        packets
    }

    fn queue_auth_fragments(&mut self, payload: Vec<u8>) {
        let fragments = fragment_payload(&payload, MAX_FRAME_DATA);
        let total = fragments.len() as u8;
        for (i, data) in fragments.into_iter().enumerate() {
            self.outgoing.push(Frame::Auth(Auth {
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
