use std::collections::{HashMap, VecDeque};

use crate::wire::{
    ChannelClose, StreamData, ChannelOpen, ChannelReset, ChannelType, DatagramFragment,
    WindowUpdate,
};

use super::{DatagramReceiver, DatagramSender, StreamReceiver, StreamSender};

pub const DEFAULT_CHANNEL_WINDOW: u64 = 65536;
const INITIATOR_FIRST_ID: u32 = 1;
const RESPONDER_FIRST_ID: u32 = 2;
const ID_STEP: u32 = 2;
const MAX_FRAGMENT_DATA: usize = 1100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    UnknownChannel,
    ChannelClosed,
    ChannelReset,
    WrongChannelKind,
    MessageTooLarge,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownChannel => f.write_str("unknown channel"),
            Self::ChannelClosed => f.write_str("channel closed"),
            Self::ChannelReset => f.write_str("channel reset"),
            Self::WrongChannelKind => f.write_str("wrong channel kind"),
            Self::MessageTooLarge => f.write_str("message too large"),
        }
    }
}

impl std::error::Error for ChannelError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelState {
    Open,
    Closed,
    Reset,
}

enum ChannelKind {
    Reliable {
        send: StreamSender,
        recv: StreamReceiver,
    },
    Datagram {
        send: DatagramSender,
        recv: DatagramReceiver,
    },
}

struct ChannelEntry {
    purpose: u16,
    state: ChannelState,
    kind: ChannelKind,
}

pub struct ChannelManager {
    channels: HashMap<u32, ChannelEntry>,
    next_local_id: u32,
    pending_accept: VecDeque<u32>,
}

impl ChannelManager {
    pub fn new(is_initiator: bool) -> Self {
        Self {
            channels: HashMap::new(),
            next_local_id: if is_initiator {
                INITIATOR_FIRST_ID
            } else {
                RESPONDER_FIRST_ID
            },
            pending_accept: VecDeque::new(),
        }
    }

    pub fn open(&mut self, purpose: u16) -> (u32, ChannelOpen) {
        let channel_id = self.alloc_id();

        self.channels.insert(
            channel_id,
            ChannelEntry {
                purpose,
                state: ChannelState::Open,
                kind: ChannelKind::Reliable {
                    send: StreamSender::new(channel_id, DEFAULT_CHANNEL_WINDOW),
                    recv: StreamReceiver::new(channel_id),
                },
            },
        );

        let frame = ChannelOpen {
            channel_id,
            channel_type: ChannelType::ReliableOrdered,
            purpose,
        };

        (channel_id, frame)
    }

    pub fn open_datagram(&mut self, purpose: u16) -> (u32, ChannelOpen) {
        let channel_id = self.alloc_id();

        self.channels.insert(
            channel_id,
            ChannelEntry {
                purpose,
                state: ChannelState::Open,
                kind: ChannelKind::Datagram {
                    send: DatagramSender::new(channel_id),
                    recv: DatagramReceiver::new(channel_id),
                },
            },
        );

        let frame = ChannelOpen {
            channel_id,
            channel_type: ChannelType::ReliableDatagram,
            purpose,
        };

        (channel_id, frame)
    }

    pub fn on_channel_open(&mut self, open: ChannelOpen) -> bool {
        if self.channels.contains_key(&open.channel_id) {
            return false;
        }

        let kind = match open.channel_type {
            ChannelType::ReliableOrdered => ChannelKind::Reliable {
                send: StreamSender::new(open.channel_id, DEFAULT_CHANNEL_WINDOW),
                recv: StreamReceiver::new(open.channel_id),
            },
            ChannelType::ReliableDatagram | ChannelType::Unreliable => ChannelKind::Datagram {
                send: DatagramSender::new(open.channel_id),
                recv: DatagramReceiver::new(open.channel_id),
            },
        };

        self.channels.insert(
            open.channel_id,
            ChannelEntry {
                purpose: open.purpose,
                state: ChannelState::Open,
                kind,
            },
        );

        self.pending_accept.push_back(open.channel_id);
        true
    }

    pub fn accept(&mut self) -> Option<(u32, u16)> {
        let channel_id = self.pending_accept.pop_front()?;
        let entry = self.channels.get(&channel_id)?;
        Some((channel_id, entry.purpose))
    }

    pub fn write(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        let entry = self.get_open_mut(channel_id)?;
        match &mut entry.kind {
            ChannelKind::Reliable { send, .. } => {
                send.write(data);
                Ok(())
            }
            ChannelKind::Datagram { .. } => Err(ChannelError::WrongChannelKind),
        }
    }

    pub fn write_fin(&mut self, channel_id: u32) -> Result<(), ChannelError> {
        let entry = self.get_open_mut(channel_id)?;
        match &mut entry.kind {
            ChannelKind::Reliable { send, .. } => {
                send.write_fin();
                Ok(())
            }
            ChannelKind::Datagram { .. } => Err(ChannelError::WrongChannelKind),
        }
    }

    pub fn read(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        let entry = self
            .channels
            .get_mut(&channel_id)
            .ok_or(ChannelError::UnknownChannel)?;
        if entry.state == ChannelState::Reset {
            return Err(ChannelError::ChannelReset);
        }
        match &mut entry.kind {
            ChannelKind::Reliable { recv, .. } => Ok(recv.read()),
            ChannelKind::Datagram { .. } => Err(ChannelError::WrongChannelKind),
        }
    }

    pub fn write_datagram(&mut self, channel_id: u32, data: &[u8]) -> Result<(), ChannelError> {
        let entry = self.get_open_mut(channel_id)?;
        match &mut entry.kind {
            ChannelKind::Datagram { send, .. } => {
                if send.write(data, MAX_FRAGMENT_DATA) {
                    Ok(())
                } else {
                    Err(ChannelError::MessageTooLarge)
                }
            }
            ChannelKind::Reliable { .. } => Err(ChannelError::WrongChannelKind),
        }
    }

    pub fn read_datagram(&mut self, channel_id: u32) -> Result<Option<Vec<u8>>, ChannelError> {
        let entry = self
            .channels
            .get_mut(&channel_id)
            .ok_or(ChannelError::UnknownChannel)?;
        if entry.state == ChannelState::Reset {
            return Err(ChannelError::ChannelReset);
        }
        match &mut entry.kind {
            ChannelKind::Datagram { recv, .. } => Ok(recv.recv()),
            ChannelKind::Reliable { .. } => Err(ChannelError::WrongChannelKind),
        }
    }

    pub fn close(&mut self, channel_id: u32) -> Result<ChannelClose, ChannelError> {
        let entry = self.get_open_mut(channel_id)?;
        if let ChannelKind::Reliable { send, .. } = &mut entry.kind {
            send.write_fin();
        }
        entry.state = ChannelState::Closed;
        Ok(ChannelClose { channel_id })
    }

    pub fn on_channel_data(&mut self, data: StreamData) {
        if let Some(entry) = self.channels.get_mut(&data.channel_id) {
            if entry.state != ChannelState::Reset {
                if let ChannelKind::Reliable { recv, .. } = &mut entry.kind {
                    recv.on_data(data.offset, data.data, data.fin);
                }
            }
        }
    }

    pub fn on_datagram_fragment(&mut self, fragment: DatagramFragment) {
        if let Some(entry) = self.channels.get_mut(&fragment.channel_id) {
            if entry.state != ChannelState::Reset {
                if let ChannelKind::Datagram { recv, .. } = &mut entry.kind {
                    recv.on_fragment(fragment);
                }
            }
        }
    }

    pub fn on_window_update(&mut self, update: &WindowUpdate) {
        if let Some(entry) = self.channels.get_mut(&update.channel_id) {
            if entry.state != ChannelState::Reset {
                if let ChannelKind::Reliable { send, .. } = &mut entry.kind {
                    send.on_window_update(update.max_offset);
                }
            }
        }
    }

    pub fn on_channel_close(&mut self, close: &ChannelClose) {
        if let Some(entry) = self.channels.get_mut(&close.channel_id) {
            entry.state = ChannelState::Closed;
        }
    }

    pub fn on_channel_reset(&mut self, reset: &ChannelReset) {
        if let Some(entry) = self.channels.get_mut(&reset.channel_id) {
            entry.state = ChannelState::Reset;
        }
    }

    pub fn poll_channel_data(&mut self, max_data: usize) -> Option<StreamData> {
        for entry in self.channels.values_mut() {
            if entry.state == ChannelState::Reset {
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
        for entry in self.channels.values_mut() {
            if entry.state == ChannelState::Reset {
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
        self.channels.values().any(|e| {
            if e.state == ChannelState::Reset {
                return false;
            }
            match &e.kind {
                ChannelKind::Reliable { send, .. } => send.can_send(),
                ChannelKind::Datagram { send, .. } => send.has_pending(),
            }
        })
    }

    pub fn is_channel_finished(&self, channel_id: u32) -> bool {
        self.channels
            .get(&channel_id)
            .map_or(false, |e| match &e.kind {
                ChannelKind::Reliable { recv, .. } => recv.is_finished(),
                ChannelKind::Datagram { .. } => e.state == ChannelState::Closed,
            })
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn pending_accept_count(&self) -> usize {
        self.pending_accept.len()
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_local_id;
        self.next_local_id += ID_STEP;
        id
    }

    fn get_open_mut(&mut self, channel_id: u32) -> Result<&mut ChannelEntry, ChannelError> {
        let entry = self
            .channels
            .get_mut(&channel_id)
            .ok_or(ChannelError::UnknownChannel)?;
        match entry.state {
            ChannelState::Open => Ok(entry),
            ChannelState::Closed => Err(ChannelError::ChannelClosed),
            ChannelState::Reset => Err(ChannelError::ChannelReset),
        }
    }
}
