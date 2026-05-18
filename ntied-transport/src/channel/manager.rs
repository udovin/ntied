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

/// Test-only helper: predicate "is `id` terminal in this channel?".
#[cfg(test)]
pub(super) fn is_terminal_for_tests(ch: &Channel, id: u64) -> bool {
    id < ch.peer_next_msg_id && !ch.recv.contains_key(&id)
}

pub(super) struct Channel {
    // -- Send side --
    pub(super) send: BTreeMap<u64, SendMsg>,
    /// Subset of `send` containing message_ids whose fragmenter is **not**
    /// `is_done` — i.e. has unsent new bytes or pending retransmits.  `emit()`
    /// iterates this set so it doesn't pay O(K) per call to skip over
    /// fully-emitted messages waiting for ack.
    ///
    /// Invariant: `id ∈ send_emittable` ⇔ `id ∈ send` ∧ `!send[id].fragmenter.is_done()`.
    pub(super) send_emittable: BTreeSet<u64>,
    /// Subset of `send` whose `reliable` flag is false.  Lets
    /// `evict_oldest_unreliable` return the leftmost in O(log) instead of
    /// linearly scanning `send`.
    ///
    /// Invariant: `id ∈ unreliable_ids` ⇔ `id ∈ send` ∧ `!send[id].reliable`.
    pub(super) unreliable_ids: BTreeSet<u64>,
    pub(super) next_message_id: u64,
    /// Sum of `fragmenter.len()` across active send messages.  Used to
    /// backpressure `send()` so local memory does not grow unboundedly.
    send_buf_used: u64,
    send_buf_cap: u64,
    /// Local cap on number of concurrent in-flight send messages.  Symmetric
    /// to `send_buf_cap` (bytes).  When `send.len() >= send_msg_cap`, `send()`
    /// auto-evicts the oldest unreliable msg or returns `WouldBlock`.  This is
    /// purely local backpressure — independent from `peer_max_messages`, which
    /// is a wire-level credit advertised by the peer.
    send_msg_cap: u64,
    /// First message_id we will NOT send.  Set by `close_send()`.
    pub(super) send_fin: Option<u64>,
    /// Cumulative bytes ever emitted as new fragments on this channel.
    /// Retransmits do not advance this.  Bounded above by `peer_max_data`.
    pub(super) data_sent: u64,
    /// Latest cumulative byte budget advertised by peer.  Starts at the
    /// agreed initial window.
    pub(super) peer_max_data: u64,
    /// Latest cumulative message-count budget advertised by peer.  Sender
    /// may allocate ids `[0, peer_max_messages)`.  Starts at the agreed
    /// initial value.
    pub(super) peer_max_messages: u64,
    /// Pending ChannelEvict frames to send: (message_id, size).
    pub(super) pending_evicts: Vec<(u64, u64)>,

    // -- Recv side --
    /// Currently-assembling messages, indexed by id.  Sender allocates ids
    /// sequentially; receiver gap-fills missing ids with empty assemblers
    /// when a higher id is observed first.  Entries are removed on
    /// completion (data moves to `ready`) or eviction — both cases mean
    /// "id is terminal" and the absence-from-recv is the only marker.
    pub(super) recv: BTreeMap<u64, MessageAssembler>,
    /// FIFO of completed messages, in completion order.  Holds message
    /// data directly — assembler is consumed when transitioning to `ready`.
    pub(super) ready: VecDeque<Vec<u8>>,
    /// Highest id we've ever seen + 1 (i.e. the next id we'd consider
    /// "new").  Like `peer_next_id` for channels.  An id `< peer_next_msg_id`
    /// that is **not** in `recv` is by definition terminal (was processed
    /// and removed).
    pub(super) peer_next_msg_id: u64,
    /// First message_id peer will NOT send.  Set by `on_peer_fin()`.
    pub(super) recv_fin: Option<u64>,
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
    /// How many concurrently-active message ids we are willing to hold.
    /// Peer-visible `max_messages = recv_msg_cap + terminated_msg_count`,
    /// clamped against `sent_max_messages` for monotonicity.
    pub(super) recv_msg_cap: u64,
    /// Cumulative count of messages that reached a terminal state (delivered
    /// via `poll`, or evicted from Assembling).  Drives `current_max_messages`.
    /// Monotonic.
    pub(super) terminated_msg_count: u64,
    /// Last `max_messages` we successfully sent to peer.
    pub(super) sent_max_messages: u64,
}

impl Channel {
    fn new(
        send_buf_cap: u64,
        send_msg_cap: u64,
        recv_buf_cap: u64,
        recv_msg_cap: u64,
    ) -> Self {
        Self {
            send: BTreeMap::new(),
            send_emittable: BTreeSet::new(),
            unreliable_ids: BTreeSet::new(),
            next_message_id: 0,
            send_buf_used: 0,
            send_buf_cap,
            send_msg_cap,
            send_fin: None,
            data_sent: 0,
            peer_max_data: recv_buf_cap,
            peer_max_messages: recv_msg_cap,
            pending_evicts: Vec::new(),
            recv: BTreeMap::new(),
            ready: VecDeque::new(),
            peer_next_msg_id: 0,
            recv_fin: None,
            data_received: 0,
            released_total: 0,
            sent_max_data: recv_buf_cap,
            force_max_data_update: false,
            recv_buf_cap,
            recv_msg_cap,
            terminated_msg_count: 0,
            sent_max_messages: recv_msg_cap,
        }
    }

    fn send_finished(&self) -> bool {
        self.send_fin.is_some() && self.send.is_empty() && self.pending_evicts.is_empty()
    }

    /// Recv side has drained: peer signalled fin AND no Assembling or
    /// queued-Ready msgs remain.
    fn recv_finished(&self) -> bool {
        self.recv_fin.is_some() && self.recv.is_empty() && self.ready.is_empty()
    }

    fn current_max_data(&self) -> u64 {
        // Monotonic clamp: even if `recv_buf_cap` was lowered at runtime,
        // we never revoke credit we already advertised.
        self.recv_buf_cap
            .saturating_add(self.released_total)
            .max(self.sent_max_data)
    }

    fn current_max_messages(&self) -> u64 {
        self.recv_msg_cap
            .saturating_add(self.terminated_msg_count)
            .max(self.sent_max_messages)
    }

    fn should_update_max_data(&self) -> bool {
        if self.force_max_data_update {
            return true;
        }
        let data_delta = self.current_max_data().saturating_sub(self.sent_max_data);
        let msg_delta = self
            .current_max_messages()
            .saturating_sub(self.sent_max_messages);
        // Half-window threshold on either dimension triggers an update.
        data_delta >= (self.recv_buf_cap / 2).max(1) || msg_delta >= (self.recv_msg_cap / 2).max(1)
    }

    /// Gap-fill `recv` with empty assemblers from `peer_next_msg_id` up to
    /// and including `id`, then advance `peer_next_msg_id`.  No-op if
    /// `id < peer_next_msg_id`.  Rejects if peer would exceed the
    /// `sent_max_messages` budget we advertised.
    fn gap_fill(&mut self, id: u64) -> Result<(), ChannelError> {
        if id < self.peer_next_msg_id {
            return Ok(());
        }
        // Peer would set its `next_message_id = id + 1` after this fragment.
        // We must have advertised at least that many allocations.
        if id + 1 > self.sent_max_messages {
            return Err(ChannelError::ProtocolViolation);
        }
        for m in self.peer_next_msg_id..=id {
            self.recv.insert(m, MessageAssembler::new());
        }
        self.peer_next_msg_id = id + 1;
        Ok(())
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
    send_msg_cap: u64,
    recv_buf_cap: u64,
    recv_msg_cap: u64,
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
        send_msg_cap: u64,
        recv_buf_cap: u64,
        recv_msg_cap: u64,
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
            send_msg_cap,
            recv_buf_cap,
            recv_msg_cap,
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
        // Per-channel message-count cap: peer advertised that we may allocate
        // at most `peer_max_messages` ids cumulatively.  Block if we've hit it.
        if channel.next_message_id >= channel.peer_max_messages {
            return Err(ChannelError::WouldBlock);
        }
        // Local in-flight message-count cap.  Symmetric to `send_buf_cap`:
        // first try to evict an unreliable, otherwise WouldBlock.
        while channel.send.len() as u64 >= channel.send_msg_cap {
            if Self::evict_oldest_unreliable(channel).is_none() {
                return Err(ChannelError::WouldBlock);
            }
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
        // Non-empty message has unsent bytes → emittable.  Zero-byte messages
        // are immediately is_done and aren't tracked.
        if data_len > 0 {
            channel.send_emittable.insert(message_id);
        }
        if !reliable {
            channel.unreliable_ids.insert(message_id);
        }
        self.updated.insert(channel_id);
        Ok(message_id)
    }

    /// Remove the oldest unreliable message from `channel.send`, drop its
    /// fragmenter, and enqueue a `ChannelEvict` frame.  Returns the evicted
    /// `message_id`, or `None` if no unreliable message is queued.
    fn evict_oldest_unreliable(channel: &mut Channel) -> Option<u64> {
        let &message_id = channel.unreliable_ids.iter().next()?;
        let msg = channel
            .send
            .remove(&message_id)
            .expect("unreliable_ids out of sync");
        let size = msg.fragmenter.max_offset_emitted();
        let total = msg.fragmenter.len();
        channel.send_emittable.remove(&message_id);
        channel.unreliable_ids.remove(&message_id);
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

        // Gap-fill if peer is sending a higher id than we've seen — ids
        // are sequential per sender, so 0..id-1 are either in-flight or
        // already terminated.  Pre-allocating empty assemblers lets late
        // fragments for in-between ids land correctly.
        channel.gap_fill(message_id)?;

        // Late fragment for a terminal id — silently drop.  Terminal means
        // either completed (data already in `ready`) or evicted: in both
        // cases the assembler was removed from `recv`.
        let Some(assembler) = channel.recv.get_mut(&message_id) else {
            return Ok(());
        };

        let before = assembler.max_offset_received();
        let was_complete = assembler.is_complete();
        assembler.write(offset, data, fin)?;
        let after = assembler.max_offset_received();
        let delta = after - before;
        let now_complete = assembler.is_complete();
        // Drop the &mut borrow before mutating other fields of `channel`.
        let _ = assembler;

        channel.data_received = channel.data_received.saturating_add(delta);
        if channel.data_received > channel.sent_max_data {
            return Err(ChannelError::ProtocolViolation);
        }

        if !was_complete && now_complete {
            // Move data out of the assembler into `ready`.  Removal from
            // `recv` is enough to mark id terminal:
            // `id < peer_next_msg_id && !recv.contains_key(&id)`.
            let asm = channel.recv.remove(&message_id).unwrap();
            channel.ready.push_back(asm.take());
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
    /// if `last_message_id` contradicts already-received state.
    pub fn on_peer_fin(
        &mut self,
        channel_id: u64,
        last_message_id: u64,
    ) -> Result<(), ChannelError> {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return Ok(());
        };
        // Reject contradictory fin: peer claims they won't send id ≥ X,
        // but we already observed an id ≥ X (either still in `recv` or
        // already promoted to `peer_next_msg_id`).
        if channel.peer_next_msg_id > last_message_id {
            return Err(ChannelError::ProtocolViolation);
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

        // Gap-fill for this id (sender's evict may be the first thing we
        // hear about this msg if all its frags were lost in transit).
        channel.gap_fill(message_id)?;

        // If msg is in `recv` (Assembling, possibly empty from gap-fill):
        // validate evict size against what we've accepted, drop assembler,
        // release `size` to the window.  If msg is NOT in `recv`, it was
        // already completed or evicted — late evict is a no-op for
        // released_total (poll bumps for completed; double-release would
        // happen if we did anything here).
        if let Some(asm) = channel.recv.remove(&message_id) {
            if size < asm.max_offset_received() {
                return Err(ChannelError::ProtocolViolation);
            }
            channel.released_total = channel.released_total.saturating_add(size);
            channel.terminated_msg_count = channel.terminated_msg_count.saturating_add(1);
        }

        self.updated.insert(channel_id);
        self.try_cleanup(channel_id);
        Ok(())
    }

    /// Handle a received ChannelMaxData.  Both values are cumulative —
    /// smaller-or-equal values ignored.
    pub fn on_peer_max_data(&mut self, channel_id: u64, max_data: u64, max_messages: u64) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            let mut changed = false;
            if max_data > channel.peer_max_data {
                channel.peer_max_data = max_data;
                changed = true;
            }
            if max_messages > channel.peer_max_messages {
                channel.peer_max_messages = max_messages;
                changed = true;
            }
            if changed {
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
            // Fast path: window has credit → any emittable will do, take the
            // leftmost id directly without a second map lookup.
            // Slow path: window blocked → only retransmits emit, scan
            // emittable for one (typically empty or small).
            let message_id = if avail > 0 {
                match channel.send_emittable.iter().next() {
                    Some(&mid) => mid,
                    None => continue,
                }
            } else {
                let mut found = None;
                for &mid in &channel.send_emittable {
                    if channel
                        .send
                        .get(&mid)
                        .map_or(false, |m| m.fragmenter.has_retransmits())
                    {
                        found = Some(mid);
                        break;
                    }
                }
                match found {
                    Some(mid) => mid,
                    None => continue,
                }
            };

            let send_msg = channel.send.get_mut(&message_id).unwrap();
            let frag = &mut send_msg.fragmenter;
            let was_offset = frag.max_offset_emitted();
            let result = if frag.has_retransmits() {
                frag.emit(out)
            } else {
                let bounded = (out.len() as u64).min(avail) as usize;
                frag.emit(&mut out[..bounded])
            };
            let Some((offset, len, fin)) = result else {
                continue;
            };

            let new_bytes = frag.max_offset_emitted().saturating_sub(was_offset);
            let done = frag.is_done();
            channel.data_sent = channel.data_sent.saturating_add(new_bytes);
            if done {
                // Fully transmitted, awaiting ack — leave in `send` but
                // drop from emittable.  `loss()` re-inserts if needed.
                channel.send_emittable.remove(&message_id);
            }
            return Some((channel_id, message_id, offset, len, fin));
        }
        None
    }

    /// Pop a completed message from `ready` and release its size to the
    /// receive window (advances `released_total`, may trigger a
    /// `ChannelMaxData` update on next drain).
    pub fn poll(&mut self, channel_id: u64) -> Option<Vec<u8>> {
        let channel = self.channels.get_mut(&channel_id)?;
        let data = channel.ready.pop_front()?;
        channel.released_total = channel.released_total.saturating_add(data.len() as u64);
        channel.terminated_msg_count = channel.terminated_msg_count.saturating_add(1);
        self.try_cleanup(channel_id);
        Some(data)
    }

    pub fn ack(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.fragmenter.ack(offset, len);
                if entry.fragmenter.is_done() {
                    let total = entry.fragmenter.len();
                    let was_unreliable = !entry.reliable;
                    channel.send.remove(&message_id);
                    channel.send_emittable.remove(&message_id);
                    if was_unreliable {
                        channel.unreliable_ids.remove(&message_id);
                    }
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
                // Loss with non-zero range introduces retransmits — revives
                // emittability for a previously ack-wait message.
                if entry.fragmenter.has_retransmits() {
                    channel.send_emittable.insert(message_id);
                }
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
        while let Some(data) = channel.ready.pop_front() {
            channel.released_total = channel.released_total.saturating_add(data.len() as u64);
            channel.terminated_msg_count = channel.terminated_msg_count.saturating_add(1);
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

    /// Drain pending per-channel MaxData updates into `out`:
    /// `(channel_id, max_data, max_messages)`.
    pub fn drain_max_data_updates(&mut self, out: &mut Vec<(u64, u64, u64)>) {
        for (&channel_id, channel) in &mut self.channels {
            if channel.should_update_max_data() {
                let max_data = channel.current_max_data();
                let max_messages = channel.current_max_messages();
                channel.sent_max_data = max_data;
                channel.sent_max_messages = max_messages;
                channel.force_max_data_update = false;
                out.push((channel_id, max_data, max_messages));
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

    /// Resize the local send-side message-count cap.  Symmetric to
    /// `set_send_buf_cap` (bytes).  Affects future `send()` calls only.
    pub fn set_send_msg_cap(&mut self, channel_id: u64, cap: u64) -> bool {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        channel.send_msg_cap = cap;
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

    /// Resize the in-flight message-count cap.  Same monotonic semantics as
    /// `set_recv_buf_cap`: grow advertises more credit on next drain;
    /// shrink does not revoke already-granted credit.
    pub fn set_recv_msg_cap(&mut self, channel_id: u64, cap: u64) -> bool {
        let Some(channel) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        channel.recv_msg_cap = cap;
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
            if ch.send_emittable.is_empty() {
                return false;
            }
            let avail = ch.peer_max_data.saturating_sub(ch.data_sent);
            if avail > 0 {
                return true;
            }
            // Window blocked: only retransmits can emit.  Scan emittable
            // (typically small) for one with retransmits.
            ch.send_emittable.iter().any(|mid| {
                ch.send
                    .get(mid)
                    .map_or(false, |m| m.fragmenter.has_retransmits())
            })
        })
    }

    pub fn readable_channels(&self) -> impl Iterator<Item = u64> + '_ {
        self.channels
            .iter()
            .filter(|(_, ch)| !ch.ready.is_empty())
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
            self.channels.insert(
                id,
                Channel::new(
                    self.send_buf_cap,
                    self.send_msg_cap,
                    self.recv_buf_cap,
                    self.recv_msg_cap,
                ),
            );
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
            self.channels.insert(
                id,
                Channel::new(
                    self.send_buf_cap,
                    self.send_msg_cap,
                    self.recv_buf_cap,
                    self.recv_msg_cap,
                ),
            );
            self.updated.insert(id);
            id += 2;
        }
        self.peer_next_id = channel_id + 2;
        Ok(self.channels.get_mut(&channel_id).unwrap())
    }
}
