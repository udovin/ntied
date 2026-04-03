use std::collections::VecDeque;
use std::time::Instant;

use crate::crypto::PublicKey;
use crate::session::Session;

use super::core::TransportCore;
use crate::wire::packet::Data;
use crate::wire::Frame;

pub struct SystemConnection {
    core: TransportCore,
    incoming_frames: VecDeque<Frame>,
}

impl SystemConnection {
    pub fn new(
        session: Session,
        local_connection_id: u64,
        remote_connection_id: u64,
        auth_payload: Vec<u8>,
    ) -> Self {
        Self {
            core: TransportCore::new(
                session,
                local_connection_id,
                remote_connection_id,
                auth_payload,
            ),
            incoming_frames: VecDeque::new(),
        }
    }

    pub fn is_established(&self) -> bool {
        self.core.is_established()
    }

    pub fn peer_public_key(&self) -> Option<&PublicKey> {
        self.core.peer_public_key()
    }

    pub fn got_connection_close(&self) -> bool {
        self.core.got_connection_close()
    }

    pub fn local_connection_id(&self) -> u64 {
        self.core.local_connection_id()
    }

    pub fn remote_connection_id(&self) -> u64 {
        self.core.remote_connection_id()
    }

    pub fn queue_connection_close(&mut self, error_code: u32) {
        self.core.queue_connection_close(error_code);
    }

    pub fn queue_ping(&mut self, ping_id: u32) {
        self.core.queue_ping(ping_id);
    }

    pub fn in_flight_count(&self) -> usize {
        self.core.in_flight_count()
    }

    pub fn has_pending(&self) -> bool {
        self.core.has_pending()
    }

    // ── Receive path ──

    pub fn on_data_packet(&mut self, data: Data, now: Instant) {
        let Some(frames) = self.core.receive_packet(data, now) else {
            return;
        };
        for frame in frames {
            self.incoming_frames.push_back(frame);
        }
    }

    /// Take next overlay frame received from the peer.
    pub fn recv_frame(&mut self) -> Option<Frame> {
        self.incoming_frames.pop_front()
    }

    /// Drain all pending overlay frames.
    pub fn recv_frames(&mut self) -> Vec<Frame> {
        self.incoming_frames.drain(..).collect()
    }

    // ── Send path ──

    /// Queue an overlay frame to send to the peer.
    pub fn send_frame(&mut self, frame: Frame) {
        self.core.queue_frame(frame);
    }

    pub fn flush(&mut self, now: Instant) {
        self.core.flush(now);
    }

    pub fn send_packets(&mut self) -> Vec<Data> {
        self.core.send_packets()
    }

    /// Convenience: flush + send_packets in one call.
    pub fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        self.flush(now);
        self.send_packets()
    }
}
