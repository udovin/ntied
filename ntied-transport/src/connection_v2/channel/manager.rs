use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

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

pub(super) struct SendEntry {
    frag: MessageFragmenter,
    deadline: Instant,
}

pub(super) struct Channel {
    pub(super) send: BTreeMap<u64, SendEntry>,
    pub(super) recv: BTreeMap<u64, MessageAssembler>,
    next_message_id: u64,
    completed: BTreeSet<u64>,
    send_buf_size: usize,
    recv_buf_size: usize,
    max_buf_size: usize,
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
        }
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
            self.send_buf_size -= entry.frag.len() as usize;
        }
    }

    /// Drop expired send messages.
    fn expire_send(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .send
            .iter()
            .filter(|(_, e)| now >= e.deadline)
            .map(|(&id, _)| id)
            .collect();
        for id in expired {
            let entry = self.send.remove(&id).unwrap();
            self.send_buf_size -= entry.frag.len() as usize;
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
/// `close()` drops all in-flight data, removes the channel, and queues
/// a `ChannelClose` frame.
pub struct ChannelManager {
    pub(super) channels: HashMap<u64, Channel>,
    /// Next ID to allocate for locally-created channels.
    local_next_id: u64,
    /// Next ID to allocate for peer-created channels.
    peer_next_id: u64,
    /// Peer channel ID base (0 for even, 1 for odd).
    peer_base: u64,
    max_buf_size: usize,
    /// Channel IDs whose state changed since last drain.
    updated: BTreeSet<u64>,
    /// Maximum number of channels per direction.
    max_channels: usize,
    /// Current count of locally-created channels.
    local_count: usize,
    /// Current count of peer-created channels.
    peer_count: usize,
    /// Pending ChannelOpen frames to send (reliable).
    pending_opens: Vec<u64>,
    /// Pending ChannelClose frames to send (reliable).
    pending_closes: Vec<u64>,
}

impl ChannelManager {
    pub fn new(max_buf_size: usize, is_initiator: bool, max_channels: usize) -> Self {
        let (local_base, peer_base) = if is_initiator { (0, 1) } else { (1, 0) };
        Self {
            channels: HashMap::new(),
            local_next_id: local_base,
            peer_next_id: peer_base,
            peer_base,
            max_buf_size,
            updated: BTreeSet::new(),
            max_channels,
            local_count: 0,
            peer_count: 0,
            pending_opens: Vec::new(),
            pending_closes: Vec::new(),
        }
    }

    /// Send a message on a channel.  Returns the assigned message_id.
    /// `deadline`: message is dropped if not fully emitted by this time.
    /// If the send buffer exceeds the limit, the oldest unsent message is evicted.
    pub fn send(
        &mut self,
        channel_id: u64,
        data: Vec<u8>,
        deadline: Instant,
    ) -> Result<u64, ChannelError> {
        let data_len = data.len();
        let channel = self.get_or_create(channel_id)?;
        channel.evict_send(data_len);
        let message_id = channel.next_message_id;
        channel.next_message_id += 1;
        channel.send_buf_size += data_len;
        channel.send.insert(
            message_id,
            SendEntry {
                frag: MessageFragmenter::new(data),
                deadline,
            },
        );
        Ok(message_id)
    }

    /// Receive a fragment from the network.
    /// If the assembler limit is reached, the oldest incomplete message is evicted.
    pub fn recv(
        &mut self,
        channel_id: u64,
        message_id: u64,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<(), ChannelError> {
        let channel = self.get_or_create(channel_id)?;

        let assembler = channel
            .recv
            .entry(message_id)
            .or_insert_with(|| MessageAssembler::new(u64::MAX));

        let before = assembler.allocated();
        assembler.write(offset, data, fin)?;
        let after = assembler.allocated();
        channel.recv_buf_size += after - before;

        if assembler.is_complete() {
            channel.completed.insert(message_id);
        }

        // Evict oldest messages (possibly including this one) if over budget.
        channel.evict_recv(0);

        self.updated.insert(channel_id);

        Ok(())
    }

    /// Handle a received ChannelOpen frame from peer.
    /// If the channel already exists (data arrived first), this is a no-op.
    pub fn on_peer_open(&mut self, channel_id: u64) -> Result<(), ChannelError> {
        self.get_or_create(channel_id)?;
        Ok(())
    }

    /// Handle a received ChannelClose frame from peer.
    pub fn on_peer_close(&mut self, channel_id: u64) {
        if self.channels.remove(&channel_id).is_some() {
            if (channel_id % 2) == self.peer_base {
                self.peer_count = self.peer_count.saturating_sub(1);
            } else {
                self.local_count = self.local_count.saturating_sub(1);
            }
            self.updated.insert(channel_id);
        }
    }

    /// Emit the next fragment for transmission.
    /// Drops expired messages before emitting.
    /// Returns `(channel_id, message_id, offset, len, fin)`.
    pub fn emit(&mut self, out: &mut [u8], now: Instant) -> Option<(u64, u64, u64, usize, bool)> {
        if out.is_empty() {
            return None;
        }

        let ids: Vec<u64> = self.channels.keys().copied().collect();
        for &channel_id in &ids {
            let channel = self.channels.get_mut(&channel_id).unwrap();
            channel.expire_send(now);

            let msg_ids: Vec<u64> = channel.send.keys().copied().collect();
            for &message_id in &msg_ids {
                let entry = channel.send.get_mut(&message_id).unwrap();
                if let Some((offset, len, fin)) = entry.frag.emit(out) {
                    return Some((channel_id, message_id, offset, len, fin));
                }
            }
        }

        None
    }

    /// Poll for a completed (reassembled) message on a channel.
    pub fn poll(&mut self, channel_id: u64) -> Option<Vec<u8>> {
        let channel = self.channels.get_mut(&channel_id)?;
        let &message_id = channel.completed.first()?;
        channel.completed.remove(&message_id);
        let assembler = channel.recv.remove(&message_id).unwrap();
        channel.recv_buf_size -= assembler.allocated();
        Some(assembler.take())
    }

    /// Acknowledge a fragment.  Cleans up completed fragmenters.
    pub fn ack(&mut self, channel_id: u64, message_id: u64, _offset: u64, _len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get(&message_id) {
                // is_done() already checks retransmits.is_empty().
                if entry.frag.is_done() {
                    let len = entry.frag.len() as usize;
                    channel.send.remove(&message_id);
                    channel.send_buf_size -= len;
                }
            }
        }
    }

    /// A fragment was lost, needs retransmission.
    pub fn loss(&mut self, channel_id: u64, message_id: u64, offset: u64, len: usize) {
        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if let Some(entry) = channel.send.get_mut(&message_id) {
                entry.frag.loss(offset, len);
            }
        }
    }

    /// Close a channel, dropping all in-flight send/recv data.
    /// For local channels, queues a ChannelClose frame for reliable delivery.
    /// The local_count is NOT decremented here — call `ack_close()` when the
    /// peer ACKs the ChannelClose to free the slot.
    pub fn close(&mut self, channel_id: u64) -> bool {
        if self.channels.remove(&channel_id).is_some() {
            let is_peer = (channel_id % 2) == self.peer_base;
            if !is_peer {
                self.pending_closes.push(channel_id);
            }
            // peer_count is not decremented here either — on_peer_close handles that.
            true
        } else {
            false
        }
    }

    /// Called when the peer ACKs a ChannelClose we sent.
    /// Decrements local_count so a new channel can be opened.
    pub fn ack_close(&mut self, channel_id: u64) {
        let is_peer = (channel_id % 2) == self.peer_base;
        if !is_peer {
            self.local_count = self.local_count.saturating_sub(1);
        }
    }

    /// True if any channel has fragments to emit.
    pub fn has_pending(&self) -> bool {
        self.channels.values().any(|ch| {
            ch.send
                .values()
                .any(|e| !e.frag.is_done() || e.frag.has_retransmits())
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

    /// Drain channel IDs whose state changed since last call.
    pub fn drain_updated(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.updated).into_iter().collect()
    }

    /// Drain pending ChannelOpen frames for transmission.
    pub fn drain_pending_opens(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_opens)
    }

    /// Drain pending ChannelClose frames for transmission.
    pub fn drain_pending_closes(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_closes)
    }

    /// Re-queue a ChannelOpen for retransmission (on loss).
    pub fn requeue_open(&mut self, channel_id: u64) {
        self.pending_opens.push(channel_id);
    }

    /// Re-queue a ChannelClose for retransmission (on loss).
    pub fn requeue_close(&mut self, channel_id: u64) {
        self.pending_closes.push(channel_id);
    }

    fn get_or_create(&mut self, channel_id: u64) -> Result<&mut Channel, ChannelError> {
        if self.channels.contains_key(&channel_id) {
            return Ok(self.channels.get_mut(&channel_id).unwrap());
        }

        let is_peer = (channel_id % 2) == self.peer_base;

        if is_peer {
            if channel_id < self.peer_next_id {
                return Err(ChannelError::IdReused);
            }
            // Gap-fill all peer channels up to channel_id.
            let mut id = self.peer_next_id;
            while id <= channel_id {
                if self.peer_count >= self.max_channels {
                    return Err(ChannelError::TooManyChannels);
                }
                self.channels.insert(id, Channel::new(self.max_buf_size));
                self.peer_count += 1;
                self.updated.insert(id);
                id += 2;
            }
            self.peer_next_id = channel_id + 2;
        } else {
            if channel_id < self.local_next_id {
                return Err(ChannelError::IdReused);
            }
            // Gap-fill all local channels up to channel_id.
            let mut id = self.local_next_id;
            while id <= channel_id {
                if self.local_count >= self.max_channels {
                    return Err(ChannelError::TooManyChannels);
                }
                self.channels.insert(id, Channel::new(self.max_buf_size));
                self.local_count += 1;
                self.pending_opens.push(id);
                id += 2;
            }
            self.local_next_id = channel_id + 2;
        }

        Ok(self.channels.get_mut(&channel_id).unwrap())
    }
}
