use std::collections::{BTreeMap, VecDeque};

use crate::v2::wire::StreamData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvResult {
    Received,
    Duplicate,
}

pub struct ReliableSendStream {
    stream_id: u32,
    send_offset: u64,
    remote_max_offset: u64,
    pending: VecDeque<u8>,
    fin_queued: bool,
    fin_sent: bool,
}

impl ReliableSendStream {
    pub fn new(stream_id: u32, remote_max_offset: u64) -> Self {
        Self {
            stream_id,
            send_offset: 0,
            remote_max_offset,
            pending: VecDeque::new(),
            fin_queued: false,
            fin_sent: false,
        }
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn send_offset(&self) -> u64 {
        self.send_offset
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_fin_sent(&self) -> bool {
        self.fin_sent
    }

    pub fn send_window(&self) -> u64 {
        self.remote_max_offset.saturating_sub(self.send_offset)
    }

    pub fn can_send(&self) -> bool {
        if self.fin_sent {
            return false;
        }
        if self.fin_queued && self.pending.is_empty() {
            return true;
        }
        !self.pending.is_empty() && self.send_window() > 0
    }

    pub fn write(&mut self, data: &[u8]) {
        self.pending.extend(data);
    }

    pub fn write_fin(&mut self) {
        self.fin_queued = true;
    }

    pub fn on_window_update(&mut self, max_offset: u64) {
        if max_offset > self.remote_max_offset {
            self.remote_max_offset = max_offset;
        }
    }

    pub fn poll_frame(&mut self, max_data: usize) -> Option<StreamData> {
        if self.fin_sent {
            return None;
        }

        let window = self.send_window() as usize;
        let available = max_data.min(window).min(self.pending.len());

        let is_fin = self.fin_queued && available == self.pending.len();

        if available == 0 && !is_fin {
            return None;
        }

        let data: Vec<u8> = self.pending.drain(..available).collect();
        let offset = self.send_offset;
        self.send_offset += data.len() as u64;

        if is_fin {
            self.fin_sent = true;
        }

        Some(StreamData {
            stream_id: self.stream_id,
            offset,
            fin: is_fin,
            data,
        })
    }
}

pub struct ReliableRecvStream {
    stream_id: u32,
    read_offset: u64,
    buffer: BTreeMap<u64, Vec<u8>>,
    fin_offset: Option<u64>,
}

impl ReliableRecvStream {
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            read_offset: 0,
            buffer: BTreeMap::new(),
            fin_offset: None,
        }
    }

    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub fn read_offset(&self) -> u64 {
        self.read_offset
    }

    pub fn is_finished(&self) -> bool {
        self.buffer.is_empty() && self.fin_offset == Some(self.read_offset)
    }

    pub fn on_data(&mut self, offset: u64, data: Vec<u8>, fin: bool) -> RecvResult {
        let end = offset + data.len() as u64;

        if fin {
            self.fin_offset = Some(end);
        }

        if end <= self.read_offset {
            return RecvResult::Duplicate;
        }

        let trimmed_start = offset.max(self.read_offset);
        let skip = (trimmed_start - offset) as usize;
        let trimmed = data[skip..].to_vec();

        if !trimmed.is_empty() {
            self.buffer.entry(trimmed_start).or_insert(trimmed);
        }

        RecvResult::Received
    }

    pub fn read(&mut self) -> Option<Vec<u8>> {
        let mut result = Vec::new();

        while let Some(entry) = self.buffer.first_entry() {
            let &offset = entry.key();
            if offset > self.read_offset {
                break;
            }
            let data = entry.remove();
            let skip = (self.read_offset - offset) as usize;
            if skip < data.len() {
                self.read_offset += (data.len() - skip) as u64;
                result.extend_from_slice(&data[skip..]);
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}
