use std::collections::{HashMap, VecDeque};

use crate::v2::wire::{StreamClose, StreamData, StreamOpen, StreamReset, StreamType, WindowUpdate};

use super::reliable::{ReliableRecvStream, ReliableSendStream};

pub const DEFAULT_STREAM_WINDOW: u64 = 65536;
const INITIATOR_FIRST_ID: u32 = 1;
const RESPONDER_FIRST_ID: u32 = 2;
const ID_STEP: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    UnknownStream,
    StreamClosed,
    StreamReset,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStream => f.write_str("unknown stream"),
            Self::StreamClosed => f.write_str("stream closed"),
            Self::StreamReset => f.write_str("stream reset"),
        }
    }
}

impl std::error::Error for StreamError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Open,
    Closed,
    Reset,
}

struct StreamEntry {
    send: ReliableSendStream,
    recv: ReliableRecvStream,
    purpose: u16,
    state: StreamState,
}

pub struct StreamManager {
    streams: HashMap<u32, StreamEntry>,
    next_local_id: u32,
    pending_accept: VecDeque<u32>,
}

impl StreamManager {
    pub fn new(is_initiator: bool) -> Self {
        Self {
            streams: HashMap::new(),
            next_local_id: if is_initiator {
                INITIATOR_FIRST_ID
            } else {
                RESPONDER_FIRST_ID
            },
            pending_accept: VecDeque::new(),
        }
    }

    pub fn open(&mut self, purpose: u16) -> (u32, StreamOpen) {
        let stream_id = self.next_local_id;
        self.next_local_id += ID_STEP;

        self.streams.insert(
            stream_id,
            StreamEntry {
                send: ReliableSendStream::new(stream_id, DEFAULT_STREAM_WINDOW),
                recv: ReliableRecvStream::new(stream_id),
                purpose,
                state: StreamState::Open,
            },
        );

        let frame = StreamOpen {
            stream_id,
            stream_type: StreamType::ReliableOrdered,
            purpose,
        };

        (stream_id, frame)
    }

    pub fn on_stream_open(&mut self, open: StreamOpen) -> bool {
        if self.streams.contains_key(&open.stream_id) {
            return false;
        }

        self.streams.insert(
            open.stream_id,
            StreamEntry {
                send: ReliableSendStream::new(open.stream_id, DEFAULT_STREAM_WINDOW),
                recv: ReliableRecvStream::new(open.stream_id),
                purpose: open.purpose,
                state: StreamState::Open,
            },
        );

        self.pending_accept.push_back(open.stream_id);
        true
    }

    pub fn accept(&mut self) -> Option<(u32, u16)> {
        let stream_id = self.pending_accept.pop_front()?;
        let entry = self.streams.get(&stream_id)?;
        Some((stream_id, entry.purpose))
    }

    pub fn write(&mut self, stream_id: u32, data: &[u8]) -> Result<(), StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        entry.send.write(data);
        Ok(())
    }

    pub fn write_fin(&mut self, stream_id: u32) -> Result<(), StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        entry.send.write_fin();
        Ok(())
    }

    pub fn read(&mut self, stream_id: u32) -> Result<Option<Vec<u8>>, StreamError> {
        let entry = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        if entry.state == StreamState::Reset {
            return Err(StreamError::StreamReset);
        }
        Ok(entry.recv.read())
    }

    pub fn close(&mut self, stream_id: u32) -> Result<StreamClose, StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        entry.send.write_fin();
        entry.state = StreamState::Closed;
        Ok(StreamClose { stream_id })
    }

    pub fn on_stream_data(&mut self, data: StreamData) {
        if let Some(entry) = self.streams.get_mut(&data.stream_id) {
            if entry.state != StreamState::Reset {
                entry.recv.on_data(data.offset, data.data, data.fin);
            }
        }
    }

    pub fn on_window_update(&mut self, update: &WindowUpdate) {
        if let Some(entry) = self.streams.get_mut(&update.stream_id) {
            if entry.state != StreamState::Reset {
                entry.send.on_window_update(update.max_offset);
            }
        }
    }

    pub fn on_stream_close(&mut self, close: &StreamClose) {
        if let Some(entry) = self.streams.get_mut(&close.stream_id) {
            entry.state = StreamState::Closed;
        }
    }

    pub fn on_stream_reset(&mut self, reset: &StreamReset) {
        if let Some(entry) = self.streams.get_mut(&reset.stream_id) {
            entry.state = StreamState::Reset;
        }
    }

    pub fn poll_stream_data(&mut self, max_data: usize) -> Option<StreamData> {
        for entry in self.streams.values_mut() {
            if entry.state == StreamState::Reset {
                continue;
            }
            if let Some(frame) = entry.send.poll_frame(max_data) {
                return Some(frame);
            }
        }
        None
    }

    pub fn has_pending_data(&self) -> bool {
        self.streams
            .values()
            .any(|e| e.state != StreamState::Reset && e.send.can_send())
    }

    pub fn is_stream_finished(&self, stream_id: u32) -> bool {
        self.streams
            .get(&stream_id)
            .map_or(false, |e| e.recv.is_finished())
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn pending_accept_count(&self) -> usize {
        self.pending_accept.len()
    }

    fn get_open_mut(&mut self, stream_id: u32) -> Result<&mut StreamEntry, StreamError> {
        let entry = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        match entry.state {
            StreamState::Open => Ok(entry),
            StreamState::Closed => Err(StreamError::StreamClosed),
            StreamState::Reset => Err(StreamError::StreamReset),
        }
    }
}
