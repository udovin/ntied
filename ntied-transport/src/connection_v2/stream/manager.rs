use std::collections::HashMap;

use super::buffer::{RecvBuf, RecvBufError, SendBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    IdReused,
    UnknownStream,
    FlowControl,
    FinalSizeMismatch,
}

impl From<RecvBufError> for StreamError {
    fn from(err: RecvBufError) -> Self {
        match err {
            RecvBufError::FlowControl => StreamError::FlowControl,
            RecvBufError::FinalSizeMismatch => StreamError::FinalSizeMismatch,
        }
    }
}

pub(super) struct Stream {
    pub(super) send: SendBuf,
    pub(super) recv: RecvBuf,
}

impl Stream {
    fn new(buf_capacity: usize) -> Self {
        Self {
            send: SendBuf::new(buf_capacity),
            recv: RecvBuf::new(buf_capacity),
        }
    }
}

/// Manages per-stream send/receive buffers.
///
/// Streams are created lazily on first `write()` or `recv()`.
///
/// Local streams (we create) use monotonic IDs via `local_next_id`.
/// Peer streams (they create) use implicit opening: when peer sends
/// stream N, all peer-side IDs from 0 to N (step 2) are considered
/// opened. If a peer stream is removed and data arrives again, it's
/// rejected because the ID is ≤ `peer_highest_id` but absent from `streams`.
pub struct StreamManager {
    pub(super) streams: HashMap<u64, Stream>,
    buf_capacity: usize,
    /// Next ID for locally-created streams.
    local_next_id: u64,
    /// Highest peer-initiated stream ID ever seen.
    /// All peer IDs from `peer_base` to `peer_highest_id` (step 2)
    /// are considered implicitly opened.
    peer_highest_id: Option<u64>,
    /// Peer stream ID base (0 for even, 1 for odd).
    peer_base: u64,
    /// Round-robin cursor for fair `send()` scheduling.
    send_cursor: u64,
}

impl StreamManager {
    /// `local_base`: starting ID for our streams (0=initiator, 1=responder).
    pub fn new(buf_capacity: usize, local_base: u64) -> Self {
        let peer_base = if local_base == 0 { 1 } else { 0 };
        Self {
            streams: HashMap::new(),
            buf_capacity,
            local_next_id: local_base,
            peer_highest_id: None,
            peer_base,
            send_cursor: 0,
        }
    }

    /// Application writes data into a stream's send buffer.
    pub fn write(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        let stream = self.get_or_create(stream_id)?;
        Ok(stream.send.write(data, fin))
    }

    /// Application reads data from a stream's receive buffer.
    /// Returns `(bytes_read, fin)`.
    pub fn read(&mut self, stream_id: u64, out: &mut [u8]) -> Result<(usize, bool), StreamError> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        let n = stream.recv.read(out);
        let fin = n == 0 && stream.recv.is_finished();
        Ok((n, fin))
    }

    /// Network delivers received stream data into the receive buffer.
    pub fn recv(
        &mut self,
        stream_id: u64,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<(), StreamError> {
        let stream = self.get_or_create(stream_id)?;
        stream.recv.write(offset, data, fin)?;
        Ok(())
    }

    /// Emit the next stream frame for transmission.
    ///
    /// Data is written into `buf`.  Returns `Some((stream_id, offset, len, fin))`
    /// or `None` when no stream has data to send.
    /// Round-robin across streams for fairness.
    pub fn emit(&mut self, buf: &mut [u8]) -> Option<(u64, u64, usize, bool)> {
        if self.streams.is_empty() || buf.is_empty() {
            return None;
        }

        let ids: Vec<u64> = self.streams.keys().copied().collect();
        let len = ids.len();

        for i in 0..len {
            let idx = (self.send_cursor as usize + i) % len;
            let stream_id = ids[idx];
            let stream = self.streams.get_mut(&stream_id).unwrap();

            let (offset, n, fin) = stream.send.emit(buf);
            if n > 0 || fin {
                self.send_cursor = ids[(idx + 1) % len];
                return Some((stream_id, offset, n, fin));
            }
        }

        None
    }

    /// Peer acknowledged stream data.
    pub fn ack(&mut self, stream_id: u64, offset: u64, len: usize) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.ack(offset, len);
        }
    }

    /// Stream data was lost, needs retransmission.
    pub fn loss(&mut self, stream_id: u64, offset: u64, len: usize) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.loss(offset, len);
        }
    }

    /// Remove a finished stream.  Returns false if the stream doesn't exist
    /// or hasn't finished on both sides.
    pub fn remove(&mut self, stream_id: u64) -> bool {
        let Some(stream) = self.streams.get(&stream_id) else {
            return false;
        };
        if stream.send.is_finished() && stream.recv.is_finished() {
            self.streams.remove(&stream_id);
            true
        } else {
            false
        }
    }

    /// Streams with contiguous data available for reading.
    pub fn readable(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams
            .iter()
            .filter(|(_, s)| s.recv.is_readable())
            .map(|(&id, _)| id)
    }

    /// Streams that can accept writes (send buffer has free space, fin not sent).
    pub fn writable(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams
            .iter()
            .filter(|(_, s)| s.send.free() > 0 && s.send.fin_off().is_none())
            .map(|(&id, _)| id)
    }

    /// Streams whose receive window should be advertised to the peer.
    ///
    /// Returns `(stream_id, new_max_data)` and updates the local max_data.
    pub fn window_updates(&mut self) -> Vec<(u64, u64)> {
        let mut updates = Vec::new();
        for (&id, stream) in &mut self.streams {
            if stream.recv.should_update_max_data() {
                stream.recv.update_max_data();
                updates.push((id, stream.recv.max_data()));
            }
        }
        updates
    }

    /// Update a stream's send-side flow control limit (from peer's WindowUpdate).
    pub fn update_send_max_data(&mut self, stream_id: u64, max_data: u64) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.update_max_data(max_data);
        }
    }

    fn get_or_create(&mut self, stream_id: u64) -> Result<&mut Stream, StreamError> {
        if self.streams.contains_key(&stream_id) {
            return Ok(self.streams.get_mut(&stream_id).unwrap());
        }

        let is_peer = (stream_id % 2) == self.peer_base;

        if is_peer {
            // Peer-initiated stream.
            if let Some(highest) = self.peer_highest_id {
                if stream_id <= highest {
                    // ID ≤ highest but not in streams → was removed → reuse.
                    return Err(StreamError::IdReused);
                }
            }
            // Implicitly open all peer streams from current highest+2 to stream_id.
            let start = self
                .peer_highest_id
                .map(|h| h + 2)
                .unwrap_or(self.peer_base);
            let mut id = start;
            while id <= stream_id {
                if !self.streams.contains_key(&id) {
                    self.streams.insert(id, Stream::new(self.buf_capacity));
                }
                id += 2;
            }
            self.peer_highest_id = Some(stream_id);
        } else {
            // Local stream.
            if stream_id < self.local_next_id {
                return Err(StreamError::IdReused);
            }
            self.local_next_id = stream_id + 2;
            self.streams
                .insert(stream_id, Stream::new(self.buf_capacity));
        }

        Ok(self.streams.get_mut(&stream_id).unwrap())
    }
}
