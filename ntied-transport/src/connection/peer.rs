use std::time::Instant;

use crate::crypto::PublicKey;
use crate::channel::manager::{ChannelError, ChannelManager};
use crate::session::Session;

use super::core::{TransportCore, MAX_FRAME_DATA};
use crate::wire::packet::Data;
use crate::wire::Frame;

pub struct PeerConnection {
    core: TransportCore,
    channels: ChannelManager,
}

impl PeerConnection {
    pub fn new(
        session: Session,
        local_connection_id: u64,
        remote_connection_id: u64,
        is_initiator: bool,
        auth_payload: Vec<u8>,
    ) -> Self {
        Self {
            core: TransportCore::new(session, local_connection_id, remote_connection_id, auth_payload),
            channels: ChannelManager::new(is_initiator),
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

    pub fn queue_connection_close(&mut self, error_code: u32) {
        self.core.queue_connection_close(error_code);
    }

    pub fn queue_ping(&mut self, ping_id: u32) {
        self.core.queue_ping(ping_id);
    }

    pub fn queue_frame(&mut self, frame: Frame) {
        self.core.queue_frame(frame);
    }

    pub fn local_connection_id(&self) -> u64 {
        self.core.local_connection_id()
    }

    pub fn remote_connection_id(&self) -> u64 {
        self.core.remote_connection_id()
    }

    pub fn on_data_packet(&mut self, data: Data, now: Instant) -> Vec<Frame> {
        let Some(frames) = self.core.receive_packet(data, now) else {
            return Vec::new();
        };

        let mut unhandled = Vec::new();
        for frame in frames {
            match frame {
                Frame::ChannelOpen(open) => { self.channels.on_channel_open(open); }
                Frame::StreamData(data) => { self.channels.on_channel_data(data); }
                Frame::ChannelClose(close) => { self.channels.on_channel_close(&close); }
                Frame::ChannelReset(reset) => { self.channels.on_channel_reset(&reset); }
                Frame::WindowUpdate(update) => { self.channels.on_window_update(&update); }
                Frame::DatagramFragment(frag) => { self.channels.on_datagram_fragment(frag); }
                Frame::Datagram(_) => {}
                other => unhandled.push(other),
            }
        }
        unhandled
    }

    /// Drain channel data into core's pending frames, then flush all + ack.
    pub fn flush(&mut self, now: Instant) {
        self.drain_channels();
        self.core.flush(now);
    }

    /// Drain ready packets for sending.
    pub fn send_packets(&mut self) -> Vec<Data> {
        self.core.send_packets()
    }

    /// Convenience: flush + send_packets in one call.
    /// Equivalent to `self.flush(now); self.send_packets()`.
    pub fn poll_packets(&mut self, now: Instant) -> Vec<Data> {
        self.flush(now);
        self.send_packets()
    }

    pub fn has_pending(&self) -> bool {
        self.core.has_pending()
            || self.channels.has_pending_data()
    }

    fn drain_channels(&mut self) {
        while let Some(data) = self.channels.poll_channel_data(MAX_FRAME_DATA) {
            self.core.queue_frame(Frame::StreamData(data));
        }
        while let Some(frag) = self.channels.poll_datagram_fragment() {
            self.core.queue_frame(Frame::DatagramFragment(frag));
        }
    }

    pub fn open_stream(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open(purpose);
        self.core.queue_frame(Frame::ChannelOpen(open));
        id
    }

    pub fn open_datagram(&mut self, purpose: u16) -> u32 {
        let (id, open) = self.channels.open_datagram(purpose);
        self.core.queue_frame(Frame::ChannelOpen(open));
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
        self.core.in_flight_count()
    }

    pub fn close_channel(&mut self, channel_id: u32) -> Result<(), ChannelError> {
        let close = self.channels.close(channel_id)?;
        self.core.queue_frame(Frame::ChannelClose(close));
        Ok(())
    }
}
