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
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// First contiguous acked offset.  Ring space before this is free.
    pub fn ack_off(&self) -> u64 {
        match self.acked.first_key_value() {
            Some((&0, &end)) => end,
            _ => 0,
        }
    }

    /// Bytes the user can still write.
    pub fn free(&self) -> usize {
        self.buf.len() - (self.write_off - self.ack_off()) as usize
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

    /// Write user data.  Returns bytes written (partial if buffer full).
    /// Pass `fin = true` to mark the end of the stream.
    /// After fin, further writes return 0.
    pub fn write(&mut self, data: &[u8], fin: bool) -> usize {
        if self.fin_off.is_some() {
            return 0;
        }

        let n = data.len().min(self.free());
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

        let n = out.len().min(self.unsent());
        if n == 0 {
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
            debug_assert!(ack_off <= self.send_off, "ack_off({ack_off}) > send_off({})", self.send_off);
            debug_assert!(self.send_off <= self.write_off, "send_off({}) > write_off({})", self.send_off, self.write_off);
            debug_assert!(
                (self.write_off - ack_off) as usize <= self.buf.len(),
                "buffered({}) > capacity({})",
                self.write_off - ack_off,
                self.buf.len()
            );

            // Retransmits must be within [ack_off, send_off).
            for (&rs, &re) in &self.retransmits {
                debug_assert!(rs >= ack_off, "retransmit start {rs} below ack_off {ack_off}");
                debug_assert!(re <= self.send_off, "retransmit end {re} beyond send_off {}", self.send_off);
            }
        }
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
    /// Next byte offset to deliver to the reader.
    read_off: u64,
    /// Received ranges: start → end.  Non-overlapping, non-adjacent.
    ranges: BTreeMap<u64, u64>,
    /// Stream final offset, if known.
    fin_off: Option<u64>,
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
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn read_off(&self) -> u64 {
        self.read_off
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
    pub fn write(
        &mut self,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, RecvBufError> {
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

        let window_end = self.read_off + self.buf.len() as u64;

        if end > window_end {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_basic() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.free(), 16);
        assert_eq!(buf.unsent(), 0);

        assert_eq!(buf.write(b"hello", false), 5);
        assert_eq!(buf.unsent(), 5);
        assert_eq!(buf.free(), 11);

        let mut out = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(off, 0);
        assert_eq!(n, 5);
        assert_eq!(&out, b"hello");

        assert_eq!(buf.unsent(), 0);
        assert_eq!(buf.free(), 11); // not freed until ack

        buf.ack(0, 5);
        assert_eq!(buf.free(), 16);
        assert!(buf.is_empty());
    }

    #[test]
    fn send_partial_write_when_full() {
        let mut buf = SendBuf::new(4);
        assert_eq!(buf.write(b"abcd", false), 4);
        assert_eq!(buf.free(), 0);
        assert_eq!(buf.write(b"e", false), 0);
    }

    #[test]
    fn send_emit_multiple_chunks() {
        let mut buf = SendBuf::new(1024);
        let data = vec![0xABu8; 1024];
        assert_eq!(buf.write(&data, false), 1024);

        let mut total = 0;
        let mut chunk = [0u8; 100];
        while buf.unsent() > 0 {
            let (off, n, _fin) = buf.emit(&mut chunk);
            assert_eq!(off, total as u64);
            total += n;
        }
        assert_eq!(total, 1024);
        assert_eq!(buf.free(), 0); // not freed until ack
    }

    #[test]
    fn send_ack_frees_space() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abcdefgh", false);
        assert_eq!(buf.free(), 0);

        let mut out = [0u8; 4];
        buf.emit(&mut out);

        buf.ack(0, 4);
        assert_eq!(buf.free(), 4);

        assert_eq!(buf.write(b"ijkl", false), 4);
        assert_eq!(buf.unsent(), 8);
    }

    #[test]
    fn send_loss_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false); // 10 bytes

        // Emit in two chunks.
        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(&out, b"ABCDE");

        buf.emit(&mut out);
        assert_eq!(&out, b"FGHIJ");
        assert_eq!(buf.unsent(), 0);
        assert!(!buf.has_retransmits());

        // First chunk [0..5) lost.
        buf.loss(0, 5);
        assert!(buf.has_retransmits());

        // Next emit returns the lost chunk, not new data.
        let mut out2 = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!(off, 0);
        assert_eq!(n, 5);
        assert_eq!(&out2, b"ABCDE");
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_only_unacked_part() {
        let mut buf = SendBuf::new(64);
        buf.write(b"ABCDEFGHIJKLMNOP", false); // 16 bytes

        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Ack [0..5) and [10..16).
        buf.ack(0, 5);
        buf.ack(10, 6);

        // Report loss of entire [0..16). Only [5..10) should be retransmitted.
        buf.loss(0, 16);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 16];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!(off, 5);
        assert_eq!(n, 5);
        assert_eq!(&out2[..5], b"FGHIJ");
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_removes_pending_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);

        let mut out = [0u8; 5];
        buf.emit(&mut out);

        // Loss detected, then late ack arrives.
        buf.loss(0, 5);
        assert!(buf.has_retransmits());

        buf.ack(0, 5);
        assert!(!buf.has_retransmits()); // ack removed it
    }

    #[test]
    fn send_noncontiguous_ack_no_free() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);

        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [5..10) but not [0..5). ack_off stays at 0.
        buf.ack(5, 5);
        assert_eq!(buf.ack_off(), 0);
        assert_eq!(buf.free(), 6); // 16 - (10 - 0) = 6

        // Now ack [0..5). ack_off jumps to 10.
        buf.ack(0, 5);
        assert_eq!(buf.ack_off(), 10);
        assert_eq!(buf.free(), 16);
    }

    #[test]
    fn send_partial_retransmit_emit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);

        let mut out = [0u8; 10];
        buf.emit(&mut out);

        buf.loss(0, 10);

        // Emit only 3 bytes of the retransmit.
        let mut small = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut small);
        assert_eq!(off, 0);
        assert_eq!(n, 3);
        assert_eq!(&small, b"ABC");

        // Remaining 7 bytes still in retransmit queue.
        assert!(buf.has_retransmits());
        let mut rest = [0u8; 7];
        let (off, n, _fin) = buf.emit(&mut rest);
        assert_eq!(off, 3);
        assert_eq!(n, 7);
        assert_eq!(&rest, b"DEFGHIJ");
    }

    #[test]
    fn send_wrap_around() {
        let mut buf = SendBuf::new(8);

        buf.write(b"abcdef", false);
        let mut tmp = [0u8; 6];
        buf.emit(&mut tmp);
        buf.ack(0, 6);

        assert_eq!(buf.write(b"ghijklmn", false), 8);
        assert_eq!(buf.unsent(), 8);

        let mut out = [0u8; 8];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(off, 6);
        assert_eq!(n, 8);
        assert_eq!(&out, b"ghijklmn");
    }

    #[test]
    fn send_repeated_cycles() {
        let mut buf = SendBuf::new(4);
        for i in 0u8..=255 {
            let data = [i; 4];
            assert_eq!(buf.write(&data, false), 4);

            let mut out = [0u8; 4];
            let (off, n, _fin) = buf.emit(&mut out);
            assert_eq!(n, 4);
            assert_eq!(out, data);

            buf.ack(off, n);
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn send_empty_emit() {
        let mut buf = SendBuf::new(8);
        let mut out = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!(off, 0);
        assert_eq!(n, 0);
    }

    #[test]
    fn send_fin_on_last_emit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", true);

        let mut out = [0u8; 5];
        let (off, n, fin) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 5));
        assert!(fin);
    }

    #[test]
    fn send_fin_partial_emit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", true);

        // Emit only 3 bytes — not at fin yet.
        let mut out = [0u8; 3];
        let (_, _, fin) = buf.emit(&mut out);
        assert!(!fin);

        // Emit remaining 2 — now at fin.
        let mut out2 = [0u8; 2];
        let (_, _, fin) = buf.emit(&mut out2);
        assert!(fin);
    }

    #[test]
    fn send_fin_empty_data() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", false);

        let mut out = [0u8; 5];
        buf.emit(&mut out);

        // Fin with no data.
        buf.write(b"", true);
        assert_eq!(buf.fin_off(), Some(5));

        // Bare fin emit.
        let (_, n, fin) = buf.emit(&mut [0u8; 1]);
        assert_eq!(n, 0);
        assert!(fin);
    }

    #[test]
    fn send_fin_on_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"end", true);

        let mut out = [0u8; 3];
        buf.emit(&mut out); // send [0..3) with fin

        // Lost — retransmit.
        buf.loss(0, 3);
        let (off, n, fin) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 3));
        assert!(fin); // retransmit also carries fin
    }

    #[test]
    fn send_write_after_fin_rejected() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", true);
        assert_eq!(buf.write(b"more", false), 0);
    }

    #[test]
    fn send_is_finished() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hi", true);
        assert!(!buf.is_finished());

        let mut out = [0u8; 2];
        buf.emit(&mut out);
        assert!(!buf.is_finished());

        buf.ack(0, 2);
        assert!(buf.is_finished());
    }

    #[test]
    fn send_emit_empty_out() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abc", false);
        let (off, n, _fin) = buf.emit(&mut []);
        assert_eq!(n, 0);
        assert_eq!(off, 0);
    }

    #[test]
    fn send_ack_zero_len() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abc", false);
        let mut out = [0u8; 3];
        buf.emit(&mut out);
        buf.ack(0, 0); // no-op
        assert_eq!(buf.ack_off(), 0);
    }

    #[test]
    fn send_loss_zero_len() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abc", false);
        let mut out = [0u8; 3];
        buf.emit(&mut out);
        buf.loss(0, 0); // no-op
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_fully_clamped() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);
        let mut out = [0u8; 5];
        buf.emit(&mut out);
        buf.ack(0, 5);

        // Loss range is fully acked — clamped to empty.
        buf.loss(0, 5);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_splits_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Mark [0..10) as lost.
        buf.loss(0, 10);
        assert!(buf.has_retransmits());

        // Ack middle [3..7) — should remove that part from retransmits.
        buf.ack(3, 4);

        // Retransmits should now be [0..3) and [7..10).
        let mut out1 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out1);
        assert_eq!((off, n), (0, 3));
        assert_eq!(&out1, b"ABC");

        let mut out2 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (7, 3));
        assert_eq!(&out2, b"HIJ");

        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_with_partial_ack_at_start() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..4).
        buf.ack(0, 4);

        // Loss [2..8) — [2..4) is acked, only [4..8) should retransmit.
        buf.loss(2, 6);

        let mut out2 = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (4, 4));
        assert_eq!(&out2, b"EFGH");
    }

    #[test]
    fn send_ack_removes_multiple_retransmits() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false); // 16 bytes
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Create two retransmit ranges.
        buf.loss(2, 3);  // [2..5)
        buf.loss(8, 3);  // [8..11)

        // Ack [0..16) should remove both from retransmits.
        buf.ack(0, 16);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_fully_covered_by_ack() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack entire range.
        buf.ack(0, 10);

        // Loss of [0..10) — entirely acked, nothing to retransmit.
        buf.loss(0, 10);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_trims_retransmit_tail() {
        // Tests remove_range inner loop where retransmit starts AFTER ack start
        // and extends past ack end.
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOPQRST", false); // 20 bytes
        let mut out = [0u8; 20];
        buf.emit(&mut out);

        // Retransmit [7..17).
        buf.loss(7, 10);

        // Ack [5..15) — retransmit [7..15) removed, tail [15..17) remains.
        buf.ack(5, 10);

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (15, 2));
        assert_eq!(&out2, b"PQ");
    }

    #[test]
    fn send_loss_acked_covers_start_via_prev_range() {
        // Tests insert_non_acked where acked range at cursor extends past it.
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Non-contiguous ack: [5..8) only.
        buf.ack(5, 3);
        assert_eq!(buf.ack_off(), 0); // not contiguous from 0

        // Loss [5..10) — acked [5..8) covers [5..8), only [8..10) retransmitted.
        buf.loss(5, 5);
        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (8, 2));
    }

    #[test]
    fn send_loss_entirely_covered_by_noncontiguous_ack() {
        // Tests insert_non_acked early return when cursor >= end.
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Non-contiguous ack: [5..10) only.
        buf.ack(5, 5);

        // Loss [5..8) — entirely within acked [5..10).
        buf.loss(5, 3);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_adjacent_to_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Retransmit [0..5).
        buf.loss(0, 5);

        // Ack [5..10) — adjacent, does not overlap retransmit.
        buf.ack(5, 5);
        assert!(buf.has_retransmits()); // [0..5) still pending
    }

    #[test]
    fn send_ack_partial_overlap_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Retransmit [0..8).
        buf.loss(0, 8);

        // Ack [3..6) — splits retransmit into [0..3) and [6..8).
        buf.ack(3, 3);

        let mut out1 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out1);
        assert_eq!((off, n), (0, 3));

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (6, 2));
    }

    #[test]
    fn send_loss_acked_covers_start_exactly() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..5) — covers exactly the start.
        buf.ack(0, 5);

        // Loss [0..5) — fully acked, nothing to retransmit.
        buf.loss(0, 5);
        assert!(!buf.has_retransmits());

        // Loss [0..8) — only [5..8) should retransmit.
        buf.loss(0, 8);
        let mut out2 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (5, 3));
    }

    #[test]
    fn send_loss_acked_at_start() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Ack [0..6).
        buf.ack(0, 6);

        // Loss [0..10) — acked covers start, only [6..10) retransmitted.
        buf.loss(0, 10);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 4];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (6, 4));
        assert_eq!(&out2, b"GHIJ");
    }

    #[test]
    fn send_accessors() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.send_off(), 0);
        assert_eq!(buf.write_off(), 0);

        buf.write(b"hello", false);
        assert_eq!(buf.write_off(), 5);
        assert_eq!(buf.send_off(), 0);

        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(buf.send_off(), 5);
    }

    #[test]
    fn recv_in_order() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 5);
        assert_eq!(buf.ranges.len(), 1);

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.read_off(), 5);
        assert!(buf.ranges.is_empty());
    }

    #[test]
    fn recv_out_of_order() {
        let mut buf = RecvBuf::new(64);

        assert_eq!(buf.write(5, b"world", false).unwrap(), 5);
        assert_eq!(buf.readable(), 0);

        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 10);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_duplicate() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 0); // no new bytes

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn recv_overlapping() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();
        assert_eq!(buf.write(3, b"loworl", false).unwrap(), 0); // fully contained

        let mut out = [0u8; 10];
        assert_eq!(buf.read(&mut out), 10);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();

        let mut out = [0u8; 5];
        buf.read(&mut out);
        assert_eq!(buf.read_off(), 5);

        assert_eq!(buf.write(0, b"hell", false).unwrap(), 0);
    }

    #[test]
    fn recv_partial_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hel", false).unwrap();

        let mut out = [0u8; 3];
        buf.read(&mut out);
        assert_eq!(buf.read_off(), 3);

        // Only bytes [3, 5) are new.
        assert_eq!(buf.write(1, b"ello", false).unwrap(), 2);
        assert_eq!(buf.readable(), 2);

        let mut out2 = [0u8; 2];
        buf.read(&mut out2);
        assert_eq!(&out2, b"lo");
    }

    #[test]
    fn recv_flow_control() {
        let mut buf = RecvBuf::new(8);
        assert_eq!(buf.write(5, b"abcde", false), Err(RecvBufError::FlowControl));
        assert!(buf.ranges.is_empty());
    }

    #[test]
    fn recv_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"done", true).unwrap();

        let mut out = [0u8; 4];
        buf.read(&mut out);
        assert_eq!(&out, b"done");
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_fin_with_gap() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"world", true).unwrap();
        assert!(!buf.is_finished());

        buf.write(0, b"hello", false).unwrap();

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"helloworld");
        assert!(buf.is_finished());
    }

    #[test]
    fn recv_partial_read() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.readable(), 5);

        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"world");
    }

    #[test]
    fn recv_retransmit_overlaps_with_received() {
        let mut buf = RecvBuf::new(64);

        buf.write(5, b"FGHIJ", false).unwrap();
        assert_eq!(buf.readable(), 0);

        // Retransmit [0..7) overlaps [5..10).
        assert_eq!(buf.write(0, b"ABCDEFG", false).unwrap(), 5); // only 5 new
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJ");
    }

    #[test]
    fn recv_retransmit_bridges_three_ranges() {
        let mut buf = RecvBuf::new(64);

        buf.write(0, b"AB", false).unwrap();
        buf.write(5, b"FG", false).unwrap();
        buf.write(10, b"KL", false).unwrap();
        assert_eq!(buf.readable(), 2);

        // [1..11) bridges all three ranges.
        assert_eq!(buf.write(1, b"BCDEFGHIJK", false).unwrap(), 6); // 6 new bytes
        assert_eq!(buf.readable(), 12);

        let mut out = [0u8; 12];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJKL");
    }

    #[test]
    fn recv_multiple_gaps_then_fill() {
        let mut buf = RecvBuf::new(64);

        buf.write(10, b"cc", false).unwrap();
        buf.write(20, b"ee", false).unwrap();
        buf.write(0, b"aa", false).unwrap();
        assert_eq!(buf.readable(), 2);

        let mut out = [0u8; 2];
        buf.read(&mut out);
        assert_eq!(&out, b"aa");

        buf.write(2, b"bbbbbbbb", false).unwrap();
        assert_eq!(buf.readable(), 10);

        let mut out2 = [0u8; 10];
        buf.read(&mut out2);
        assert_eq!(&out2, b"bbbbbbbbcc");
    }

    #[test]
    fn recv_fin_size_mismatch() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap(); // fin at 5

        // Different fin offset → error.
        assert_eq!(
            buf.write(0, b"helloworld", true),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_data_past_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap(); // fin at 5

        // Non-fin data extending past fin → error.
        assert_eq!(
            buf.write(3, b"loworld", false),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_same_fin_twice_ok() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        // Same fin offset is fine.
        assert!(buf.write(0, b"hello", true).is_ok());
    }

    #[test]
    fn recv_full_duplicate_skips_ring_write() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        // Fully covered — returns 0, no redundant work.
        assert_eq!(buf.write(1, b"ell", false).unwrap(), 0);
    }

    #[test]
    fn recv_read_nothing_readable() {
        let mut buf = RecvBuf::new(64);
        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 0);

        // Gap at [0, 5) — data at 5 but nothing contiguous from 0.
        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.read(&mut out), 0);
    }

    #[test]
    fn recv_write_empty_with_fin() {
        let mut buf = RecvBuf::new(64);
        // Empty data with fin — sets fin but writes 0 bytes.
        assert_eq!(buf.write(5, b"", true).unwrap(), 0);
        assert!(!buf.is_finished()); // not finished, read_off=0 != fin=5
    }

    #[test]
    fn recv_is_readable() {
        let mut buf = RecvBuf::new(64);
        assert!(!buf.is_readable());

        buf.write(0, b"hi", false).unwrap();
        assert!(buf.is_readable());
    }

    #[test]
    fn recv_wrap_around() {
        let mut buf = RecvBuf::new(8);

        // Fill and read to move read_off forward.
        buf.write(0, b"abcdef", false).unwrap();
        let mut tmp = [0u8; 6];
        buf.read(&mut tmp);
        assert_eq!(buf.read_off(), 6);

        // Write data that wraps in the ring: offsets [6..14) → ring [6..8) + [0..6).
        buf.write(6, b"ghijklmn", false).unwrap();
        assert_eq!(buf.readable(), 8);

        let mut out = [0u8; 8];
        buf.read(&mut out);
        assert_eq!(&out, b"ghijklmn");
    }

    #[test]
    fn recv_readable_after_partial_read() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap(); // [0, 10)

        // Partial read: 3 bytes. read_off = 3, range still {0: 10}.
        let mut out = [0u8; 3];
        buf.read(&mut out);
        assert_eq!(&out, b"hel");
        assert_eq!(buf.read_off(), 3);

        // readable should be 7, not 10.
        assert_eq!(buf.readable(), 7);

        // Read the rest.
        let mut out2 = [0u8; 7];
        assert_eq!(buf.read(&mut out2), 7);
        assert_eq!(&out2, b"loworld");
    }

    #[test]
    fn recv_accessors() {
        let buf = RecvBuf::new(32);
        assert_eq!(buf.capacity(), 32);
        assert_eq!(buf.read_off(), 0);
    }

    #[test]
    fn recv_capacity_limits_total_stored() {
        let mut buf = RecvBuf::new(16);
        buf.write(0, b"abcdefghijklmnop", false).unwrap();
        assert_eq!(buf.ranges.len(), 1);

        // window_end = 0 + 16 = 16, offset 16 is out of window.
        assert_eq!(
            buf.write(16, b"q", false),
            Err(RecvBufError::FlowControl)
        );
    }
}
