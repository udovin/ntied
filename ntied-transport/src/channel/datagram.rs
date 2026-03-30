use std::collections::{HashMap, HashSet, VecDeque};

use crate::wire::DatagramFragment;

const MAX_DATAGRAM_MSG: usize = 256 * 1024;

pub struct DatagramSender {
    channel_id: u32,
    next_message_id: u32,
    outgoing: VecDeque<DatagramFragment>,
}

impl DatagramSender {
    pub fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            next_message_id: 1,
            outgoing: VecDeque::new(),
        }
    }

    pub fn write(&mut self, data: &[u8], max_fragment: usize) -> bool {
        if data.is_empty() || data.len() > MAX_DATAGRAM_MSG || max_fragment == 0 {
            return false;
        }

        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);

        let chunks: Vec<&[u8]> = data.chunks(max_fragment).collect();
        let total = chunks.len() as u16;

        for (i, chunk) in chunks.into_iter().enumerate() {
            self.outgoing.push_back(DatagramFragment {
                channel_id: self.channel_id,
                message_id,
                fragment_index: i as u16,
                fragment_total: total,
                data: chunk.to_vec(),
            });
        }

        true
    }

    pub fn poll_fragment(&mut self) -> Option<DatagramFragment> {
        self.outgoing.pop_front()
    }

    pub fn has_pending(&self) -> bool {
        !self.outgoing.is_empty()
    }
}

struct MessageAssembly {
    total: u16,
    fragments: HashMap<u16, Vec<u8>>,
}

impl MessageAssembly {
    fn new(total: u16) -> Self {
        Self {
            total,
            fragments: HashMap::new(),
        }
    }

    fn insert(&mut self, index: u16, data: Vec<u8>) {
        if index < self.total {
            self.fragments.entry(index).or_insert(data);
        }
    }

    fn is_complete(&self) -> bool {
        self.fragments.len() == self.total as usize
    }

    fn assemble(self) -> Vec<u8> {
        let mut indices: Vec<u16> = self.fragments.keys().copied().collect();
        indices.sort_unstable();
        let mut result = Vec::new();
        for i in indices {
            if let Some(data) = self.fragments.get(&i) {
                result.extend_from_slice(data);
            }
        }
        result
    }
}

pub struct DatagramReceiver {
    channel_id: u32,
    pending: HashMap<u32, MessageAssembly>,
    delivered: HashSet<u32>,
    completed: VecDeque<Vec<u8>>,
}

impl DatagramReceiver {
    pub fn new(channel_id: u32) -> Self {
        Self {
            channel_id,
            pending: HashMap::new(),
            delivered: HashSet::new(),
            completed: VecDeque::new(),
        }
    }

    pub fn on_fragment(&mut self, fragment: DatagramFragment) {
        if fragment.channel_id != self.channel_id {
            return;
        }
        if fragment.fragment_total == 0 {
            return;
        }
        if self.delivered.contains(&fragment.message_id) {
            return;
        }

        let assembly = self
            .pending
            .entry(fragment.message_id)
            .or_insert_with(|| MessageAssembly::new(fragment.fragment_total));

        if assembly.total != fragment.fragment_total {
            return;
        }

        assembly.insert(fragment.fragment_index, fragment.data);

        if assembly.is_complete() {
            if let Some(assembly) = self.pending.remove(&fragment.message_id) {
                self.delivered.insert(fragment.message_id);
                self.completed.push_back(assembly.assemble());
            }
        }
    }

    pub fn recv(&mut self) -> Option<Vec<u8>> {
        self.completed.pop_front()
    }

    pub fn pending_message_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fragment_roundtrip() {
        let mut sender = DatagramSender::new(1);
        let mut receiver = DatagramReceiver::new(1);

        assert!(sender.write(b"hello", 1024));
        let frag = sender.poll_fragment().unwrap();
        assert!(sender.poll_fragment().is_none());

        assert_eq!(frag.fragment_total, 1);
        assert_eq!(frag.fragment_index, 0);

        receiver.on_fragment(frag);
        assert_eq!(receiver.recv().unwrap(), b"hello");
    }

    #[test]
    fn multi_fragment_roundtrip() {
        let mut sender = DatagramSender::new(1);
        let mut receiver = DatagramReceiver::new(1);

        let data = vec![0xABu8; 300];
        assert!(sender.write(&data, 100));

        let mut fragments = Vec::new();
        while let Some(f) = sender.poll_fragment() {
            fragments.push(f);
        }
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].fragment_total, 3);

        for f in fragments {
            receiver.on_fragment(f);
        }
        assert_eq!(receiver.recv().unwrap(), data);
    }

    #[test]
    fn out_of_order_delivery() {
        let mut sender = DatagramSender::new(1);
        let mut receiver = DatagramReceiver::new(1);

        let data = vec![0xCDu8; 250];
        assert!(sender.write(&data, 100));

        let mut fragments = Vec::new();
        while let Some(f) = sender.poll_fragment() {
            fragments.push(f);
        }

        receiver.on_fragment(fragments.remove(2));
        assert!(receiver.recv().is_none());
        receiver.on_fragment(fragments.remove(0));
        assert!(receiver.recv().is_none());
        receiver.on_fragment(fragments.remove(0));
        assert_eq!(receiver.recv().unwrap(), data);
    }

    #[test]
    fn duplicate_fragment_ignored() {
        let mut sender = DatagramSender::new(1);
        let mut receiver = DatagramReceiver::new(1);

        assert!(sender.write(b"dup", 100));
        let frag = sender.poll_fragment().unwrap();
        let frag2 = DatagramFragment {
            channel_id: frag.channel_id,
            message_id: frag.message_id,
            fragment_index: frag.fragment_index,
            fragment_total: frag.fragment_total,
            data: frag.data.clone(),
        };

        receiver.on_fragment(frag);
        assert!(receiver.recv().is_some());

        receiver.on_fragment(frag2);
        assert!(receiver.recv().is_none());
    }

    #[test]
    fn multiple_messages_independent() {
        let mut sender = DatagramSender::new(1);
        let mut receiver = DatagramReceiver::new(1);

        assert!(sender.write(b"first", 100));
        assert!(sender.write(b"second", 100));

        let f1 = sender.poll_fragment().unwrap();
        let f2 = sender.poll_fragment().unwrap();

        receiver.on_fragment(f2);
        assert_eq!(receiver.recv().unwrap(), b"second");

        receiver.on_fragment(f1);
        assert_eq!(receiver.recv().unwrap(), b"first");
    }

    #[test]
    fn empty_message_rejected() {
        let mut sender = DatagramSender::new(1);
        assert!(!sender.write(b"", 100));
        assert!(!sender.has_pending());
    }

    #[test]
    fn oversized_message_rejected() {
        let mut sender = DatagramSender::new(1);
        let big = vec![0u8; MAX_DATAGRAM_MSG + 1];
        assert!(!sender.write(&big, 100));
    }

    #[test]
    fn wrong_channel_id_ignored() {
        let mut receiver = DatagramReceiver::new(5);
        receiver.on_fragment(DatagramFragment {
            channel_id: 99,
            message_id: 1,
            fragment_index: 0,
            fragment_total: 1,
            data: vec![1, 2, 3],
        });
        assert!(receiver.recv().is_none());
    }

    #[test]
    fn mismatched_total_ignored() {
        let mut receiver = DatagramReceiver::new(1);

        receiver.on_fragment(DatagramFragment {
            channel_id: 1,
            message_id: 1,
            fragment_index: 0,
            fragment_total: 2,
            data: vec![1],
        });

        receiver.on_fragment(DatagramFragment {
            channel_id: 1,
            message_id: 1,
            fragment_index: 1,
            fragment_total: 3,
            data: vec![2],
        });

        assert!(receiver.recv().is_none());
        assert_eq!(receiver.pending_message_count(), 1);
    }

    #[test]
    fn message_id_wraps() {
        let mut sender = DatagramSender::new(1);
        sender.next_message_id = u32::MAX;

        assert!(sender.write(b"a", 100));
        let f1 = sender.poll_fragment().unwrap();
        assert_eq!(f1.message_id, u32::MAX);

        assert!(sender.write(b"b", 100));
        let f2 = sender.poll_fragment().unwrap();
        assert_eq!(f2.message_id, 0);
    }

    #[test]
    fn has_pending_reflects_state() {
        let mut sender = DatagramSender::new(1);
        assert!(!sender.has_pending());

        sender.write(b"x", 100);
        assert!(sender.has_pending());

        sender.poll_fragment();
        assert!(!sender.has_pending());
    }
}
