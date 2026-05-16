use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::RangeBounds;

use super::message::{AssemblerError, MessageAssembler, MessageFragmenter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    IdReused,
    UnknownChannel,
    TooManyChannels,
    AssemblerError(AssemblerError),
    /// Local send buffer is full of reliable messages, leaving no room for
    /// the new message and no unreliable messages to evict.  Caller must
    /// wait until reliable messages are acked.
    WouldBlock,
    /// Peer violated the channel protocol (bad evict size, post-evict overrun,
    /// fragment past `ChannelFin` boundary, exceeded advertised `max_data`).
    /// Caller should close the connection.
    ProtocolViolation,
}

impl From<AssemblerError> for ChannelError {
    fn from(err: AssemblerError) -> Self {
        ChannelError::AssemblerError(err)
    }
}

pub(super) struct SendMsg {
    pub(super) fragmenter: MessageFragmenter,
    pub(super) reliable: bool,
}

pub(super) struct Tombstone {
    /// Total bytes the sender accounted for this id (== `ChannelEvict.size`
    /// or the message's final length for delivered ids).
    pub(super) final_size: u64,
    /// Of `final_size`, how many bytes have already been counted into
    /// `data_received`.  Used so late/duplicate fragments contribute their
    /// real delta exactly once.
    pub(super) counted: u64,
}

pub(super) struct Channel {
    // -- Send side --
    pub(super) send: BTreeMap<u64, SendMsg>,
    pub(super) next_message_id: u64,
    /// Sum of `fragmenter.len()` across active send messages.  Used to
    /// backpressure `send()` so local memory does not grow unboundedly.
    send_buf_used: u64,
    send_buf_cap: u64,
    /// First message_id we will NOT send.  Set by `close_send()`.
    pub(super) send_fin: Option<u64>,
    /// Cumulative bytes ever emitted as new fragments on this channel.
    /// Retransmits do not advance this.  Bounded above by `peer_max_data`.
    pub(super) data_sent: u64,
    /// Latest cumulative byte budget advertised by peer.  Starts at the
    /// agreed initial window.
    pub(super) peer_max_data: u64,
    /// Pending ChannelEvict frames to send: (message_id, size).
    pub(super) pending_evicts: Vec<(u64, u64)>,

    // -- Recv side --
    pub(super) recv: BTreeMap<u64, MessageAssembler>,
    /// First message_id peer will NOT send.  Set by `on_peer_fin()`.
    pub(super) recv_fin: Option<u64>,
    /// Ids whose assembler `is_complete` and are waiting for `poll()`.
    pub(super) delivery_queue: VecDeque<u64>,
    /// Cumulative bytes counted as received (over all live + terminal ids).
    /// Monotonic.  Compared against `sent_max_data` to detect peer overrun.
    pub(super) data_received: u64,
    /// Cumulative `final_size` over terminal ids (delivered ∪ evicted).
    /// Drives `current_max_data`.  Monotonic.
    pub(super) released_total: u64,
    /// Last `max_data` we successfully sent to peer.  Monotonic — never
    /// reduced even if `recv_buf_cap` shrinks at runtime.
    pub(super) sent_max_data: u64,
    /// Force re-send of `ChannelMaxData` after a carrier frame was lost.
    pub(super) force_max_data_update: bool,
    /// How many in-flight (received-but-not-yet-released) bytes we are
    /// willing to hold.  The peer-visible `max_data = recv_buf_cap +
    /// released_total`, clamped to never fall below `sent_max_data` so the
    /// wire invariant of monotonic credit is preserved when this is shrunk.
    pub(super) recv_buf_cap: u64,
    /// Terminal ids with surviving state (above watermark).  Tracks
    /// `final_size` and how much was already counted in `data_received`.
    pub(super) tombstones: BTreeMap<u64, Tombstone>,
    /// All message_ids strictly less than this value are terminal.
    /// Used to garbage-collect `tombstones` for contiguous prefixes.
    pub(super) tombstone_watermark: u64,
}

impl Channel {
    fn new(send_buf_cap: u64, recv_buf_cap: u64) -> Self {
        Self {
            send: BTreeMap::new(),
            next_message_id: 0,
            send_buf_used: 0,
            send_buf_cap,
            send_fin: None,
            data_sent: 0,
            peer_max_data: recv_buf_cap,
            pending_evicts: Vec::new(),
            recv: BTreeMap::new(),
            recv_fin: None,
            delivery_queue: VecDeque::new(),
            data_received: 0,
            released_total: 0,
            sent_max_data: recv_buf_cap,
            force_max_data_update: false,
            recv_buf_cap,
            tombstones: BTreeMap::new(),
            tombstone_watermark: 0,
        }
    }

    fn send_finished(&self) -> bool {
        self.send_fin.is_some() && self.send.is_empty() && self.pending_evicts.is_empty()
    }

    fn recv_finished(&self) -> bool {
        self.recv_fin.is_some() && self.recv.is_empty() && self.delivery_queue.is_empty()
    }

    fn current_max_data(&self) -> u64 {
        // Monotonic clamp: even if `recv_buf_cap` was lowered at runtime,
        // we never revoke credit we already advertised.
        self.recv_buf_cap
            .saturating_add(self.released_total)
            .max(self.sent_max_data)
    }

    fn should_update_max_data(&self) -> bool {
        if self.force_max_data_update {
            return true;
        }
        let delta = self.current_max_data().saturating_sub(self.sent_max_data);
        // Threshold is half of the *current* cap; if cap shrunk below the
        // threshold, fall back to 1 so any progress still emits eventually.
        delta >= (self.recv_buf_cap / 2).max(1)
    }

    fn is_terminal(&self, id: u64) -> bool {
        id < self.tombstone_watermark || self.tombstones.contains_key(&id)
    }

    fn advance_watermark(&mut self) {
        while let Some((&id, _)) = self.tombstones.first_key_value() {
            if id == self.tombstone_watermark {
                self.tombstones.remove(&id);
                self.tombstone_watermark += 1;
            } else {
                break;
            }
        }
    }

    fn terminate(&mut self, id: u64, final_size: u64, counted: u64) {
        self.released_total = self.released_total.saturating_add(final_size);
        self.tombstones.insert(id, Tombstone { final_size, counted });
        if id == self.tombstone_watermark {
            self.advance_watermark();
        }
    }
}

/// Manages message-oriented channels with mixed reliability per message.
///
/// Each channel can carry many messages concurrently.  Per-message reliability
/// is chosen by the sender: `send(.., reliable=true)` cannot be evicted;
/// `send(.., reliable=false)` may be evicted at any point via `evict()`.
///
/// # Flow control
///
/// Per-channel cumulative byte window (QUIC-style):
/// - Sender: `data_sent` ≤ `peer_max_data`.  Retransmits do not advance
///   `data_sent`.  `data_sent` is never decremented; evicted bytes are
///   recovered when the peer's resulting `ChannelMaxData` update arrives.
/// - Receiver: `data_received` ≤ `sent_max_data`.  `sent_max_data` =
///   `recv_buf_cap + released_total`, where `released_total` accumulates
///   `final_size` for each terminal message (delivered or evicted).
///
/// `recv_buf_cap` is agreed implicitly at channel creation (same default
/// on both sides).  Must be ≥ the largest reliable message to avoid deadlock.
///
/// # Eviction
///
/// `evict(channel_id, message_id)` drops an unreliable message from the send
/// buffer and queues a `ChannelEvict { message_id, size }` frame.  `size` is
/// the sender's `max_offset_emitted` at evict time; the peer releases exactly
/// that many bytes of window upon receipt, regardless of how many fragments
/// physically arrived.  Late fragments after evict are absorbed via tombstone
/// bookkeeping without re-allocating assemblers.
///
/// # Lifecycle
///
/// Channels follow the same parity/gap-fill/`MaxChannels` mechanics as
/// streams.  A channel is removed when both sides have signalled fin and
/// drained.  Cleanup of a peer-channel grants one extra `MaxChannels` credit.
pub struct ChannelManager {
    pub(super) channels: BTreeMap<u64, Channel>,
    local_next_id: u64,
    peer_next_id: u64,
    local_base: u64,
    peer_base: u64,
    send_buf_cap: u64,
    recv_buf_cap: u64,
    updated: BTreeSet<u64>,
    max_channels: usize,
    peer_max_channels: u64,
    advertised_max_channels: u64,
    sent_max_channels: u64,
    force_max_channels_update: bool,
    pending_opens: Vec<u64>,
    pending_fins: Vec<(u64, u64)>,
    send_cursor: u64,
}

impl ChannelManager {
    pub fn new(
        send_buf_cap: u64,
        recv_buf_cap: u64,
        is_initiator: bool,
        max_channels: usize,
    ) -> Self {
        let (local_base, peer_base) = if is_initiator { (0, 1) } else { (1, 0) };
        let initial = max_channels as u64;
        Self {
            channels: BTreeMap::new(),
            local_next_id: local_base,
            peer_next_id: peer_base,
            local_base,
            peer_base,
            send_buf_cap,
            recv_buf_cap,
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

    /// Queue a message for transmission on a channel.  Returns the assigned
    /// `message_id`.
    ///
    /// If accepting this message would exceed the local send-buffer cap,
    /// the manager evicts the oldest unreliable in-flight message(s) to
    /// make room.  Evictions are signalled to the peer via `ChannelEvict`
    /// frames (which release flow-control budget once acked).
    ///
    /// Returns `WouldBlock` only if there are no unreliable messages left
    /// to evict and the buffer still cannot hold the new data — which can
    /// only happen when the buffer is full of *reliable* messages.
    pub fn send(
        &mut self,
        channel_id: u64,
        data: Vec<u8>,
        reliable: bool,
    ) -> Result<u64, ChannelError> {
        let data_len = data.len() as u64;
        let channel = if (channel_id % 2) == self.peer_base {
            self.channels
                .get_mut(&channel_id)
                .ok_or(ChannelError::UnknownChannel)?
        } else {
            self.get_or_create_local(channel_id)?
        };
        if channel.send_fin.is_some() {
            return Err(ChannelError::UnknownChannel);
        }
        while channel.send_buf_used.saturating_add(data_len) > channel.send_buf_cap {
            if Self::evict_oldest_unreliable(channel).is_none() {
                return Err(ChannelError::WouldBlock);
            }
        }
        let message_id = channel.next_message_id;
        channel.next_message_id += 1;
        channel.send_buf_used += data_len;
        channel.send.insert(
            message_id,
            SendMsg {
                fragmenter: MessageFragmenter::new(data),
                reliable,
            },
        );
        if channel.send.iter().any(|(_, m)| !m.reliable) {
            // Conservative: any unreliable in the buffer might be evictable
            // later, but we don't need to track that explicitly here.
        }
        self.updated.insert(channel_id);
        Ok(message_id)
    }

    /// Remove the oldest unreliable message from `channel.send`, drop its
    /// fragmenter, and enqueue a `ChannelEvict` frame.  Returns the evicted
    /// `message_id`, or `None` if no unreliable message is queued.
    fn evict_oldest_unreliable(channel: &mut Channel) -> Option<u64> {
        let (&message_id, msg) = channel.send.iter().find(|(_, m)| !m.reliable)?;
        let size = msg.fragmenter.max_offset_emitted();
        let total = msg.fragmenter.len();
        channel.send.remove(&message_id);
        channel.send_buf_used = channel.send_buf_used.saturating_sub(total);
        channel.pending_evicts.push((message_id, size));
        Some(message_id)
    }

    /// Create a local channel without sending data.  Queues a ChannelOpen
    /// frame.  Rejects peer-parity ids.
    pub fn on_local_open(&mut self, channel_id: u64) -> Result<(), ChannelError> {
        if (channel_id % 2) == self.peer_base {
            return Err(ChannelError::UnknownChannel);
        }
        self.get_or_create_local(channel_id)?;
        Ok(())
    }

    /// Receive a fragment.  Peer-parity ids are gap-filled; local-parity ids
    /// must already exist.
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

        // Fragment past the peer's advertised fin boundary.
        if let Some(fin_id) = channel.recv_fin {
            if message_id >= fin_id {
                return Err(ChannelError::ProtocolViolation);
            }
        }

        let frag_end = offset.saturating_add(data.len() as u64);

        // Terminal id: absorb the fragment into the tombstone (for counted
        // bookkeeping) without re-allocating an assembler.
        if channel.is_terminal(message_id) {
            if let Some(ts) = channel.tombstones.get_mut(&message_id) {
                if frag_end > ts.final_size {
                    return Err(ChannelError::ProtocolViolation);
                }
                let new_counted = ts.counted.max(frag_end);
                let delta = new_counted - ts.counted;
                ts.counted = new_counted;
                channel.data_received = channel.data_received.saturating_add(delta);
                if channel.data_received > channel.sent_max_data {
                    return Err(ChannelError::ProtocolViolation);
                }
            }
            // Below watermark: tombstone GC'd, silently drop the fragment.
            return Ok(());
        }

        let assembler = channel
            .recv
            .entry(message_id)
            .or_insert_with(|| MessageAssembler::new(u64::MAX));

        let before = assembler.max_offset_received();
        assembler.write(offset, data, fin)?;
        let after = assembler.max_offset_received();
        let delta = after - before;
        channel.data_received = channel.data_received.saturating_add(delta);
        if channel.data_received > channel.sent_max_data {
            return Err(ChannelError::ProtocolViolation);
        }

        if assembler.is_complete() && !channel.delivery_queue.contains(&message_id) {
            channel.delivery_queue.push_back(message_id);
        }

        self.updated.insert(channel_id);
        self.try_cleanup(channel_id);

        Ok(())
    }

    /// Handle a received ChannelOpen.  Idempotent.
    pub fn on_peer_open(&mut self, channel_id: u64) -> Result<(), ChannelError> {
        if (channel_id % 2) != self.peer_base {
            return Err(ChannelError::UnknownChannel);
        }
        self.get_or_create_peer(channel_id)?;
        Ok(())
    }

    /// Handle a received ChannelFin.  Idempotent.  Returns `ProtocolViolation`
    /// if `last_message_id` contradicts already-received fragments.
    pub fn on_peer_fin(
        &mut self,
        channel_id: u64,
        last_message_id: u64,
    ) -> Result<(), ChannelError> {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return Ok(());
        };
        // Reject contradictory fin: peer claims they won't send id ≥ X,
        // but we already have an assembler for id ≥ X.
        if let Some((&max_seen, _)) = channel.recv.iter().next_back() {
            if max_seen >= last_message_id {
                return Err(ChannelError::ProtocolViolation);
            }
        }
        // Same check for tombstoned ids that came from peer-side evict at id ≥ X.
        if let Some((&max_terminal, _)) = channel.tombstones.iter().next_back() {
            if max_terminal >= last_message_id {
                return Err(ChannelError::ProtocolViolation);
            }
        }
        if channel.recv_fin.is_none() {
            channel.recv_fin = Some(last_message_id);
            self.updated.insert(channel_id);
        }
        self.try_cleanup(channel_id);
        Ok(())
    }

    /// Handle a received ChannelEvict from peer.  Transitions the message
    /// into Evicted state; subsequent fragments are absorbed by the tombstone.
    pub fn on_peer_evict(
        &mut self,
        channel_id: u64,
        message_id: u64,
        size: u64,
    ) -> Result<(), ChannelError> {
        let channel = if (channel_id % 2) == self.peer_base {
            self.get_or_create_peer(channel_id)?
        } else {
            self.channels
                .get_mut(&channel_id)
                .ok_or(ChannelError::UnknownChannel)?
        };

        if let Some(fin_id) = channel.recv_fin {
            if message_id >= fin_id {
                return Err(ChannelError::ProtocolViolation);
            }
        }

        if channel.is_terminal(message_id) {
            // Late evict for already-delivered/evicted id — no-op.
            return Ok(());
        }

        let counted = if let Some(asm) = channel.recv.get(&message_id) {
            let received = asm.max_offset_received();
            if size < received {
                return Err(ChannelError::ProtocolViolation);
            }
            received
        } else {
            0
        };

        if channel.recv.remove(&message_id).is_some() {
            // It might have been queued for delivery already.
            channel.delivery_queue.retain(|&id| id != message_id);
        }
        channel.terminate(message_id, size, counted);

        self.updated.insert(channel_id);
        self.try_cleanup(channel_id);
        Ok(())
    }

    /// Handle a received ChannelMaxData.  Cumulative — smaller values ignored.
    pub fn on_peer_max_data(&mut self, channel_id: u64, max_data: u64) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if max_data > channel.peer_max_data {
                channel.peer_max_data = max_data;
                self.updated.insert(channel_id);
            }
        }
    }

    /// Emit the next fragment for transmission.  Window-aware: new bytes
    /// stop at `peer_max_data`; retransmits are emitted regardless.
    ///
    /// Returns `(channel_id, message_id, offset, len, fin)`.
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

    fn try_emit_in<R: RangeBounds<u64>>(
        channels: &mut BTreeMap<u64, Channel>,
        range: R,
        out: &mut [u8],
    ) -> Option<(u64, u64, u64, usize, bool)> {
        for (&channel_id, channel) in channels.range_mut(range) {
            let avail = channel.peer_max_data.saturating_sub(channel.data_sent);
            for (&message_id, send_msg) in channel.send.iter_mut() {
                let frag = &mut send_msg.fragmenter;
                let was_offset = frag.max_offset_emitted();
                let result = if frag.has_retransmits() {
                    frag.emit(out)
                } else if avail == 0 {
                    None
                } else {
                    let bounded = (out.len() as u64).min(avail) as usize;
                    frag.emit(&mut out[..bounded])
                };
                if let Some((offset, len, fin)) = result {
                    let new_bytes = frag.max_offset_emitted().saturating_sub(was_offset);
                    channel.data_sent = channel.data_sent.saturating_add(new_bytes);
                    return Some((channel_id, message_id, offset, len, fin));
                }
            }
        }
        None
    }

    /// Pop a completed message from the delivery queue.  Releases its
    /// `final_size` bytes back to the receive window (advances
    /// `released_total`, may trigger a `ChannelMaxData` update).
    pub fn poll(&mut self, channel_id: u64) -> Option<Vec<u8>> {
        let channel = self.channels.get_mut(&channel_id)?;
        let message_id = channel.delivery_queue.pop_front()?;
        let assembler = channel.recv.remove(&message_id)?;
        // Delivered messages always have a FIN — their fin_off is the final size.
        let final_size = assembler.fin_off().unwrap_or_else(|| assembler.max_offset_received());
        let data = assembler.take();
        channel.terminate(message_id, final_size, final_size);
        self.try_cleanup(channel_id);
        Some(data)
    }

    pub fn ack(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.fragmenter.ack(offset, len);
                if entry.fragmenter.is_done() {
                    let total = entry.fragmenter.len();
                    channel.send.remove(&message_id);
                    channel.send_buf_used = channel.send_buf_used.saturating_sub(total);
                }
            }
        }
        self.try_cleanup(channel_id);
    }

    pub fn loss(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.fragmenter.loss(offset, len);
            }
        }
    }

    /// Discard all Ready (but not yet polled) messages on the channel,
    /// releasing their window budget.  Used when the application abandons
    /// its receive handle: keeps flow control consistent with the sender's
    /// `data_sent` counter so the channel can be cleaned up cleanly once
    /// the peer signals `ChannelFin`.
    pub fn drain_delivery_queue(&mut self, channel_id: u64) {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return;
        };
        while let Some(message_id) = channel.delivery_queue.pop_front() {
            if let Some(assembler) = channel.recv.remove(&message_id) {
                let final_size = assembler
                    .fin_off()
                    .unwrap_or_else(|| assembler.max_offset_received());
                channel.terminate(message_id, final_size, final_size);
            }
        }
        self.try_cleanup(channel_id);
    }

    /// Half-close: no more new messages from us on this channel.  Queues
    /// `ChannelFin { last_message_id = next_message_id }`.  Already-queued
    /// messages continue to drain (reliable ones must complete, unreliable
    /// may be evicted).
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

    // -- MaxChannels (count-based) ------------------------------------------

    pub fn should_update_max_channels(&self) -> bool {
        if self.force_max_channels_update {
            return true;
        }
        let delta = self
            .advertised_max_channels
            .saturating_sub(self.sent_max_channels);
        delta >= ((self.max_channels as u64) / 2).max(1)
    }

    pub fn drain_max_channels_update(&mut self) -> Option<u64> {
        if !self.should_update_max_channels() {
            return None;
        }
        self.force_max_channels_update = false;
        self.sent_max_channels = self.advertised_max_channels;
        Some(self.advertised_max_channels)
    }

    pub fn requeue_max_channels_update(&mut self) {
        self.force_max_channels_update = true;
    }

    pub fn update_send_max_channels(&mut self, count: u64) {
        if count > self.peer_max_channels {
            self.peer_max_channels = count;
        }
    }

    // -- ChannelMaxData (byte-based per channel) ----------------------------

    /// Drain pending per-channel MaxData updates into `out`: `(channel_id, max_data)`.
    pub fn drain_max_data_updates(&mut self, out: &mut Vec<(u64, u64)>) {
        for (&channel_id, channel) in &mut self.channels {
            if channel.should_update_max_data() {
                let val = channel.current_max_data();
                channel.sent_max_data = val;
                channel.force_max_data_update = false;
                out.push((channel_id, val));
            }
        }
    }

    pub fn requeue_max_data_update(&mut self, channel_id: u64) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            channel.force_max_data_update = true;
        }
    }

    // -- Runtime buffer resize ----------------------------------------------

    /// Resize the local send-buffer cap.  Affects future `send()` calls only:
    /// already-queued messages stay in place.  Returns `false` if the channel
    /// doesn't exist.
    pub fn set_send_buf_cap(&mut self, channel_id: u64, cap: u64) -> bool {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        channel.send_buf_cap = cap;
        true
    }

    /// Resize the receive-buffer cap (the amount of in-flight bytes we are
    /// willing to hold from the peer).
    ///
    /// - Growing: increases `current_max_data`; the next drain emits a
    ///   `ChannelMaxData` with the new larger value, granting the peer more
    ///   credit.
    /// - Shrinking: does **not** revoke already-advertised credit (the wire
    ///   invariant is monotonic).  Future credit growth slows or stalls until
    ///   `released_total` catches up.
    ///
    /// Returns `false` if the channel doesn't exist.
    pub fn set_recv_buf_cap(&mut self, channel_id: u64, cap: u64) -> bool {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        channel.recv_buf_cap = cap;
        true
    }

    // -- ChannelEvict --------------------------------------------------------

    /// Drain pending evicts into `out`: `(channel_id, message_id, size)`.
    pub fn drain_pending_evicts(&mut self, out: &mut Vec<(u64, u64, u64)>) {
        for (&channel_id, channel) in &mut self.channels {
            for (mid, size) in channel.pending_evicts.drain(..) {
                out.push((channel_id, mid, size));
            }
        }
    }

    pub fn requeue_evict(&mut self, channel_id: u64, message_id: u64, size: u64) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            channel.pending_evicts.push((message_id, size));
        }
    }

    // -- Queries -------------------------------------------------------------

    pub fn has_pending(&self) -> bool {
        self.channels.values().any(|ch| {
            if !ch.pending_evicts.is_empty() || ch.should_update_max_data() {
                return true;
            }
            let avail = ch.peer_max_data.saturating_sub(ch.data_sent);
            ch.send.values().any(|m| {
                let frag = &m.fragmenter;
                let unsent = frag.len().saturating_sub(frag.max_offset_emitted());
                (unsent > 0 && avail > 0) || frag.has_retransmits()
            })
        })
    }

    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels
            .iter()
            .filter(|(_, ch)| !ch.delivery_queue.is_empty())
            .map(|(&id, _)| id)
    }

    pub fn drain_updated(&mut self, out: &mut Vec<u64>) {
        out.extend(std::mem::take(&mut self.updated));
    }

    pub fn is_writable(&self, channel_id: u64) -> bool {
        self.channels
            .get(&channel_id)
            .map_or(false, |c| c.send_fin.is_none())
    }

    pub fn drain_pending_opens(&mut self, out: &mut Vec<u64>) {
        out.append(&mut self.pending_opens);
    }

    pub fn drain_pending_fins(&mut self, out: &mut Vec<(u64, u64)>) {
        out.append(&mut self.pending_fins);
    }

    pub fn requeue_open(&mut self, channel_id: u64) {
        self.pending_opens.push(channel_id);
    }

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
        let opened_after = (channel_id - self.local_base) / 2 + 1;
        if opened_after > self.peer_max_channels {
            return Err(ChannelError::TooManyChannels);
        }
        let mut id = self.local_next_id;
        while id <= channel_id {
            self.channels
                .insert(id, Channel::new(self.send_buf_cap, self.recv_buf_cap));
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
        let opened_after = (channel_id - self.peer_base) / 2 + 1;
        if opened_after > self.advertised_max_channels {
            return Err(ChannelError::TooManyChannels);
        }
        let mut id = self.peer_next_id;
        while id <= channel_id {
            self.channels
                .insert(id, Channel::new(self.send_buf_cap, self.recv_buf_cap));
            self.updated.insert(id);
            id += 2;
        }
        self.peer_next_id = channel_id + 2;
        Ok(self.channels.get_mut(&channel_id).unwrap())
    }
}
