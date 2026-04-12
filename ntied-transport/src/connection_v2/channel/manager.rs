use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use super::message::{AssemblerError, MessageAssembler, MessageFragmenter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    IdReused,
    UnknownChannel,
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
/// Channels are created lazily on first `send()` or `recv()`.
/// When the number of in-progress assemblers exceeds `max_assemblers`,
/// the oldest incomplete message is evicted (datagram semantics).
/// `close()` drops all in-flight data and removes the channel.
pub struct ChannelManager {
    pub(super) channels: HashMap<u64, Channel>,
    next_id: u64,
    max_buf_size: usize,
}

impl ChannelManager {
    pub fn new(max_buf_size: usize) -> Self {
        Self {
            channels: HashMap::new(),
            next_id: 0,
            max_buf_size,
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

        Ok(())
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
                if entry.frag.is_done() && !entry.frag.has_retransmits() {
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
    pub fn close(&mut self, channel_id: u64) -> bool {
        self.channels.remove(&channel_id).is_some()
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

    fn get_or_create(&mut self, channel_id: u64) -> Result<&mut Channel, ChannelError> {
        if self.channels.contains_key(&channel_id) {
            return Ok(self.channels.get_mut(&channel_id).unwrap());
        }
        if channel_id < self.next_id {
            return Err(ChannelError::IdReused);
        }
        self.next_id = channel_id + 1;
        self.channels
            .insert(channel_id, Channel::new(self.max_buf_size));
        Ok(self.channels.get_mut(&channel_id).unwrap())
    }
}

