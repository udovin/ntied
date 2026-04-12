use std::collections::BTreeMap;

/// Fixed-capacity send buffer with range-based ack and retransmit.
///
/// # Layout
///
/// ```text
/// [acked | in_flight / lost | unsent | free]
///  ^       ^ack_off()         ^send_off ^write_off
/// ```
///
/// Data is stored in a ring buffer.  `acked` tracks confirmed ranges,
/// `retransmits` tracks lost ranges.  `emit()` prioritizes retransmits,
/// then new data from `send_off`.
///
/// # Invariants
///
/// These hold after every public method call and are checked by `debug_assert`:
///
/// - **I1**: `ack_off() <= send_off <= write_off`
/// - **I2**: `write_off - ack_off() <= capacity`
///   (ring buffer never overflows; live data always fits)
/// - **I3**: `∀ (s, e) ∈ retransmits: ack_off() <= s < e <= send_off`
///   (only previously-sent, non-acked data can be retransmitted)
/// - **I4**: ranges in `acked` are sorted, non-overlapping, non-adjacent
/// - **I5**: ranges in `retransmits` are sorted, non-overlapping, non-adjacent
/// - **I6**: `fin_off.is_none() || fin_off == Some(write_off_at_fin_time)`
///   (fin is immutable once set)
///
/// # Ring safety
///
/// Byte at stream offset `x` occupies `buf[x % capacity]`.  Two offsets
/// `x` and `y` collide iff `|x - y| >= capacity`.  I2 guarantees
/// `write_off - ack_off() <= capacity`, so all live offsets
/// `[ack_off(), write_off)` map to distinct ring positions.
pub struct SendBuf {
    buf: Box<[u8]>,
    acked: BTreeMap<u64, u64>,
    retransmits: BTreeMap<u64, u64>,
    send_off: u64,
    write_off: u64,
    fin_off: Option<u64>,
    /// Peer's receive window: max stream offset we may send new data to.
    /// Retransmits are exempt (already accepted by the peer's window).
    pub(super) max_data: u64,
    /// Offset at which emit() was last blocked by max_data.
    /// Used to decide whether to send a STREAM_DATA_BLOCKED frame.
    blocked_at: Option<u64>,
    /// Cached first contiguous acked offset.  Updated in `ack()`.
    /// Avoids O(log n) BTreeMap lookup on every `cap()`/`write()`.
    cached_ack_off: u64,
}

impl SendBuf {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            buf: vec![0u8; capacity].into_boxed_slice(),
            acked: BTreeMap::new(),
            retransmits: BTreeMap::new(),
            send_off: 0,
            write_off: 0,
            fin_off: None,
            max_data: capacity as u64,
            blocked_at: None,
            cached_ack_off: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// First contiguous acked offset.  Ring space before this is free.  O(1).
    pub fn ack_off(&self) -> u64 {
        self.cached_ack_off
    }

    /// How many bytes the application can write (ring free space).
    /// Window does NOT limit writes — only `emit()`.
    pub fn cap(&self) -> usize {
        self.buf.len() - (self.write_off - self.ack_off()) as usize
    }

    /// Alias for `cap()`.
    pub fn free(&self) -> usize {
        self.cap()
    }

    /// New unsent bytes available.
    pub fn unsent(&self) -> usize {
        (self.write_off - self.send_off) as usize
    }

    pub fn has_retransmits(&self) -> bool {
        !self.retransmits.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.write_off == self.ack_off()
    }

    /// All data has been acked including fin.
    pub fn is_finished(&self) -> bool {
        match self.fin_off {
            Some(fin) => self.ack_off() >= fin,
            None => false,
        }
    }

    pub fn send_off(&self) -> u64 {
        self.send_off
    }

    pub fn write_off(&self) -> u64 {
        self.write_off
    }

    pub fn fin_off(&self) -> Option<u64> {
        self.fin_off
    }

    /// Peer's receive window limit (max stream offset for new data).
    pub fn max_data(&self) -> u64 {
        self.max_data
    }

    /// Update the peer's receive window.  Only increases.
    /// Clears `blocked_at` if the new limit unblocks the stream.
    pub fn update_max_data(&mut self, max_data: u64) {
        if max_data > self.max_data {
            self.max_data = max_data;
            if self.blocked_at.is_some() {
                // blocked_at < new max_data is guaranteed: blocked_at was set
                // to send_off when send_off == old max_data, and new max_data > old.
                self.blocked_at = None;
            }
        }
    }

    /// True if new data is blocked by the peer's window.
    pub fn is_blocked(&self) -> bool {
        self.send_off >= self.max_data && self.unsent() > 0
    }

    /// Offset where emit() was last blocked, for STREAM_DATA_BLOCKED frame.
    /// Cleared when `update_max_data()` unblocks.
    pub fn blocked_at(&self) -> Option<u64> {
        self.blocked_at
    }

    /// Write user data.  Returns bytes written.
    /// Limited by `cap()` (ring free space).  Window only limits `emit()`.
    /// After fin, further writes return 0.
    pub fn write(&mut self, data: &[u8], fin: bool) -> usize {
        if self.fin_off.is_some() {
            return 0;
        }

        let n = data.len().min(self.cap());
        if n > 0 {
            self.ring_copy_in(self.write_off, &data[..n]);
            self.write_off += n as u64;
        }

        if fin {
            self.fin_off = Some(self.write_off);
        }

        self.check_invariants();
        n
    }

    /// Emit data for transmission.  Returns `(stream_offset, bytes_read, fin)`.
    ///
    /// Retransmits are emitted first (I3 guarantees their data is in the ring).
    /// Then new data from `send_off`.  `fin` is set when the last stream byte
    /// is emitted and no retransmits remain.
    pub fn emit(&mut self, out: &mut [u8]) -> (u64, usize, bool) {
        if out.is_empty() {
            return (self.send_off, 0, false);
        }

        if let Some((&start, &end)) = self.retransmits.first_key_value() {
            // SAFETY(ring): start >= ack_off() by I3, so ring data is live.
            let n = out.len().min((end - start) as usize);
            self.ring_copy_out(start, &mut out[..n]);
            let emit_end = start + n as u64;

            self.retransmits.remove(&start);
            if emit_end < end {
                self.retransmits.insert(emit_end, end);
            }

            // fin_off == emit_end ∧ retransmits empty → unsent == 0 by I3.
            let fin = self.fin_off == Some(emit_end) && self.retransmits.is_empty();
            self.check_invariants();
            return (start, n, fin);
        }

        // New data is limited by peer's receive window.
        let window = self.max_data.saturating_sub(self.send_off) as usize;
        let n = out.len().min(self.unsent()).min(window);
        if n == 0 {
            // n == 0 with unsent > 0 implies window == 0 (emit buffer is non-empty,
            // checked at top of function).
            if self.unsent() > 0 {
                self.blocked_at = Some(self.send_off);
            }
            let fin = self.fin_off == Some(self.send_off) && self.retransmits.is_empty();
            return (self.send_off, 0, fin);
        }
        let offset = self.send_off;
        self.ring_copy_out(self.send_off, &mut out[..n]);
        self.send_off += n as u64;

        let fin = self.fin_off == Some(self.send_off) && self.retransmits.is_empty();
        self.check_invariants();
        (offset, n, fin)
    }

    /// Mark range `[offset, offset+len)` as acknowledged by peer.
    ///
    /// Preserves I4 (merges into `acked`) and I5 (removes from `retransmits`).
    pub fn ack(&mut self, offset: u64, len: usize) {
        if len == 0 {
            return;
        }
        let end = offset + len as u64;
        debug_assert!(
            end <= self.write_off,
            "ack({offset}, {len}) beyond write_off({})",
            self.write_off
        );

        Self::insert_range(&mut self.acked, offset, end);
        Self::remove_range(&mut self.retransmits, offset, end);

        self.refresh_ack_off();
        self.check_invariants();
    }

    /// Mark range `[offset, offset+len)` as lost, needing retransmission.
    ///
    /// Clamped to `[ack_off(), send_off)`.  Only non-acked sub-ranges are
    /// inserted into `retransmits`, preserving I3 and I5.
    pub fn loss(&mut self, offset: u64, len: usize) {
        if len == 0 {
            return;
        }
        let end = offset + len as u64;

        let ack_off = self.ack_off();
        let start = offset.max(ack_off);
        let end = end.min(self.send_off);
        if start >= end {
            return;
        }

        Self::insert_non_acked(&self.acked, &mut self.retransmits, start, end);

        self.check_invariants();
    }

    /// Insert `[start, end)` into a range map, merging overlapping/adjacent.
    /// Preserves sorted, non-overlapping, non-adjacent invariant (I4/I5).
    fn insert_range(map: &mut BTreeMap<u64, u64>, start: u64, end: u64) {
        let mut merged_start = start;
        let mut merged_end = end;

        // Merge with range just before start.
        if let Some((&rs, &re)) = map.range(..start).next_back() {
            if re >= start {
                merged_start = rs;
                merged_end = merged_end.max(re);
                map.remove(&rs);
            }
        }

        // Merge with overlapping/adjacent ranges from start onward.
        loop {
            let Some((&rs, &re)) = map.range(start..=merged_end).next() else {
                break;
            };
            merged_end = merged_end.max(re);
            map.remove(&rs);
        }

        map.insert(merged_start, merged_end);
    }

    /// Remove `[start, end)` from a range map, splitting/trimming as needed.
    /// Preserves sorted, non-overlapping, non-adjacent invariant (I4/I5).
    fn remove_range(map: &mut BTreeMap<u64, u64>, start: u64, end: u64) {
        if let Some((&rs, &re)) = map.range(..=start).next_back() {
            if re > start {
                map.remove(&rs);
                if rs < start {
                    map.insert(rs, start);
                }
                if re > end {
                    map.insert(end, re);
                }
            }
        }

        loop {
            let Some((&rs, &re)) = map.range(start..end).next() else {
                break;
            };
            map.remove(&rs);
            if re > end {
                map.insert(end, re);
            }
        }
    }

    /// Insert `[start, end)` into `retransmits`, skipping sub-ranges
    /// covered by `acked`.  Guarantees I3: only non-acked gaps are inserted.
    fn insert_non_acked(
        acked: &BTreeMap<u64, u64>,
        retransmits: &mut BTreeMap<u64, u64>,
        start: u64,
        end: u64,
    ) {
        let mut cursor = start;

        // Check acked range starting before `cursor`.
        if let Some((&_rs, &re)) = acked.range(..=cursor).next_back() {
            if re > cursor {
                cursor = re;
            }
        }

        if cursor >= end {
            return;
        }

        // Walk acked ranges in [cursor, end), insert gaps into retransmits.
        loop {
            let Some((&rs, &re)) = acked.range(cursor..end).next() else {
                break;
            };
            // cursor < rs is guaranteed: acked ranges are non-adjacent,
            // so each range starts strictly after the previous one's end.
            debug_assert!(cursor < rs, "acked ranges should be non-adjacent");
            Self::insert_range(retransmits, cursor, rs);
            cursor = re;
        }

        if cursor < end {
            Self::insert_range(retransmits, cursor, end);
        }
    }

    #[inline]
    fn check_invariants(&self) {
        if cfg!(debug_assertions) {
            let ack_off = self.ack_off();
            debug_assert!(
                ack_off <= self.send_off,
                "ack_off({ack_off}) > send_off({})",
                self.send_off
            );
            debug_assert!(
                self.send_off <= self.write_off,
                "send_off({}) > write_off({})",
                self.send_off,
                self.write_off
            );
            debug_assert!(
                (self.write_off - ack_off) as usize <= self.buf.len(),
                "buffered({}) > capacity({})",
                self.write_off - ack_off,
                self.buf.len()
            );

            // Retransmits must be within [ack_off, send_off).
            for (&rs, &re) in &self.retransmits {
                debug_assert!(
                    rs >= ack_off,
                    "retransmit start {rs} below ack_off {ack_off}"
                );
                debug_assert!(
                    re <= self.send_off,
                    "retransmit end {re} beyond send_off {}",
                    self.send_off
                );
            }
        }
    }

    /// Recompute `cached_ack_off` from the acked BTreeMap.
    fn refresh_ack_off(&mut self) {
        self.cached_ack_off = match self.acked.first_key_value() {
            Some((&start, &end)) if start <= self.cached_ack_off => end,
            _ => self.cached_ack_off,
        };
    }

    fn ring_copy_in(&mut self, stream_off: u64, data: &[u8]) {
        let cap = self.buf.len();
        let start = (stream_off % cap as u64) as usize;
        let first = data.len().min(cap - start);
        self.buf[start..start + first].copy_from_slice(&data[..first]);
        if first < data.len() {
            self.buf[..data.len() - first].copy_from_slice(&data[first..]);
        }
    }

    fn ring_copy_out(&self, stream_off: u64, out: &mut [u8]) {
        let cap = self.buf.len();
        let n = out.len();
        let start = (stream_off % cap as u64) as usize;
        let first = n.min(cap - start);
        out[..first].copy_from_slice(&self.buf[start..start + first]);
        if first < n {
            out[first..n].copy_from_slice(&self.buf[..n - first]);
        }
    }
}

/// Receive buffer: ring buffer for data, `BTreeMap` for range tracking.
///
/// # Layout
///
/// Data is stored in a fixed ring buffer.  A `BTreeMap<u64, u64>` (start → end)
/// tracks which byte ranges have been received.  Gaps between ranges represent
/// missing data.  Capacity equals the flow control window.
///
/// # Invariants
///
/// These hold after every public method call:
///
/// - **I1**: ranges are sorted, non-overlapping, non-adjacent (enforced by `insert_range`)
/// - **I2**: `∀ (s, e) ∈ ranges: e > read_off`
///   (fully consumed ranges are removed in `read()`)
/// - **I3**: `∀ (s, e) ∈ ranges: s < read_off + capacity`
///   (no range starts beyond the receive window)
/// - **I4**: ring buffer at `off % capacity` contains valid data for any
///   offset covered by a range where `off >= read_off`
/// - **I5**: `fin_off` is immutable once set; no data accepted past `fin_off`
///
/// # Ring safety
///
/// Same as `SendBuf`: distinct live offsets map to distinct ring positions
/// because the window `[read_off, read_off + capacity)` spans at most
/// `capacity` offsets.
pub struct RecvBuf {
    buf: Box<[u8]>,
    /// Next byte offset to deliver to the reader (= consumed).
    read_off: u64,
    /// Received ranges: start → end.  Non-overlapping, non-adjacent.
    pub(super) ranges: BTreeMap<u64, u64>,
    /// Stream final offset, if known.
    fin_off: Option<u64>,
    /// Current advertised max offset to the peer.
    pub(super) max_data: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvBufError {
    /// Data exceeds the receive window (flow control violation).
    FlowControl,
    /// Received data contradicts a previously established final size.
    FinalSizeMismatch,
}

impl RecvBuf {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            buf: vec![0u8; capacity].into_boxed_slice(),
            read_off: 0,
            ranges: BTreeMap::new(),
            fin_off: None,
            max_data: capacity as u64,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn read_off(&self) -> u64 {
        self.read_off
    }

    /// Current advertised limit to the peer.
    pub fn max_data(&self) -> u64 {
        self.max_data
    }

    /// The next limit to advertise: `read_off + capacity`.
    pub fn max_data_next(&self) -> u64 {
        self.read_off + self.buf.len() as u64
    }

    /// True when available window has dropped below half — time to send WindowUpdate.
    pub fn should_update_max_data(&self) -> bool {
        self.fin_off.is_none() && (self.max_data - self.read_off) < (self.buf.len() as u64 / 2)
    }

    /// Commit the next window limit. Call after sending WindowUpdate.
    pub fn update_max_data(&mut self) {
        self.max_data = self.max_data_next();
    }

    /// Window size (= capacity, fixed).
    pub fn window(&self) -> u64 {
        self.buf.len() as u64
    }

    /// Contiguous bytes available for reading.  O(log n).
    ///
    /// `start <= read_off` is possible after partial reads (range keeps its
    /// original key).  `end > read_off` is guaranteed by I2.
    pub fn readable(&self) -> usize {
        if let Some((&start, &end)) = self.ranges.first_key_value() {
            if start <= self.read_off {
                debug_assert!(end > self.read_off); // I2
                return (end - self.read_off) as usize;
            }
        }
        0
    }

    pub fn is_readable(&self) -> bool {
        self.readable() > 0 || self.is_finished()
    }

    pub fn is_finished(&self) -> bool {
        self.fin_off == Some(self.read_off) && self.readable() == 0
    }

    /// Write received data at the given stream offset.
    ///
    /// Returns the number of new bytes stored (0 for duplicates).
    /// Validates fin consistency (I5) and window bounds (I3) before mutation.
    pub fn write(&mut self, offset: u64, data: &[u8], fin: bool) -> Result<usize, RecvBufError> {
        let end = offset + data.len() as u64;

        // Validate fin consistency.
        if fin {
            if let Some(existing) = self.fin_off {
                if existing != end {
                    return Err(RecvBufError::FinalSizeMismatch);
                }
            }
            self.fin_off = Some(end);
        } else if let Some(fin) = self.fin_off {
            // Non-fin data must not extend past the final size.
            if end > fin {
                return Err(RecvBufError::FinalSizeMismatch);
            }
        }

        if data.is_empty() {
            return Ok(0);
        }

        if end > self.max_data {
            return Err(RecvBufError::FlowControl);
        }

        if end <= self.read_off {
            return Ok(0);
        }

        // Clip below read_off.
        let eff_start = offset.max(self.read_off);
        let skip = (eff_start - offset) as usize;
        let eff_data = &data[skip..];
        let eff_end = eff_start + eff_data.len() as u64;

        // Fast path: check if entirely covered by an existing range.
        if let Some((&_rs, &re)) = self.ranges.range(..=eff_start).next_back() {
            if re >= eff_end {
                return Ok(0);
            }
        }

        // Copy into ring buffer, then track the range.
        self.ring_copy_in(eff_start, eff_data);
        let new_bytes = self.insert_range(eff_start, eff_end);

        Ok(new_bytes)
    }

    /// Read contiguous data into `out`.  Returns the number of bytes read.
    /// Advances `read_off` and removes fully consumed ranges (preserving I2).
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.readable());
        if n == 0 {
            return 0;
        }

        self.ring_copy_out(self.read_off, &mut out[..n]);
        self.read_off += n as u64;

        // Clean up fully consumed ranges.
        while let Some(entry) = self.ranges.first_entry() {
            if *entry.get() <= self.read_off {
                entry.remove();
            } else {
                break;
            }
        }

        n
    }

    /// Insert range `[start, end)`, merging overlapping/adjacent ranges.
    /// Returns the number of genuinely new bytes (not already covered).
    /// Preserves I1 (sorted, non-overlapping, non-adjacent).
    /// Zero heap allocations — overlapping ranges are removed on the fly.
    fn insert_range(&mut self, start: u64, end: u64) -> usize {
        let mut merged_start = start;
        let mut merged_end = end;
        let mut already_covered: u64 = 0;

        // A range starting before `start` might extend into [start, end).
        if let Some((&rs, &re)) = self.ranges.range(..start).next_back() {
            if re >= start {
                already_covered += re.min(end) - start;
                merged_start = rs;
                merged_end = merged_end.max(re);
                self.ranges.remove(&rs);
            }
        }

        // Remove overlapping/adjacent ranges on the fly.
        // After each removal the next query finds the next candidate.
        loop {
            let Some((&rs, &re)) = self.ranges.range(start..=merged_end).next() else {
                break;
            };
            let overlap_start = rs.max(start);
            let overlap_end = re.min(end);
            if overlap_end > overlap_start {
                already_covered += overlap_end - overlap_start;
            }
            merged_end = merged_end.max(re);
            self.ranges.remove(&rs);
        }

        self.ranges.insert(merged_start, merged_end);

        (end - start - already_covered) as usize
    }

    fn ring_copy_in(&mut self, stream_off: u64, data: &[u8]) {
        let cap = self.buf.len();
        let start = (stream_off % cap as u64) as usize;
        let first = data.len().min(cap - start);
        self.buf[start..start + first].copy_from_slice(&data[..first]);
        if first < data.len() {
            self.buf[..data.len() - first].copy_from_slice(&data[first..]);
        }
    }

    fn ring_copy_out(&self, stream_off: u64, out: &mut [u8]) {
        let cap = self.buf.len();
        let n = out.len();
        let start = (stream_off % cap as u64) as usize;
        let first = n.min(cap - start);
        out[..first].copy_from_slice(&self.buf[start..start + first]);
        if first < n {
            out[first..n].copy_from_slice(&self.buf[..n - first]);
        }
    }
}
