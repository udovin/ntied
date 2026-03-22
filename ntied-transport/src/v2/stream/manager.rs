use std::collections::{HashMap, VecDeque};

use crate::v2::wire::{
    DatagramFragment, StreamClose, StreamData, StreamOpen, StreamReset, StreamType, WindowUpdate,
};

use super::datagram::{DatagramReceiver, DatagramSender};
use super::reliable::{ReliableRecvStream, ReliableSendStream};

pub const DEFAULT_STREAM_WINDOW: u64 = 65536;
const INITIATOR_FIRST_ID: u32 = 1;
const RESPONDER_FIRST_ID: u32 = 2;
const ID_STEP: u32 = 2;
const MAX_FRAGMENT_DATA: usize = 1100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    UnknownStream,
    StreamClosed,
    StreamReset,
    WrongChannelKind,
    MessageTooLarge,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStream => f.write_str("unknown stream"),
            Self::StreamClosed => f.write_str("stream closed"),
            Self::StreamReset => f.write_str("stream reset"),
            Self::WrongChannelKind => f.write_str("wrong channel kind"),
            Self::MessageTooLarge => f.write_str("message too large"),
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

enum ChannelKind {
    Reliable {
        send: ReliableSendStream,
        recv: ReliableRecvStream,
    },
    Datagram {
        send: DatagramSender,
        recv: DatagramReceiver,
    },
}

struct StreamEntry {
    purpose: u16,
    state: StreamState,
    kind: ChannelKind,
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
        let stream_id = self.alloc_id();

        self.streams.insert(
            stream_id,
            StreamEntry {
                purpose,
                state: StreamState::Open,
                kind: ChannelKind::Reliable {
                    send: ReliableSendStream::new(stream_id, DEFAULT_STREAM_WINDOW),
                    recv: ReliableRecvStream::new(stream_id),
                },
            },
        );

        let frame = StreamOpen {
            stream_id,
            stream_type: StreamType::ReliableOrdered,
            purpose,
        };

        (stream_id, frame)
    }

    pub fn open_datagram(&mut self, purpose: u16) -> (u32, StreamOpen) {
        let stream_id = self.alloc_id();

        self.streams.insert(
            stream_id,
            StreamEntry {
                purpose,
                state: StreamState::Open,
                kind: ChannelKind::Datagram {
                    send: DatagramSender::new(stream_id),
                    recv: DatagramReceiver::new(stream_id),
                },
            },
        );

        let frame = StreamOpen {
            stream_id,
            stream_type: StreamType::ReliableDatagram,
            purpose,
        };

        (stream_id, frame)
    }

    pub fn on_stream_open(&mut self, open: StreamOpen) -> bool {
        if self.streams.contains_key(&open.stream_id) {
            return false;
        }

        let kind = match open.stream_type {
            StreamType::ReliableOrdered => ChannelKind::Reliable {
                send: ReliableSendStream::new(open.stream_id, DEFAULT_STREAM_WINDOW),
                recv: ReliableRecvStream::new(open.stream_id),
            },
            StreamType::ReliableDatagram | StreamType::Unreliable => ChannelKind::Datagram {
                send: DatagramSender::new(open.stream_id),
                recv: DatagramReceiver::new(open.stream_id),
            },
        };

        self.streams.insert(
            open.stream_id,
            StreamEntry {
                purpose: open.purpose,
                state: StreamState::Open,
                kind,
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
        match &mut entry.kind {
            ChannelKind::Reliable { send, .. } => {
                send.write(data);
                Ok(())
            }
            ChannelKind::Datagram { .. } => Err(StreamError::WrongChannelKind),
        }
    }

    pub fn write_fin(&mut self, stream_id: u32) -> Result<(), StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        match &mut entry.kind {
            ChannelKind::Reliable { send, .. } => {
                send.write_fin();
                Ok(())
            }
            ChannelKind::Datagram { .. } => Err(StreamError::WrongChannelKind),
        }
    }

    pub fn read(&mut self, stream_id: u32) -> Result<Option<Vec<u8>>, StreamError> {
        let entry = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        if entry.state == StreamState::Reset {
            return Err(StreamError::StreamReset);
        }
        match &mut entry.kind {
            ChannelKind::Reliable { recv, .. } => Ok(recv.read()),
            ChannelKind::Datagram { .. } => Err(StreamError::WrongChannelKind),
        }
    }

    pub fn write_datagram(&mut self, stream_id: u32, data: &[u8]) -> Result<(), StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        match &mut entry.kind {
            ChannelKind::Datagram { send, .. } => {
                if send.write(data, MAX_FRAGMENT_DATA) {
                    Ok(())
                } else {
                    Err(StreamError::MessageTooLarge)
                }
            }
            ChannelKind::Reliable { .. } => Err(StreamError::WrongChannelKind),
        }
    }

    pub fn read_datagram(&mut self, stream_id: u32) -> Result<Option<Vec<u8>>, StreamError> {
        let entry = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        if entry.state == StreamState::Reset {
            return Err(StreamError::StreamReset);
        }
        match &mut entry.kind {
            ChannelKind::Datagram { recv, .. } => Ok(recv.recv()),
            ChannelKind::Reliable { .. } => Err(StreamError::WrongChannelKind),
        }
    }

    pub fn close(&mut self, stream_id: u32) -> Result<StreamClose, StreamError> {
        let entry = self.get_open_mut(stream_id)?;
        if let ChannelKind::Reliable { send, .. } = &mut entry.kind {
            send.write_fin();
        }
        entry.state = StreamState::Closed;
        Ok(StreamClose { stream_id })
    }

    pub fn on_stream_data(&mut self, data: StreamData) {
        if let Some(entry) = self.streams.get_mut(&data.stream_id) {
            if entry.state != StreamState::Reset {
                if let ChannelKind::Reliable { recv, .. } = &mut entry.kind {
                    recv.on_data(data.offset, data.data, data.fin);
                }
            }
        }
    }

    pub fn on_datagram_fragment(&mut self, fragment: DatagramFragment) {
        if let Some(entry) = self.streams.get_mut(&fragment.stream_id) {
            if entry.state != StreamState::Reset {
                if let ChannelKind::Datagram { recv, .. } = &mut entry.kind {
                    recv.on_fragment(fragment);
                }
            }
        }
    }

    pub fn on_window_update(&mut self, update: &WindowUpdate) {
        if let Some(entry) = self.streams.get_mut(&update.stream_id) {
            if entry.state != StreamState::Reset {
                if let ChannelKind::Reliable { send, .. } = &mut entry.kind {
                    send.on_window_update(update.max_offset);
                }
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
            if let ChannelKind::Reliable { send, .. } = &mut entry.kind {
                if let Some(frame) = send.poll_frame(max_data) {
                    return Some(frame);
                }
            }
        }
        None
    }

    pub fn poll_datagram_fragment(&mut self) -> Option<DatagramFragment> {
        for entry in self.streams.values_mut() {
            if entry.state == StreamState::Reset {
                continue;
            }
            if let ChannelKind::Datagram { send, .. } = &mut entry.kind {
                if let Some(frag) = send.poll_fragment() {
                    return Some(frag);
                }
            }
        }
        None
    }

    pub fn has_pending_data(&self) -> bool {
        self.streams.values().any(|e| {
            if e.state == StreamState::Reset {
                return false;
            }
            match &e.kind {
                ChannelKind::Reliable { send, .. } => send.can_send(),
                ChannelKind::Datagram { send, .. } => send.has_pending(),
            }
        })
    }

    pub fn is_stream_finished(&self, stream_id: u32) -> bool {
        self.streams
            .get(&stream_id)
            .map_or(false, |e| match &e.kind {
                ChannelKind::Reliable { recv, .. } => recv.is_finished(),
                ChannelKind::Datagram { .. } => e.state == StreamState::Closed,
            })
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn pending_accept_count(&self) -> usize {
        self.pending_accept.len()
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_local_id;
        self.next_local_id += ID_STEP;
        id
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
