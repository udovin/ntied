use std::collections::{BTreeSet, HashMap};

use super::buffer::{RecvBuf, RecvBufError, SendBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    IdReused,
    UnknownStream,
    FlowControl,
    FinalSizeMismatch,
    TooManyStreams,
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
/// Both local and peer streams are implicitly created with gap-fill:
/// when stream N is accessed, all streams of the same parity from
/// the current watermark up to N are created.
///
/// Local streams use even IDs for initiator, odd for responder.
/// Peer streams use the opposite parity.
///
/// If a stream is removed (both sides finished) and the same ID is
/// accessed again, it is rejected as `IdReused`.
pub struct StreamManager {
    pub(super) streams: HashMap<u64, Stream>,
    buf_capacity: usize,
    /// Next ID to allocate for locally-created streams.
    local_next_id: u64,
    /// Next ID to allocate for peer-created streams.
    peer_next_id: u64,
    /// Peer stream ID base (0 for even, 1 for odd).
    peer_base: u64,
    /// Round-robin cursor for fair `send()` scheduling.
    send_cursor: u64,
    /// Stream IDs whose state changed since last drain (new or data received).
    updated: BTreeSet<u64>,
    /// Maximum number of streams per direction.
    max_streams: usize,
    /// Current count of locally-created streams.
    local_count: usize,
    /// Current count of peer-created streams.
    peer_count: usize,
}

impl StreamManager {
    pub fn new(buf_capacity: usize, is_initiator: bool) -> Self {
        let (local_base, peer_base) = if is_initiator { (0, 1) } else { (1, 0) };
        Self {
            streams: HashMap::new(),
            buf_capacity,
            local_next_id: local_base,
            peer_next_id: peer_base,
            peer_base,
            send_cursor: 0,
            updated: BTreeSet::new(),
            max_streams: 256,
            local_count: 0,
            peer_count: 0,
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
        self.updated.insert(stream_id);
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
            if (stream_id % 2) == self.peer_base {
                self.peer_count = self.peer_count.saturating_sub(1);
            } else {
                self.local_count = self.local_count.saturating_sub(1);
            }
            true
        } else {
            false
        }
    }

    /// Drain stream IDs whose state changed since last call.
    pub fn drain_updated(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.updated).into_iter().collect()
    }

    /// True if any stream has unsent data or retransmits to emit.
    pub fn has_pending(&self) -> bool {
        self.streams
            .values()
            .any(|s| s.send.unsent() > 0 || s.send.has_retransmits())
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
            if stream_id < self.peer_next_id {
                return Err(StreamError::IdReused);
            }
            // Gap-fill all peer streams up to stream_id.
            let mut id = self.peer_next_id;
            while id <= stream_id {
                if self.peer_count >= self.max_streams {
                    return Err(StreamError::TooManyStreams);
                }
                self.streams.insert(id, Stream::new(self.buf_capacity));
                self.peer_count += 1;
                self.updated.insert(id);
                id += 2;
            }
            self.peer_next_id = stream_id + 2;
        } else {
            if stream_id < self.local_next_id {
                return Err(StreamError::IdReused);
            }
            // Gap-fill all local streams up to stream_id.
            let mut id = self.local_next_id;
            while id <= stream_id {
                if self.local_count >= self.max_streams {
                    return Err(StreamError::TooManyStreams);
                }
                self.streams.insert(id, Stream::new(self.buf_capacity));
                self.local_count += 1;
                id += 2;
            }
            self.local_next_id = stream_id + 2;
        }

        Ok(self.streams.get_mut(&stream_id).unwrap())
    }
}
