use std::time::Instant;

use crate::crypto::PublicKey;
use crate::packet::{RecvAckState, RecvResult, SendAckState};
use crate::session::{DecryptedData, Session, SessionEvent};
use crate::stream::manager::{StreamError, StreamManager};
use crate::wire::packet::{Data, MAX_PACKET_PAYLOAD};
use crate::wire::{
    Auth, AuthComplete, ConnectionClose, Frame, Ping, Pong, RekeyAck, Writer, decode_frames,
    encode_frames,
};

const MAX_FRAME_DATA: usize = 1100;

pub struct PeerConnection {
    session: Session,
    local_session_id: u64,
    remote_session_id: u64,
    streams: StreamManager,
    send_ack: SendAckState,
    recv_ack: RecvAckState,
    outgoing: Vec<Frame>,
    established: bool,
    peer_public_key: Option<PublicKey>,
    got_connection_close: bool,
}

impl PeerConnection {
    pub fn new(
        session: Session,
        local_session_id: u64,
        remote_session_id: u64,
        is_initiator: bool,
        auth_payload: Vec<u8>,
    ) -> Self {
        let mut conn = Self {
            session,
            local_session_id,
            remote_session_id,
            streams: StreamManager::new(is_initiator),
            send_ack: SendAckState::new(),
            recv_ack: RecvAckState::new(),
            outgoing: Vec::new(),
            established: false,
            peer_public_key: None,
            got_connection_close: false,
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

    pub fn local_session_id(&self) -> u64 {
        self.local_session_id
    }

    pub fn remote_session_id(&self) -> u64 {
        self.remote_session_id
    }

    pub fn on_data_packet(&mut self, data: Data, now: Instant) {
        let recv_result = self.recv_ack.receive(data.counter, now);
        if recv_result != RecvResult::Accepted {
            return;
        }

        let Some(decrypted) = self.session.decrypt(data) else {
            return;
        };

        let Ok(frames) = decode_frames(&decrypted.payload) else {
            return;
        };

        for frame in frames {
            self.dispatch_frame(frame, now);
        }
    }

    pub fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        let mut frames = std::mem::take(&mut self.outgoing);

        if let Some(ack) = self.recv_ack.generate_ack(now) {
            frames.push(Frame::Ack(ack));
        }

        while let Some(data) = self.streams.poll_stream_data(MAX_FRAME_DATA) {
            frames.push(Frame::StreamData(data));
        }

        while let Some(frag) = self.streams.poll_datagram_fragment() {
            frames.push(Frame::DatagramFragment(frag));
        }

        if frames.is_empty() {
            return Vec::new();
        }

        self.build_packets(frames, now)
    }

    pub fn has_pending(&self) -> bool {
        !self.outgoing.is_empty()
            || self.streams.has_pending_data()
            || self.recv_ack.largest().is_some()
    }

    pub fn open_stream(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.streams.open(purpose);
        self.outgoing.push(Frame::StreamOpen(open));
        id
    }

    pub fn open_datagram(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.streams.open_datagram(purpose);
        self.outgoing.push(Frame::StreamOpen(open));
        id
    }

    pub fn write_datagram(&mut self, stream_id: u32, data: &[u8]) -> Result<(), StreamError> {
        self.streams.write_datagram(stream_id, data)
    }

    pub fn read_datagram(&mut self, stream_id: u32) -> Result<Option<Vec<u8>>, StreamError> {
        self.streams.read_datagram(stream_id)
    }

    pub fn has_pending_accept(&self) -> bool {
        self.streams.pending_accept_count() > 0
    }

    pub fn accept_stream(&mut self) -> Option<(u32, u16)> {
        self.streams.accept()
    }

    pub fn write(&mut self, stream_id: u32, data: &[u8]) -> Result<(), StreamError> {
        self.streams.write(stream_id, data)
    }

    pub fn read(&mut self, stream_id: u32) -> Result<Option<Vec<u8>>, StreamError> {
        self.streams.read(stream_id)
    }

    pub fn is_stream_finished(&self, stream_id: u32) -> bool {
        self.streams.is_stream_finished(stream_id)
    }

    pub fn in_flight_count(&self) -> usize {
        self.send_ack.in_flight_count()
    }

    pub fn close_stream(&mut self, stream_id: u32) -> Result<(), StreamError> {
        let close = self.streams.close(stream_id)?;
        self.outgoing.push(Frame::StreamClose(close));
        Ok(())
    }

    fn dispatch_frame(&mut self, frame: Frame, now: Instant) {
        match frame {
            Frame::Ack(ack) => {
                let lost = self.send_ack.on_ack_received(&ack, now);
                self.outgoing.extend(lost);
            }
            Frame::Ping(ping) => {
                self.outgoing.push(Frame::Pong(Pong {
                    ping_id: ping.ping_id,
                }));
            }
            Frame::Pong(_) => {}
            Frame::StreamOpen(open) => {
                self.streams.on_stream_open(open);
            }
            Frame::StreamData(data) => {
                self.streams.on_stream_data(data);
            }
            Frame::StreamClose(close) => {
                self.streams.on_stream_close(&close);
            }
            Frame::StreamReset(reset) => {
                self.streams.on_stream_reset(&reset);
            }
            Frame::WindowUpdate(update) => {
                self.streams.on_window_update(&update);
            }
            Frame::Auth(_) | Frame::AuthComplete(_) | Frame::Rekey(_) | Frame::RekeyAck(_) => {
                if let Some(event) = self.session.process_incoming_frame(&frame) {
                    self.handle_session_event(event);
                }
            }
            Frame::ConnectionClose(_) => {
                self.got_connection_close = true;
            }
            Frame::DatagramFragment(frag) => {
                self.streams.on_datagram_fragment(frag);
            }
            Frame::Datagram(_) => {}
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
                receiver_session_id: self.remote_session_id,
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
