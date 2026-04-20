use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeBounds;

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
    /// True if this stream has never emitted a frame. Used to send
    /// an empty Stream frame to notify the peer that the stream exists.
    needs_open: bool,
}

impl Stream {
    fn new_local(buf_capacity: usize) -> Self {
        Self {
            send: SendBuf::new(buf_capacity),
            recv: RecvBuf::new(buf_capacity),
            needs_open: true,
        }
    }

    fn new_peer(buf_capacity: usize) -> Self {
        Self {
            send: SendBuf::new(buf_capacity),
            recv: RecvBuf::new(buf_capacity),
            needs_open: false,
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
/// # Stream-count flow control
///
/// QUIC-style cumulative MAX_STREAMS credit:
/// - Each side advertises `MaxStreams { count }`: "you may open up to `count`
///   local streams in total over the connection lifetime".
/// - Sender refuses to open beyond peer's advertised credit (returns
///   `TooManyStreams` to app — backpressure).
/// - Receiver advances its credit by 1 each time it cleans up a peer stream
///   (frees memory) and periodically emits an updated `MaxStreams` frame.
/// - Initial credit on both sides equals `max_streams`.
///
/// This eliminates the cleanup race that would otherwise let one side open
/// a new stream before the other has freed its slot.
///
/// If a stream is removed (both sides finished) and the same ID is
/// accessed again, it is rejected as `IdReused`.
pub struct StreamManager {
    pub(super) streams: BTreeMap<u64, Stream>,
    buf_capacity: usize,
    /// Next ID to allocate for locally-created streams.
    local_next_id: u64,
    /// Next ID to allocate for peer-created streams.
    peer_next_id: u64,
    /// Local stream ID base (parity).
    local_base: u64,
    /// Peer stream ID base (0 for even, 1 for odd).
    peer_base: u64,
    /// Round-robin cursor: stream ID to start the next emit search from.
    /// After emitting from stream X, advances to X+1 so the next emit
    /// considers the smallest stream ID strictly greater than X first
    /// and wraps around if none.
    send_cursor: u64,
    /// Stream IDs whose state changed since last drain (new or data received).
    updated: BTreeSet<u64>,
    /// Initial per-direction stream count cap (used as the threshold for
    /// `should_update_max_streams`).
    max_streams: usize,
    /// Cumulative count of local streams the peer has permitted us to open.
    /// Init = `max_streams`.  Increases when peer sends a `MaxStreams` update.
    peer_max_streams: u64,
    /// Cumulative count of peer streams we permit.  Init = `max_streams`.
    /// Increases by 1 each time `try_remove` cleans up a peer stream.
    advertised_max_streams: u64,
    /// Last `advertised_max_streams` value we successfully sent to peer.
    /// Difference vs `advertised_max_streams` triggers update emission.
    sent_max_streams: u64,
    /// True if a `MaxStreams` frame was lost and must be re-sent regardless
    /// of threshold.
    force_max_streams_update: bool,
}

impl StreamManager {
    pub fn new(buf_capacity: usize, is_initiator: bool, max_streams: usize) -> Self {
        let (local_base, peer_base) = if is_initiator { (0, 1) } else { (1, 0) };
        let initial = max_streams as u64;
        Self {
            streams: BTreeMap::new(),
            buf_capacity,
            local_next_id: local_base,
            peer_next_id: peer_base,
            local_base,
            peer_base,
            send_cursor: 0,
            updated: BTreeSet::new(),
            max_streams,
            peer_max_streams: initial,
            advertised_max_streams: initial,
            sent_max_streams: initial,
            force_max_streams_update: false,
        }
    }

    /// Application writes data into a stream's send buffer.
    ///
    /// For local-parity IDs the stream is gap-filled if missing.
    /// For peer-parity IDs the stream must already exist (peer opens it),
    /// otherwise returns `UnknownStream`.
    pub fn write(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        let stream = if (stream_id % 2) == self.peer_base {
            self.streams
                .get_mut(&stream_id)
                .ok_or(StreamError::UnknownStream)?
        } else {
            self.get_or_create_local(stream_id)?
        };
        Ok(stream.send.write(data, fin))
    }

    /// Application reads data from a stream's receive buffer.
    /// Returns `(bytes_read, fin)`.
    ///
    /// When the stream is fully finished on both sides (FIN received and
    /// drained, FIN sent and acked), it is automatically cleaned up.
    /// Subsequent calls on the same `stream_id` return `UnknownStream`.
    pub fn read(&mut self, stream_id: u64, out: &mut [u8]) -> Result<(usize, bool), StreamError> {
        let stream = self
            .streams
            .get_mut(&stream_id)
            .ok_or(StreamError::UnknownStream)?;
        let n = stream.recv.read(out);
        let fin = n == 0 && stream.recv.is_finished();
        self.try_remove(stream_id);
        Ok((n, fin))
    }

    /// Network delivers received stream data into the receive buffer.
    ///
    /// Peer-parity IDs are gap-filled (implicit open).
    /// Local-parity IDs must already exist — peer cannot fabricate streams
    /// on our side.  Returns `UnknownStream` otherwise.
    pub fn recv(
        &mut self,
        stream_id: u64,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<(), StreamError> {
        let stream = if (stream_id % 2) == self.peer_base {
            self.get_or_create_peer(stream_id)?
        } else {
            self.streams
                .get_mut(&stream_id)
                .ok_or(StreamError::UnknownStream)?
        };
        stream.recv.write(offset, data, fin)?;
        self.updated.insert(stream_id);
        self.try_remove(stream_id);
        Ok(())
    }

    /// Emit the next stream frame for transmission.
    ///
    /// Data is written into `buf`.  Returns `Some((stream_id, offset, len, fin))`
    /// or `None` when no stream has data to send.
    ///
    /// Round-robin across streams via `send_cursor`: pass 1 walks streams with
    /// `id >= send_cursor`, pass 2 wraps to `id < send_cursor`. Holes in the
    /// ID space are skipped naturally by `BTreeMap::range`.
    pub fn emit(&mut self, buf: &mut [u8]) -> Option<(u64, u64, usize, bool)> {
        if buf.is_empty() {
            return None;
        }

        let result = Self::try_emit_in(&mut self.streams, self.send_cursor.., buf)
            .or_else(|| Self::try_emit_in(&mut self.streams, ..self.send_cursor, buf));

        if let Some((stream_id, _, _, _)) = result {
            self.send_cursor = stream_id.saturating_add(1);
        }
        result
    }

    /// Try to emit from the first stream in `range` that has data
    /// (or `needs_open`).  Returns the chosen stream's frame.
    fn try_emit_in<R: RangeBounds<u64>>(
        streams: &mut BTreeMap<u64, Stream>,
        range: R,
        buf: &mut [u8],
    ) -> Option<(u64, u64, usize, bool)> {
        for (&stream_id, stream) in streams.range_mut(range) {
            let (offset, n, fin) = stream.send.emit(buf);
            if n > 0 || fin {
                stream.needs_open = false;
                return Some((stream_id, offset, n, fin));
            }
            // No data, but stream never emitted → empty frame to notify peer.
            if stream.needs_open {
                stream.needs_open = false;
                return Some((stream_id, 0, 0, false));
            }
        }
        None
    }

    /// Peer acknowledged stream data.
    /// May trigger automatic cleanup if this ACK completes the send side
    /// while the recv side was already drained.
    pub fn ack(&mut self, stream_id: u64, offset: u64, len: usize) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.ack(offset, len);
        }
        self.try_remove(stream_id);
    }

    /// Stream data was lost, needs retransmission.
    pub fn loss(&mut self, stream_id: u64, offset: u64, len: usize) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.loss(offset, len);
        }
    }

    /// Remove the stream if both sides are fully finished.
    /// Called automatically from `read`, `recv`, and `ack`.
    /// Cleaning up a peer-stream releases one slot of memory; we issue an
    /// extra credit to the peer via `advertised_max_streams`.
    fn try_remove(&mut self, stream_id: u64) {
        let Some(stream) = self.streams.get(&stream_id) else {
            return;
        };
        if !stream.send.is_finished() || !stream.recv.is_finished() {
            return;
        }
        self.streams.remove(&stream_id);
        if (stream_id % 2) == self.peer_base {
            self.advertised_max_streams = self.advertised_max_streams.saturating_add(1);
        }
        // No local-side counter to decrement: outgoing flow is gated by
        // `peer_max_streams` (the credit peer grants us), not a live count.
        self.updated.insert(stream_id);
    }

    /// True if the cumulative credit growth since the last sent update has
    /// reached the half-window threshold; caller should drain and emit.
    pub fn should_update_max_streams(&self) -> bool {
        if self.force_max_streams_update {
            return true;
        }
        let delta = self.advertised_max_streams.saturating_sub(self.sent_max_streams);
        delta >= ((self.max_streams as u64) / 2).max(1)
    }

    /// Take the pending `MaxStreams` value to send.  Returns `None` if no
    /// update is needed.  Marks the value as sent.
    pub fn drain_max_streams_update(&mut self) -> Option<u64> {
        if !self.should_update_max_streams() {
            return None;
        }
        self.force_max_streams_update = false;
        self.sent_max_streams = self.advertised_max_streams;
        Some(self.advertised_max_streams)
    }

    /// Re-queue a `MaxStreams` update after the carrier frame was lost.
    pub fn requeue_max_streams_update(&mut self) {
        self.force_max_streams_update = true;
    }

    /// Receive a `MaxStreams` update from peer.  Increases our outgoing credit.
    pub fn update_send_max_streams(&mut self, count: u64) {
        if count > self.peer_max_streams {
            self.peer_max_streams = count;
        }
    }

    /// Drain stream IDs whose state changed since last call into `out`.
    /// Caller's buffer is appended to (existing contents preserved).
    pub fn drain_updated(&mut self, out: &mut Vec<u64>) {
        out.extend(std::mem::take(&mut self.updated));
    }

    /// True if any stream has unsent data or retransmits to emit.
    pub fn has_pending(&self) -> bool {
        self.streams
            .values()
            .any(|s| s.needs_open || s.send.unsent() > 0 || s.send.has_retransmits())
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
    /// Appends `(stream_id, new_max_data)` to `out` and updates the local max_data.
    pub fn window_updates(&mut self, out: &mut Vec<(u64, u64)>) {
        for (&id, stream) in &mut self.streams {
            if stream.recv.should_update_max_data() {
                stream.recv.update_max_data();
                out.push((id, stream.recv.max_data()));
            }
        }
    }

    /// Update a stream's send-side flow control limit (from peer's WindowUpdate).
    pub fn update_send_max_data(&mut self, stream_id: u64, max_data: u64) {
        if let Some(stream) = self.streams.get_mut(&stream_id) {
            stream.send.update_max_data(max_data);
        }
    }

    fn get_or_create_local(&mut self, stream_id: u64) -> Result<&mut Stream, StreamError> {
        debug_assert!(stream_id % 2 != self.peer_base);
        if self.streams.contains_key(&stream_id) {
            return Ok(self.streams.get_mut(&stream_id).unwrap());
        }
        if stream_id < self.local_next_id {
            return Err(StreamError::IdReused);
        }
        // Cumulative-count credit check: how many local streams will exist
        // (ever opened) after this gap-fill?  Must not exceed peer's grant.
        let opened_after = (stream_id - self.local_base) / 2 + 1;
        if opened_after > self.peer_max_streams {
            return Err(StreamError::TooManyStreams);
        }
        let mut id = self.local_next_id;
        while id <= stream_id {
            self.streams
                .insert(id, Stream::new_local(self.buf_capacity));
            id += 2;
        }
        self.local_next_id = stream_id + 2;
        Ok(self.streams.get_mut(&stream_id).unwrap())
    }

    fn get_or_create_peer(&mut self, stream_id: u64) -> Result<&mut Stream, StreamError> {
        debug_assert!(stream_id % 2 == self.peer_base);
        if self.streams.contains_key(&stream_id) {
            return Ok(self.streams.get_mut(&stream_id).unwrap());
        }
        if stream_id < self.peer_next_id {
            return Err(StreamError::IdReused);
        }
        // Peer must respect the credit we advertised.
        let opened_after = (stream_id - self.peer_base) / 2 + 1;
        if opened_after > self.advertised_max_streams {
            return Err(StreamError::TooManyStreams);
        }
        let mut id = self.peer_next_id;
        while id <= stream_id {
            self.streams.insert(id, Stream::new_peer(self.buf_capacity));
            self.updated.insert(id);
            id += 2;
        }
        self.peer_next_id = stream_id + 2;
        Ok(self.streams.get_mut(&stream_id).unwrap())
    }
}
