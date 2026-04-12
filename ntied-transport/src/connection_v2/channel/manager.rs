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

struct SendEntry {
    frag: MessageFragmenter,
    deadline: Instant,
}

struct Channel {
    send: BTreeMap<u64, SendEntry>,
    recv: BTreeMap<u64, MessageAssembler>,
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
    channels: HashMap<u64, Channel>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn now() -> Instant {
        Instant::now()
    }

    fn far_future() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    #[test]
    fn send_and_emit() {
        let mut mgr = ChannelManager::new(65536);
        let msg_id = mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        assert_eq!(msg_id, 0);

        let mut out = [0u8; 100];
        let (ch, msg, off, len, fin) = mgr.emit(&mut out, now()).unwrap();
        assert_eq!((ch, msg, off, len), (0, 0, 0, 5));
        assert!(fin);

        assert!(mgr.emit(&mut out, now()).is_none());
    }

    #[test]
    fn recv_and_poll() {
        let mut mgr = ChannelManager::new(65536);
        mgr.recv(0, 0, 0, b"hello", true).unwrap();

        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"hello");

        assert!(mgr.poll(0).is_none());
    }

    #[test]
    fn recv_fragmented() {
        let mut mgr = ChannelManager::new(65536);
        mgr.recv(0, 0, 0, b"hel", false).unwrap();
        assert!(mgr.poll(0).is_none());

        mgr.recv(0, 0, 3, b"lo", true).unwrap();
        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"hello");
    }

    #[test]
    fn recv_out_of_order() {
        let mut mgr = ChannelManager::new(65536);
        mgr.recv(0, 0, 5, b"world", true).unwrap();
        mgr.recv(0, 0, 0, b"hello", false).unwrap();

        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"helloworld");
    }

    #[test]
    fn multiple_messages() {
        let mut mgr = ChannelManager::new(65536);
        let id0 = mgr.send(0, b"first".to_vec(), far_future()).unwrap();
        let id1 = mgr.send(0, b"second".to_vec(), far_future()).unwrap();
        assert_ne!(id0, id1);

        let mut out = [0u8; 100];
        let mut emitted = Vec::new();
        while let Some(frag) = mgr.emit(&mut out, now()) {
            emitted.push(frag);
        }
        assert_eq!(emitted.len(), 2);
    }

    #[test]
    fn loss_retransmit() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(0, b"ABCDEFGHIJ".to_vec(), far_future()).unwrap();

        let mut out = [0u8; 5];
        let (_, msg_id, off1, _, _) = mgr.emit(&mut out, now()).unwrap();
        let (_, _, _, _, fin) = mgr.emit(&mut out, now()).unwrap();
        assert!(fin);
        assert!(mgr.emit(&mut out, now()).is_none());

        mgr.loss(0, msg_id, off1, 5);
        let (_, _, off, len, _) = mgr.emit(&mut out, now()).unwrap();
        assert_eq!((off, len), (0, 5));
        assert_eq!(&out, b"ABCDE");
    }

    #[test]
    fn close_channel() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        mgr.recv(0, 99, 0, b"incoming", false).unwrap();

        assert!(mgr.close(0));
        assert!(!mgr.channels.contains_key(&0));

        assert!(!mgr.close(0));
    }

    #[test]
    fn close_reuse_rejected() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(5, b"hi".to_vec(), far_future()).unwrap();
        mgr.close(5);

        assert_eq!(
            mgr.send(3, b"hi".to_vec(), far_future()),
            Err(ChannelError::IdReused)
        );
    }

    #[test]
    fn recv_too_large_evicts() {
        // Buffer can hold 10 bytes total.
        let mut mgr = ChannelManager::new(10);
        // First message: 6 bytes.
        mgr.recv(0, 0, 0, b"aaaaaa", false).unwrap();
        // Second message: 6 bytes. Total would be 12 > 10. Evicts first.
        mgr.recv(0, 1, 0, b"bbbbbb", false).unwrap();
        assert!(!mgr.channels[&0].recv.contains_key(&0));
        assert!(mgr.channels[&0].recv.contains_key(&1));
    }

    #[test]
    fn readable_channels() {
        let mut mgr = ChannelManager::new(65536);
        mgr.recv(0, 0, 0, b"msg", true).unwrap();
        mgr.send(1, b"out".to_vec(), far_future()).unwrap();

        let readable: Vec<u64> = mgr.readable_channels().collect();
        assert!(readable.contains(&0));
        assert!(!readable.contains(&1));
    }

    #[test]
    fn emit_empty_buf() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        assert!(mgr.emit(&mut [], now()).is_none());
    }

    #[test]
    fn loss_unknown_channel() {
        let mut mgr = ChannelManager::new(65536);
        mgr.loss(99, 0, 0, 5); // no panic
    }

    #[test]
    fn loss_unknown_message() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        mgr.loss(0, 99, 0, 5); // no panic
    }

    #[test]
    fn has_pending() {
        let mut mgr = ChannelManager::new(65536);
        assert!(!mgr.has_pending());

        mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        assert!(mgr.has_pending());

        let mut out = [0u8; 100];
        mgr.emit(&mut out, now());
        assert!(!mgr.has_pending());
    }

    #[test]
    fn recv_creates_channel() {
        let mut mgr = ChannelManager::new(65536);
        mgr.recv(5, 0, 0, b"data", true).unwrap();
        assert!(mgr.channels.contains_key(&5));
    }

    #[test]
    fn multiple_channels() {
        let mut mgr = ChannelManager::new(65536);
        mgr.send(0, b"ch0".to_vec(), far_future()).unwrap();
        mgr.send(1, b"ch1".to_vec(), far_future()).unwrap();

        let mut out = [0u8; 100];
        let mut channel_ids = Vec::new();
        while let Some((ch, _, _, _, _)) = mgr.emit(&mut out, now()) {
            channel_ids.push(ch);
        }
        assert_eq!(channel_ids.len(), 2);
        assert!(channel_ids.contains(&0));
        assert!(channel_ids.contains(&1));
    }

    #[test]
    fn roundtrip() {
        let mut sender = ChannelManager::new(65536);
        let mut receiver = ChannelManager::new(65536);

        sender
            .send(0, b"hello world!".to_vec(), far_future())
            .unwrap();

        let mut buf = [0u8; 5];
        while let Some((ch, msg, off, len, fin)) = sender.emit(&mut buf, now()) {
            receiver.recv(ch, msg, off, &buf[..len], fin).unwrap();
        }

        let msg = receiver.poll(0).unwrap();
        assert_eq!(msg, b"hello world!");
    }

    #[test]
    fn eviction_oldest_dropped() {
        // Buffer holds 8 bytes. 3 messages of 3 bytes each = 9 > 8.
        let mut mgr = ChannelManager::new(8);

        mgr.recv(0, 10, 0, b"aaa", false).unwrap();
        mgr.recv(0, 20, 0, b"bbb", false).unwrap();
        assert_eq!(mgr.channels[&0].recv.len(), 2);

        // 3rd message needs 3 bytes. Total would be 9 > 8. Evicts oldest (10).
        mgr.recv(0, 30, 0, b"ccc", false).unwrap();
        assert!(!mgr.channels[&0].recv.contains_key(&10));
        assert!(mgr.channels[&0].recv.contains_key(&20));
        assert!(mgr.channels[&0].recv.contains_key(&30));
    }

    #[test]
    fn eviction_does_not_affect_existing_message() {
        let mut mgr = ChannelManager::new(65536);

        mgr.recv(0, 10, 0, b"aaa", false).unwrap();
        mgr.recv(0, 20, 0, b"bbb", false).unwrap();

        // Writing to existing message_id=10 completes it.
        mgr.recv(0, 10, 3, b"ddd", true).unwrap();
        // Both still in recv (10 completed but not polled, 20 incomplete).
        assert_eq!(mgr.channels[&0].recv.len(), 2);
        let msg = mgr.poll(0).unwrap();
        assert_eq!(&msg[..6], b"aaaddd");
        // Now 10 removed by poll.
        assert_eq!(mgr.channels[&0].recv.len(), 1);
    }

    #[test]
    fn eviction_current_message_can_be_dropped() {
        // Buffer holds 5 bytes. One message of 10 bytes exceeds limit.
        let mut mgr = ChannelManager::new(5);
        // First fragment grows assembler to 10 bytes (offset=5, data=5 → resize to 10).
        mgr.recv(0, 0, 5, b"world", false).unwrap();
        // Assembler is 10 bytes > max 5 → evicted (including this message).
        assert!(mgr.channels[&0].recv.is_empty());
    }

    #[test]
    fn completed_evicted_when_full() {
        // Buffer holds 10 bytes.
        let mut mgr = ChannelManager::new(10);

        // Complete a 6-byte message (stays in completed, counts in budget).
        mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
        assert!(mgr.poll(0).is_none() == false); // read it? no, leave it
        // Oops, poll consumed it. Let's redo without polling.

        let mut mgr = ChannelManager::new(10);
        mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
        // Don't poll — completed has 6 bytes in budget.

        // New message: 6 bytes. Total would be 12 > 10.
        // No assemblers to evict → evicts oldest completed.
        mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();

        // First completed message was dropped.
        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"bbbbbb");
        assert!(mgr.poll(0).is_none());
    }

    #[test]
    fn poll_frees_budget() {
        let mut mgr = ChannelManager::new(10);
        mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();

        // Poll frees budget.
        let _ = mgr.poll(0).unwrap();

        // Now 6 more bytes fit without eviction.
        mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();
        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"bbbbbb");
    }

    #[test]
    fn completed_evicted_before_poll() {
        let mut mgr = ChannelManager::new(10);

        mgr.recv(0, 0, 0, b"aaaaaa", true).unwrap();
        // Don't poll — msg 0 in completed + recv.

        // New 6-byte msg exceeds budget → evicts msg 0 from recv AND completed.
        mgr.recv(0, 1, 0, b"bbbbbb", true).unwrap();

        // Msg 0 was evicted. Only msg 1 available.
        let msg = mgr.poll(0).unwrap();
        assert_eq!(msg, b"bbbbbb");
        assert!(mgr.poll(0).is_none());
    }

    #[test]
    fn send_eviction_empties_map() {
        // Buffer holds 3 bytes. Message is 5 bytes — eviction empties map, still inserts.
        let mut mgr = ChannelManager::new(3);
        mgr.send(0, b"hello".to_vec(), far_future()).unwrap();
        // Message exceeds limit but nothing to evict → inserted anyway.
        assert_eq!(mgr.channels[&0].send.len(), 1);
    }

    #[test]
    fn ttl_expiration() {
        let mut mgr = ChannelManager::new(65536);
        let past = Instant::now(); // deadline in the past
        mgr.send(0, b"expired".to_vec(), past).unwrap();

        // Sleep to ensure now > deadline.
        std::thread::sleep(Duration::from_millis(1));

        let mut out = [0u8; 100];
        // emit expires the message — returns None.
        assert!(mgr.emit(&mut out, Instant::now()).is_none());
        assert!(!mgr.has_pending());
    }

    #[test]
    fn would_evict_check() {
        let mut mgr = ChannelManager::new(10);
        mgr.send(0, b"aaaa".to_vec(), far_future()).unwrap();
        mgr.send(0, b"bbbb".to_vec(), far_future()).unwrap();
        // 8 bytes used. 4 more would be 12 > 10 → eviction.
        assert!(mgr.would_evict(0, 4));
        // 2 more would be 10 = 10 → no eviction.
        assert!(!mgr.would_evict(0, 2));
        // Unknown channel → no eviction (will be created).
        assert!(!mgr.would_evict(99, 100));
    }

    #[test]
    fn send_eviction() {
        // Buffer holds 10 bytes.
        let mut mgr = ChannelManager::new(10);
        mgr.send(0, b"aaaa".to_vec(), far_future()).unwrap(); // 4 bytes
        mgr.send(0, b"bbbb".to_vec(), far_future()).unwrap(); // 4 bytes, total 8
        // 3rd message: 4 bytes, total would be 12 > 10. Evicts oldest.
        mgr.send(0, b"cccc".to_vec(), far_future()).unwrap();
        assert!(!mgr.channels[&0].send.contains_key(&0)); // msg 0 evicted
        assert!(mgr.channels[&0].send.contains_key(&1));
        assert!(mgr.channels[&0].send.contains_key(&2));
    }
}
