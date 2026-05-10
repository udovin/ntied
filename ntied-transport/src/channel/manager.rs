use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeBounds;

use super::message::{AssemblerError, MessageAssembler, MessageFragmenter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    IdReused,
    UnknownChannel,
    TooManyChannels,
    AssemblerError(AssemblerError),
}

impl From<AssemblerError> for ChannelError {
    fn from(err: AssemblerError) -> Self {
        ChannelError::AssemblerError(err)
    }
}

pub(super) struct Channel {
    pub(super) send: BTreeMap<u64, MessageFragmenter>,
    pub(super) recv: BTreeMap<u64, MessageAssembler>,
    next_message_id: u64,
    completed: BTreeSet<u64>,
    send_buf_size: usize,
    recv_buf_size: usize,
    max_buf_size: usize,
    /// First message_id we will NOT send (boundary).  Set by `close_send()`.
    /// Send side is "finished" when this is set AND no in-flight sends remain.
    send_fin: Option<u64>,
    /// First message_id peer will NOT send.  Set by `on_peer_fin()`.
    /// Recv side is "finished" when this is set AND `recv` and `completed` are empty.
    recv_fin: Option<u64>,
}

impl Channel {
    fn new(max_buf_size: usize) -> Self {
        Self {
            send: BTreeMap::new(),
            recv: BTreeMap::new(),
            next_message_id: 0,
            completed: BTreeSet::new(),
            send_buf_size: 0,
            recv_buf_size: 0,
            max_buf_size,
            send_fin: None,
            recv_fin: None,
        }
    }

    fn send_finished(&self) -> bool {
        self.send_fin.is_some() && self.send.is_empty()
    }

    fn recv_finished(&self) -> bool {
        self.recv_fin.is_some() && self.recv.is_empty() && self.completed.is_empty()
    }

    fn evict_recv(&mut self, needed: usize) {
        while self.recv_buf_size + needed > self.max_buf_size {
            let (&oldest, _) = self.recv.first_key_value().unwrap();
            let asm = self.recv.remove(&oldest).unwrap();
            self.recv_buf_size -= asm.allocated();
            if asm.is_complete() {
                self.completed.remove(&oldest);
            }
        }
    }

    fn evict_send(&mut self, needed: usize) {
        while self.send_buf_size + needed > self.max_buf_size {
            let Some((&oldest, _)) = self.send.first_key_value() else {
                break;
            };
            let entry = self.send.remove(&oldest).unwrap();
            self.send_buf_size -= entry.len() as usize;
        }
    }
}

/// Manages message-oriented channels with semi-reliable delivery.
///
/// Each channel can have multiple messages in-flight simultaneously.
/// Messages are fragmented for transmission and reassembled on receive.
///
/// Local channels (we create) use even or odd IDs depending on role.
/// Peer channels (they create) use the opposite parity and are implicitly
/// opened with gap-fill (all peer IDs up to the received one are created).
///
/// When creating a local channel for the first time, a `ChannelOpen` frame
/// is queued for reliable delivery. If the peer receives data before the
/// `ChannelOpen`, the channel is already created and the frame is ignored.
///
/// # Half-close lifecycle
///
/// Channels close per direction via `close_send()` → `ChannelFin` frame.
/// The channel is removed when **both** sides have signalled fin and drained
/// their respective in-flight state — same model as streams.
///
/// # Channel-count flow control
///
/// QUIC-style cumulative MAX_CHANNELS credit, mirror of MAX_STREAMS for
/// streams.  Each side advertises a permitted cumulative open count; the
/// other side refuses to open beyond it.  Receiver advances its credit by
/// 1 each time `try_cleanup` removes a peer channel (frees memory).
pub struct ChannelManager {
    pub(super) channels: BTreeMap<u64, Channel>,
    /// Next ID to allocate for locally-created channels.
    local_next_id: u64,
    /// Next ID to allocate for peer-created channels.
    peer_next_id: u64,
    /// Local channel ID base (parity).
    local_base: u64,
    /// Peer channel ID base (0 for even, 1 for odd).
    peer_base: u64,
    max_buf_size: usize,
    /// Channel IDs whose state changed since last drain.
    updated: BTreeSet<u64>,
    /// Initial per-direction channel count cap (also threshold for updates).
    max_channels: usize,
    /// Cumulative count of local channels peer has permitted us to open.
    peer_max_channels: u64,
    /// Cumulative count of peer channels we permit.  Increases on cleanup.
    advertised_max_channels: u64,
    /// Last `advertised_max_channels` value we successfully sent.
    sent_max_channels: u64,
    /// Force re-send after loss.
    force_max_channels_update: bool,
    /// Pending ChannelOpen frames to send (reliable).
    pending_opens: Vec<u64>,
    /// Pending ChannelFin frames: (channel_id, last_message_id).
    pending_fins: Vec<(u64, u64)>,
    /// Round-robin cursor: channel ID to start the next emit search from.
    send_cursor: u64,
}

impl ChannelManager {
    pub fn new(max_buf_size: usize, is_initiator: bool, max_channels: usize) -> Self {
        let (local_base, peer_base) = if is_initiator { (0, 1) } else { (1, 0) };
        let initial = max_channels as u64;
        Self {
            channels: BTreeMap::new(),
            local_next_id: local_base,
            peer_next_id: peer_base,
            local_base,
            peer_base,
            max_buf_size,
            updated: BTreeSet::new(),
            max_channels,
            peer_max_channels: initial,
            advertised_max_channels: initial,
            sent_max_channels: initial,
            force_max_channels_update: false,
            pending_opens: Vec::new(),
            pending_fins: Vec::new(),
            send_cursor: 0,
        }
    }

    /// Send a message on a channel.  Returns the assigned message_id.
    ///
    /// Channels are semi-reliable: if the send buffer would exceed
    /// `max_buf_size`, the oldest unsent (or partially-sent) message is
    /// silently evicted.  Use `would_evict()` beforehand if the application
    /// wants to detect backpressure (e.g. slow network) and skip submitting.
    ///
    /// For local-parity IDs the channel is gap-filled if missing.
    /// For peer-parity IDs the channel must already exist (peer opens it),
    /// otherwise returns `UnknownChannel`.
    pub fn send(&mut self, channel_id: u64, data: Vec<u8>) -> Result<u64, ChannelError> {
        let data_len = data.len();
        let channel = if (channel_id % 2) == self.peer_base {
            self.channels
                .get_mut(&channel_id)
                .ok_or(ChannelError::UnknownChannel)?
        } else {
            self.get_or_create_local(channel_id)?
        };
        channel.evict_send(data_len);
        let message_id = channel.next_message_id;
        channel.next_message_id += 1;
        channel.send_buf_size += data_len;
        channel
            .send
            .insert(message_id, MessageFragmenter::new(data));
        Ok(message_id)
    }

    /// Create a local channel without sending data.
    /// Queues a ChannelOpen frame for reliable delivery.
    /// Rejects peer-parity IDs with `UnknownChannel`.
    pub fn on_local_open(&mut self, channel_id: u64) -> Result<(), ChannelError> {
        if (channel_id % 2) == self.peer_base {
            return Err(ChannelError::UnknownChannel);
        }
        self.get_or_create_local(channel_id)?;
        Ok(())
    }

    /// Receive a fragment from the network.
    /// If the assembler limit is reached, the oldest incomplete message is evicted.
    ///
    /// Peer-parity IDs are gap-filled (implicit open).
    /// Local-parity IDs must already exist — peer cannot fabricate channels
    /// on our side.  Returns `UnknownChannel` otherwise.
    pub fn recv(
        &mut self,
        channel_id: u64,
        message_id: u64,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<(), ChannelError> {
        let channel = if (channel_id % 2) == self.peer_base {
            self.get_or_create_peer(channel_id)?
        } else {
            self.channels
                .get_mut(&channel_id)
                .ok_or(ChannelError::UnknownChannel)?
        };

        let max_len = channel.max_buf_size as u64;
        let assembler = channel
            .recv
            .entry(message_id)
            .or_insert_with(|| MessageAssembler::new(max_len));

        let before = assembler.allocated();
        match assembler.write(offset, data, fin) {
            Ok(_) => {}
            Err(AssemblerError::TooLarge) => {
                // Fragment exceeds per-channel budget — drop this message entirely.
                let asm = channel.recv.remove(&message_id).unwrap();
                channel.recv_buf_size -= asm.allocated();
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        let after = assembler.allocated();
        channel.recv_buf_size += after - before;

        if assembler.is_complete() {
            channel.completed.insert(message_id);
        }

        // Evict oldest messages if total across messages exceeds budget.
        channel.evict_recv(0);

        self.updated.insert(channel_id);
        self.try_cleanup(channel_id);

        Ok(())
    }

    /// Handle a received ChannelOpen frame from peer.
    /// If the channel already exists (data arrived first), this is a no-op.
    /// Rejects local-parity IDs (peer cannot open our channels).
    pub fn on_peer_open(&mut self, channel_id: u64) -> Result<(), ChannelError> {
        if (channel_id % 2) != self.peer_base {
            return Err(ChannelError::UnknownChannel);
        }
        self.get_or_create_peer(channel_id)?;
        Ok(())
    }

    /// Handle a received `ChannelFin` frame from peer.
    /// Sets recv-side fin boundary.  Drops in-progress assemblers with
    /// `message_id >= last_message_id` (peer won't send those).
    /// May trigger auto-cleanup if our send side is also done.
    pub fn on_peer_fin(&mut self, channel_id: u64, last_message_id: u64) {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return;
        };
        // Idempotent: ignore later/duplicate fins.
        if channel.recv_fin.is_none() {
            channel.recv_fin = Some(last_message_id);
            // Prune assemblers above the boundary (peer won't send them).
            channel.recv.retain(|&id, asm| {
                if id >= last_message_id {
                    channel.recv_buf_size -= asm.allocated();
                    if asm.is_complete() {
                        channel.completed.remove(&id);
                    }
                    false
                } else {
                    true
                }
            });
            self.updated.insert(channel_id);
        }
        self.try_cleanup(channel_id);
    }

    /// Emit the next fragment for transmission.
    /// Returns `(channel_id, message_id, offset, len, fin)`.
    ///
    /// Round-robin across channels via `send_cursor`: pass 1 walks channels with
    /// `id >= send_cursor`, pass 2 wraps to `id < send_cursor`.  Within a
    /// channel, messages are tried in BTreeMap order (FIFO by message_id).
    pub fn emit(&mut self, out: &mut [u8]) -> Option<(u64, u64, u64, usize, bool)> {
        if out.is_empty() {
            return None;
        }

        let result = Self::try_emit_in(&mut self.channels, self.send_cursor.., out)
            .or_else(|| Self::try_emit_in(&mut self.channels, ..self.send_cursor, out));

        if let Some((channel_id, _, _, _, _)) = result {
            self.send_cursor = channel_id.saturating_add(1);
        }
        result
    }

    /// Try to emit from the first channel in `range` that has a fragment to send.
    fn try_emit_in<R: RangeBounds<u64>>(
        channels: &mut BTreeMap<u64, Channel>,
        range: R,
        out: &mut [u8],
    ) -> Option<(u64, u64, u64, usize, bool)> {
        for (&channel_id, channel) in channels.range_mut(range) {
            for (&message_id, entry) in channel.send.iter_mut() {
                if let Some((offset, len, fin)) = entry.emit(out) {
                    return Some((channel_id, message_id, offset, len, fin));
                }
            }
        }
        None
    }

    /// Poll for a completed (reassembled) message on a channel.
    /// May trigger auto-cleanup if this drains the recv side.
    pub fn poll(&mut self, channel_id: u64) -> Option<Vec<u8>> {
        let channel = self.channels.get_mut(&channel_id)?;
        let &message_id = channel.completed.first()?;
        channel.completed.remove(&message_id);
        let assembler = channel.recv.remove(&message_id).unwrap();
        channel.recv_buf_size -= assembler.allocated();
        let data = assembler.take();
        self.try_cleanup(channel_id);
        Some(data)
    }

    /// Acknowledge a fragment range. Removes acked range from retransmits.
    /// Cleans up the message when all fragments are sent and none need retransmission.
    /// May trigger auto-cleanup if this completes the send side.
    pub fn ack(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.ack(offset, len);
                if entry.is_done() {
                    let total = entry.len() as usize;
                    channel.send.remove(&message_id);
                    channel.send_buf_size -= total;
                }
            }
        }
        self.try_cleanup(channel_id);
    }

    /// A fragment was lost, needs retransmission.
    pub fn loss(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.loss(offset, len);
            }
        }
    }

    /// Half-close: signal that we will not send any more messages on this
    /// channel.  Queues a `ChannelFin` frame.  In-flight sends will continue
    /// to drain; the channel is removed when both sides have signalled fin
    /// and drained (`try_cleanup`).
    ///
    /// Returns `true` if fin was newly set, `false` if already closed or
    /// channel doesn't exist.
    pub fn close_send(&mut self, channel_id: u64) -> bool {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        if channel.send_fin.is_some() {
            return false;
        }
        let last_message_id = channel.next_message_id;
        channel.send_fin = Some(last_message_id);
        self.pending_fins.push((channel_id, last_message_id));
        self.updated.insert(channel_id);
        self.try_cleanup(channel_id);
        true
    }

    /// Remove the channel if both sides are fully finished.
    /// Cleaning up a peer-channel issues an extra credit via
    /// `advertised_max_channels`.
    fn try_cleanup(&mut self, channel_id: u64) {
        let Some(channel) = self.channels.get(&channel_id) else {
            return;
        };
        if !channel.send_finished() || !channel.recv_finished() {
            return;
        }
        self.channels.remove(&channel_id);
        if (channel_id % 2) == self.peer_base {
            self.advertised_max_channels = self.advertised_max_channels.saturating_add(1);
        }
        self.updated.insert(channel_id);
    }

    /// True if cumulative credit growth has reached the half-window threshold.
    pub fn should_update_max_channels(&self) -> bool {
        if self.force_max_channels_update {
            return true;
        }
        let delta = self
            .advertised_max_channels
            .saturating_sub(self.sent_max_channels);
        delta >= ((self.max_channels as u64) / 2).max(1)
    }

    /// Take the pending `MaxChannels` value to send, marking as sent.
    pub fn drain_max_channels_update(&mut self) -> Option<u64> {
        if !self.should_update_max_channels() {
            return None;
        }
        self.force_max_channels_update = false;
        self.sent_max_channels = self.advertised_max_channels;
        Some(self.advertised_max_channels)
    }

    /// Re-queue a `MaxChannels` update after the carrier frame was lost.
    pub fn requeue_max_channels_update(&mut self) {
        self.force_max_channels_update = true;
    }

    /// Receive a `MaxChannels` update from peer.  Increases our outgoing credit.
    pub fn update_send_max_channels(&mut self, count: u64) {
        if count > self.peer_max_channels {
            self.peer_max_channels = count;
        }
    }

    /// True if any channel has fragments to emit.
    pub fn has_pending(&self) -> bool {
        self.channels.values().any(|ch| {
            ch.send
                .values()
                .any(|e| !e.is_done() || e.has_retransmits())
        })
    }

    /// True if sending `data_len` bytes on `channel_id` would evict existing messages.
    pub fn would_evict(&self, channel_id: u64, data_len: usize) -> bool {
        match self.channels.get(&channel_id) {
            Some(ch) => ch.send_buf_size + data_len > ch.max_buf_size,
            None => false,
        }
    }

    /// Channels with completed messages ready to poll.
    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels
            .iter()
            .filter(|(_, ch)| !ch.completed.is_empty())
            .map(|(&id, _)| id)
    }

    /// Drain channel IDs whose state changed since last call into `out`.
    /// Caller's buffer is appended to (existing contents preserved).
    pub fn drain_updated(&mut self, out: &mut Vec<u64>) {
        out.extend(std::mem::take(&mut self.updated));
    }

    /// True if the channel exists in the manager and our send side is
    /// still open (no FIN queued locally). Returns false for unknown ids
    /// and for channels we've already closed via `close_send` — used by
    /// the node-level accept loop to distinguish fresh peer-initiated
    /// channels from stale surfacing of our own closed channels.
    pub fn is_writable(&self, channel_id: u64) -> bool {
        self.channels
            .get(&channel_id)
            .map_or(false, |c| c.send_fin.is_none())
    }

    /// Drain pending ChannelOpen frames for transmission into `out`.
    pub fn drain_pending_opens(&mut self, out: &mut Vec<u64>) {
        out.append(&mut self.pending_opens);
    }

    /// Drain pending ChannelFin frames `(channel_id, last_message_id)` into `out`.
    pub fn drain_pending_fins(&mut self, out: &mut Vec<(u64, u64)>) {
        out.append(&mut self.pending_fins);
    }

    /// Re-queue a ChannelOpen for retransmission (on loss).
    pub fn requeue_open(&mut self, channel_id: u64) {
        self.pending_opens.push(channel_id);
    }

    /// Re-queue a ChannelFin for retransmission (on loss).
    pub fn requeue_fin(&mut self, channel_id: u64, last_message_id: u64) {
        self.pending_fins.push((channel_id, last_message_id));
    }

    fn get_or_create_local(&mut self, channel_id: u64) -> Result<&mut Channel, ChannelError> {
        if self.channels.contains_key(&channel_id) {
            return Ok(self.channels.get_mut(&channel_id).unwrap());
        }
        if channel_id < self.local_next_id {
            return Err(ChannelError::IdReused);
        }
        // Cumulative-count credit check vs peer's grant.
        let opened_after = (channel_id - self.local_base) / 2 + 1;
        if opened_after > self.peer_max_channels {
            return Err(ChannelError::TooManyChannels);
        }
        let mut id = self.local_next_id;
        while id <= channel_id {
            self.channels.insert(id, Channel::new(self.max_buf_size));
            self.pending_opens.push(id);
            id += 2;
        }
        self.local_next_id = channel_id + 2;
        Ok(self.channels.get_mut(&channel_id).unwrap())
    }

    fn get_or_create_peer(&mut self, channel_id: u64) -> Result<&mut Channel, ChannelError> {
        if self.channels.contains_key(&channel_id) {
            return Ok(self.channels.get_mut(&channel_id).unwrap());
        }
        if channel_id < self.peer_next_id {
            return Err(ChannelError::IdReused);
        }
        // Peer must respect the credit we advertised.
        let opened_after = (channel_id - self.peer_base) / 2 + 1;
        if opened_after > self.advertised_max_channels {
            return Err(ChannelError::TooManyChannels);
        }
        let mut id = self.peer_next_id;
        while id <= channel_id {
            self.channels.insert(id, Channel::new(self.max_buf_size));
            self.updated.insert(id);
            id += 2;
        }
        self.peer_next_id = channel_id + 2;
        Ok(self.channels.get_mut(&channel_id).unwrap())
    }
}
