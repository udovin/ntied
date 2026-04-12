use std::collections::{BTreeMap, VecDeque};

/// Send buffer backed by `VecDeque<u8>`.
///
/// # Layout
///
/// ```text
/// VecDeque: [ack_off .. send_off .. write_off]
///            ^front     ^emitted    ^back
/// ```
///
/// `base_off` is the stream offset of `VecDeque[0]`.  Index for stream offset
/// `x` is `(x - base_off) as usize`.  `drain(..n)` from front advances `base_off`.
///
/// # Invariants
///
/// - **I1**: `base_off == ack_off()` after every `ack()` that advances the front
/// - **I2**: `base_off <= send_off <= write_off`
/// - **I3**: `write_off - base_off == data.len()`
/// - **I4**: `write_off - base_off <= capacity`  (window limit)
/// - **I5**: retransmits ⊆ `[base_off, send_off)`
/// - **I6**: acked/retransmit ranges are sorted, non-overlapping, non-adjacent
/// - **I7**: `fin_off` is immutable once set
pub struct SendBuf {
    data: VecDeque<u8>,
    acked: BTreeMap<u64, u64>,
    retransmits: BTreeMap<u64, u64>,
    base_off: u64,
    send_off: u64,
    write_off: u64,
    fin_off: Option<u64>,
    capacity: usize,
    max_data: u64,
    blocked_at: Option<u64>,
}

impl SendBuf {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            data: VecDeque::with_capacity(capacity),
            acked: BTreeMap::new(),
            retransmits: BTreeMap::new(),
            base_off: 0,
            send_off: 0,
            write_off: 0,
            fin_off: None,
            capacity,
            max_data: capacity as u64,
            blocked_at: None,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// First contiguous acked offset.  Data before this is drained.
    pub fn ack_off(&self) -> u64 {
        self.base_off
    }

    /// How many bytes the application can write (deque free space).
    /// Window does NOT limit writes — only `emit()`.
    pub fn cap(&self) -> usize {
        self.capacity - self.data.len()
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
        self.data.is_empty()
    }

    /// All data has been acked including fin.
    pub fn is_finished(&self) -> bool {
        match self.fin_off {
            Some(fin) => self.base_off >= fin && self.data.is_empty(),
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

    pub fn max_data(&self) -> u64 {
        self.max_data
    }

    pub fn update_max_data(&mut self, max_data: u64) {
        if max_data > self.max_data {
            self.max_data = max_data;
            if self.blocked_at.is_some() {
                self.blocked_at = None;
            }
        }
    }

    pub fn is_blocked(&self) -> bool {
        self.send_off >= self.max_data && self.unsent() > 0
    }

    pub fn blocked_at(&self) -> Option<u64> {
        self.blocked_at
    }

    /// Write user data.  Limited by `cap()` (deque free space).
    /// Window only limits `emit()`.  After fin, further writes return 0.
    pub fn write(&mut self, data: &[u8], fin: bool) -> usize {
        if self.fin_off.is_some() {
            return 0;
        }

        let n = data.len().min(self.cap());
        if n > 0 {
            self.data.extend(&data[..n]);
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
    /// Retransmits are emitted first (I5 guarantees their data is in the deque).
    pub fn emit(&mut self, out: &mut [u8]) -> (u64, usize, bool) {
        if out.is_empty() {
            return (self.send_off, 0, false);
        }

        if let Some((&start, &end)) = self.retransmits.first_key_value() {
            let n = out.len().min((end - start) as usize);
            self.copy_out(start, &mut out[..n]);
            let emit_end = start + n as u64;

            self.retransmits.remove(&start);
            if emit_end < end {
                self.retransmits.insert(emit_end, end);
            }

            // I5: retransmits ⊆ [base_off, send_off), fin_off == write_off,
            // so emit_end <= send_off <= write_off == fin_off when fin.
            let fin = self.fin_off == Some(emit_end) && self.retransmits.is_empty();
            self.check_invariants();
            return (start, n, fin);
        }

        let window = self.max_data.saturating_sub(self.send_off) as usize;
        let n = out.len().min(self.unsent()).min(window);
        if n == 0 {
            if self.unsent() > 0 {
                self.blocked_at = Some(self.send_off);
            }
            let fin = self.fin_off == Some(self.send_off) && self.retransmits.is_empty();
            return (self.send_off, 0, fin);
        }
        let offset = self.send_off;
        self.copy_out(self.send_off, &mut out[..n]);
        self.send_off += n as u64;

        let fin = self.fin_off == Some(self.send_off) && self.retransmits.is_empty();
        self.check_invariants();
        (offset, n, fin)
    }

    /// Mark range `[offset, offset+len)` as acknowledged by peer.
    ///
    /// Drains contiguously acked data from the front of the deque.
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

        self.drain_acked();
        self.check_invariants();
    }

    /// Mark range `[offset, offset+len)` as lost, needing retransmission.
    pub fn loss(&mut self, offset: u64, len: usize) {
        if len == 0 {
            return;
        }
        let end = offset + len as u64;

        let start = offset.max(self.base_off);
        let end = end.min(self.send_off);
        if start >= end {
            return;
        }

        Self::insert_non_acked(&self.acked, &mut self.retransmits, start, end);

        self.check_invariants();
    }

    /// Drain contiguously acked bytes from the front.
    fn drain_acked(&mut self) {
        // Drain all contiguously acked ranges from the front.
        while let Some(entry) = self.acked.first_entry() {
            let &start = entry.key();
            let &end = entry.get();
            if start > self.base_off {
                break;
            }
            debug_assert!(end > self.base_off);
            let n = (end - self.base_off) as usize;
            // n > 0 guaranteed: end > base_off by debug_assert above.
            self.data.drain(..n);
            self.base_off = end;
            entry.remove();
        }

        // send_off may lag behind after non-contiguous ack+drain.
        self.send_off = self.send_off.max(self.base_off);
    }

    /// Read `n` bytes from the deque at the given stream offset.
    fn copy_out(&mut self, stream_off: u64, out: &mut [u8]) {
        let start = (stream_off - self.base_off) as usize;
        let slice = self.data.make_contiguous();
        out.copy_from_slice(&slice[start..start + out.len()]);
    }

    fn insert_range(map: &mut BTreeMap<u64, u64>, start: u64, end: u64) {
        let mut merged_start = start;
        let mut merged_end = end;

        if let Some((&rs, &re)) = map.range(..start).next_back() {
            if re >= start { // may be false if prev range ends before start
                merged_start = rs;
                merged_end = merged_end.max(re);
                map.remove(&rs);
            }
        }

        loop {
            let Some((&rs, &re)) = map.range(start..=merged_end).next() else {
                break;
            };
            merged_end = merged_end.max(re);
            map.remove(&rs);
        }

        map.insert(merged_start, merged_end);
    }

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

    fn insert_non_acked(
        acked: &BTreeMap<u64, u64>,
        retransmits: &mut BTreeMap<u64, u64>,
        start: u64,
        end: u64,
    ) {
        let mut cursor = start;

        if let Some((&_rs, &re)) = acked.range(..=cursor).next_back() {
            if re > cursor {
                cursor = re;
            }
        }

        if cursor >= end {
            return;
        }

        loop {
            let Some((&rs, &re)) = acked.range(cursor..end).next() else {
                break;
            };
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
            debug_assert!(self.base_off <= self.send_off, "base_off > send_off");
            debug_assert!(self.send_off <= self.write_off, "send_off > write_off");
            debug_assert_eq!(
                self.data.len(),
                (self.write_off - self.base_off) as usize,
                "deque len mismatch"
            );
            debug_assert!(self.data.len() <= self.capacity, "exceeds capacity");

            for (&rs, &re) in &self.retransmits {
                debug_assert!(rs >= self.base_off, "retransmit below base_off");
                debug_assert!(re <= self.send_off, "retransmit beyond send_off");
            }
        }
    }
}

/// Receive buffer backed by `BTreeMap<u64, Vec<u8>>`.
///
/// Data is stored as byte chunks keyed by start offset.  Gaps between
/// chunks represent missing data.  The total stored bytes are bounded
/// by `capacity` (= flow control window).
///
/// # Invariants
///
/// - **I1**: chunks are non-overlapping, non-adjacent (merged on insert)
/// - **I2**: no chunk contains bytes below `read_off`
/// - **I3**: `len` equals the sum of all chunk lengths
/// - **I4**: `len <= capacity`
/// - **I5**: `fin_off` is immutable once set; no data accepted past `fin_off`
pub struct RecvBuf {
    data: BTreeMap<u64, Vec<u8>>,
    read_off: u64,
    len: usize,
    capacity: usize,
    fin_off: Option<u64>,
    max_data: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvBufError {
    FlowControl,
    FinalSizeMismatch,
}

impl RecvBuf {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            data: BTreeMap::new(),
            read_off: 0,
            len: 0,
            capacity,
            fin_off: None,
            max_data: capacity as u64,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn read_off(&self) -> u64 {
        self.read_off
    }

    pub fn max_data(&self) -> u64 {
        self.max_data
    }

    pub fn max_data_next(&self) -> u64 {
        self.read_off + self.capacity as u64
    }

    pub fn should_update_max_data(&self) -> bool {
        self.fin_off.is_none()
            && (self.max_data - self.read_off) < (self.capacity as u64 / 2)
    }

    pub fn update_max_data(&mut self) {
        self.max_data = self.max_data_next();
    }

    pub fn window(&self) -> u64 {
        self.capacity as u64
    }

    /// Contiguous bytes available for reading. O(1).
    ///
    /// Since chunks are always merged (I1), the first chunk that covers
    /// `read_off` contains all contiguous data.
    pub fn readable(&self) -> usize {
        if let Some((&chunk_off, chunk)) = self.data.first_key_value() {
            if chunk_off <= self.read_off {
                let skip = (self.read_off - chunk_off) as usize;
                return chunk.len() - skip;
            }
        }
        0
    }

    pub fn is_readable(&self) -> bool {
        self.readable() > 0 || self.is_finished()
    }

    pub fn is_finished(&self) -> bool {
        self.fin_off == Some(self.read_off) && self.len == 0
    }

    /// Write received data.  Returns new bytes stored (0 for duplicates).
    /// Validates fin consistency (I5) and window bounds (I4) before mutation.
    pub fn write(
        &mut self,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, RecvBufError> {
        let end = offset + data.len() as u64;

        if fin {
            if let Some(existing) = self.fin_off {
                if existing != end {
                    return Err(RecvBufError::FinalSizeMismatch);
                }
            }
            self.fin_off = Some(end);
        } else if let Some(fin) = self.fin_off {
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

        let eff_start = offset.max(self.read_off);
        let skip = (eff_start - offset) as usize;
        let eff_data = &data[skip..];
        let eff_end = eff_start + eff_data.len() as u64;

        // Fast path: entirely covered by existing chunk.
        if let Some((&_rs, re_data)) = self.data.range(..=eff_start).next_back() {
            if _rs + re_data.len() as u64 >= eff_end {
                return Ok(0);
            }
        }

        let new_bytes = self.insert_non_overlapping(eff_start, eff_data, eff_end);

        self.check_invariants();
        Ok(new_bytes)
    }

    /// Read contiguous data into `out`.  Returns bytes read.
    /// Advances `read_off` and removes consumed chunks (preserving I2).
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.readable());
        if n == 0 {
            return 0;
        }

        let mut written = 0;
        while written < n {
            // first_entry is guaranteed: n <= readable(), and readable > 0
            // implies data is non-empty with a chunk covering read_off.
            let entry = self.data.first_entry().unwrap();
            let chunk_off = *entry.key();
            let chunk = entry.get();
            let skip = (self.read_off - chunk_off) as usize;
            let available = chunk.len() - skip;
            let to_copy = available.min(n - written);

            out[written..written + to_copy].copy_from_slice(&chunk[skip..skip + to_copy]);
            written += to_copy;
            self.read_off += to_copy as u64;

            if skip + to_copy >= chunk.len() {
                let removed = entry.remove();
                self.len -= removed.len();
            } else {
                // Partial read: trim consumed prefix, re-insert at read_off.
                let removed = entry.remove();
                let remaining = removed[skip + to_copy..].to_vec();
                self.len -= removed.len();
                self.len += remaining.len();
                self.data.insert(self.read_off, remaining);
                break;
            }
        }

        self.check_invariants();
        written
    }

    /// Insert only non-overlapping parts of `[off, eff_end)`.
    /// Returns the number of new bytes stored.
    fn insert_non_overlapping(&mut self, off: u64, data: &[u8], eff_end: u64) -> usize {
        let mut cursor = off;
        let mut new_bytes = 0;

        if let Some((&prev_off, prev_data)) = self.data.range(..=off).next_back() {
            let prev_end = prev_off + prev_data.len() as u64;
            if prev_end > cursor {
                cursor = prev_end;
            }
        }

        // cursor >= eff_end is caught by the fast path in write().
        debug_assert!(cursor < eff_end);

        let keys: Vec<u64> = self
            .data
            .range(cursor..eff_end)
            .map(|(&k, _)| k)
            .collect();

        for chunk_off in keys {
            // cursor < chunk_off guaranteed: chunks are non-overlapping, non-adjacent,
            // and we advance cursor to existing_end which is < next chunk_off.
            debug_assert!(cursor < chunk_off);
            let gap_end = chunk_off.min(eff_end);
            let d_start = (cursor - off) as usize;
            let d_end = (gap_end - off) as usize;
            let chunk = data[d_start..d_end].to_vec();
            new_bytes += chunk.len();
            self.data.insert(cursor, chunk);

            let existing = self.data.get(&chunk_off).unwrap();
            let existing_end = chunk_off + existing.len() as u64;
            cursor = existing_end;
        }

        if cursor < eff_end {
            let d_start = (cursor - off) as usize;
            let chunk = data[d_start..].to_vec();
            new_bytes += chunk.len();
            self.data.insert(cursor, chunk);
        }

        self.len += new_bytes;
        self.try_merge_around(off, eff_end);
        new_bytes
    }

    /// Merge adjacent chunks in the affected range.
    fn try_merge_around(&mut self, from: u64, to: u64) {
        let start_key = self
            .data
            .range(..from)
            .next_back()
            .map(|(&k, _)| k)
            .unwrap_or(from);

        let mut cursor = start_key;
        loop {
            // cursor always points to an existing chunk (start_key was found,
            // merging doesn't change the key).
            let cur_end = cursor + self.data.get(&cursor).unwrap().len() as u64;
            if cur_end > to {
                break;
            }
            // Remove next chunk if adjacent, append to current.
            let Some(next) = self.data.remove(&cur_end) else { break };
            self.data.get_mut(&cursor).unwrap().extend_from_slice(&next);
        }
    }

    #[inline]
    fn check_invariants(&self) {
        if cfg!(debug_assertions) {
            let actual_len: usize = self.data.values().map(|v| v.len()).sum();
            debug_assert_eq!(self.len, actual_len, "len mismatch");
            debug_assert!(self.len <= self.capacity, "len exceeds capacity");

            let mut prev_end: Option<u64> = None;
            for (&off, chunk) in &self.data {
                debug_assert!(!chunk.is_empty(), "empty chunk at {off}");
                if let Some(pe) = prev_end {
                    debug_assert!(off > pe, "overlapping/adjacent chunks at {pe} and {off}");
                }
                prev_end = Some(off + chunk.len() as u64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- SendBuf tests -------------------------------------------------------

    #[test]
    fn send_basic() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.free(), 16);
        assert_eq!(buf.unsent(), 0);

        assert_eq!(buf.write(b"hello", false), 5);
        assert_eq!(buf.unsent(), 5);

        let mut out = [0u8; 5];
        let (off, n, fin) = buf.emit(&mut out);
        assert_eq!((off, n), (0, 5));
        assert!(!fin);
        assert_eq!(&out, b"hello");

        buf.ack(0, 5);
        assert!(buf.is_empty());
        assert_eq!(buf.free(), 16);
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
        assert_eq!(buf.free(), 0);
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
        assert_eq!(buf.ack_off(), 4);

        buf.update_max_data(12);
        assert_eq!(buf.write(b"ijkl", false), 4);
        assert_eq!(buf.unsent(), 8);
    }

    #[test]
    fn send_loss_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);

        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(&out, b"ABCDE");
        buf.emit(&mut out);
        assert_eq!(&out, b"FGHIJ");
        assert_eq!(buf.unsent(), 0);

        buf.loss(0, 5);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (0, 5));
        assert_eq!(&out2, b"ABCDE");
    }

    #[test]
    fn send_loss_only_unacked_part() {
        let mut buf = SendBuf::new(64);
        buf.write(b"ABCDEFGHIJKLMNOP", false);
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        buf.ack(0, 5);
        buf.ack(10, 6);
        buf.loss(0, 16);

        let mut out2 = [0u8; 16];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (5, 5));
        assert_eq!(&out2[..5], b"FGHIJ");
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_removes_pending_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);
        let mut out = [0u8; 5];
        buf.emit(&mut out);

        buf.loss(0, 5);
        assert!(buf.has_retransmits());
        buf.ack(0, 5);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_overlapping_ranges() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false);
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Create non-contiguous acked range that won't be drained.
        buf.ack(2, 4); // acked = {2: 6}. Not from 0, stays.
        // Now ack overlapping range: insert_range(4, 8).
        // range(..4) finds (2, 6). re=6 >= start=4 → prev merge!
        buf.ack(4, 4);
        assert_eq!(buf.ack_off(), 0); // gap at [0..2)

        buf.ack(0, 2);
        assert_eq!(buf.ack_off(), 8);
    }

    #[test]
    fn send_noncontiguous_ack_gap_between() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false);
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        buf.ack(0, 5);
        buf.ack(10, 6);
        assert_eq!(buf.ack_off(), 5);
    }

    #[test]
    fn send_noncontiguous_ack_no_free() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        buf.ack(5, 5);
        assert_eq!(buf.ack_off(), 0);

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

        let mut small = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut small);
        assert_eq!((off, n), (0, 3));
        assert_eq!(&small, b"ABC");

        assert!(buf.has_retransmits());
        let mut rest = [0u8; 7];
        let (off, n, _fin) = buf.emit(&mut rest);
        assert_eq!((off, n), (3, 7));
        assert_eq!(&rest, b"DEFGHIJ");
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
            buf.update_max_data(buf.ack_off() + 4);
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
        let (_, _, fin) = buf.emit(&mut out);
        assert!(fin);
    }

    #[test]
    fn send_fin_partial_emit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"hello", true);

        let mut out = [0u8; 3];
        let (_, _, fin) = buf.emit(&mut out);
        assert!(!fin);

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

        buf.write(b"", true);
        assert_eq!(buf.fin_off(), Some(5));

        let (_, n, fin) = buf.emit(&mut [0u8; 1]);
        assert_eq!(n, 0);
        assert!(fin);
    }

    #[test]
    fn send_fin_on_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"end", true);
        let mut out = [0u8; 3];
        buf.emit(&mut out);

        buf.loss(0, 3);
        let (_, _, fin) = buf.emit(&mut out);
        assert!(fin);
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
        buf.ack(0, 0);
        assert_eq!(buf.ack_off(), 0);
    }

    #[test]
    fn send_loss_zero_len() {
        let mut buf = SendBuf::new(8);
        buf.write(b"abc", false);
        let mut out = [0u8; 3];
        buf.emit(&mut out);
        buf.loss(0, 0);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_loss_fully_clamped() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDE", false);
        let mut out = [0u8; 5];
        buf.emit(&mut out);
        buf.ack(0, 5);

        buf.loss(0, 5);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_splits_retransmit() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        buf.loss(0, 10);
        buf.ack(3, 4);

        let mut out1 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out1);
        assert_eq!((off, n), (0, 3));

        let mut out2 = [0u8; 3];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (7, 3));
    }

    #[test]
    fn send_ack_removes_multiple_retransmits() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false);
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        buf.loss(2, 3);
        buf.loss(8, 3);

        buf.ack(0, 16);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_ack_no_overlap_with_retransmit() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOP", false);
        let mut out = [0u8; 16];
        buf.emit(&mut out);

        // Retransmit [0..5).
        buf.loss(0, 5);

        // Ack [5..10) — does not overlap retransmit [0..5).
        buf.ack(5, 5);
        assert!(buf.has_retransmits()); // [0..5) still there
    }

    #[test]
    fn send_ack_trims_retransmit_tail() {
        let mut buf = SendBuf::new(32);
        buf.write(b"ABCDEFGHIJKLMNOPQRST", false);
        let mut out = [0u8; 20];
        buf.emit(&mut out);

        buf.loss(7, 10);
        buf.ack(5, 10);

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (15, 2));
    }

    #[test]
    fn send_loss_acked_covers_start_via_prev_range() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        buf.ack(5, 3);
        buf.loss(5, 5);

        let mut out2 = [0u8; 2];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (8, 2));
    }

    #[test]
    fn send_loss_at_acked_boundary() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        // Non-contiguous ack so it won't drain.
        buf.ack(2, 3); // acked = {2: 5}. Not from 0.
        // Loss starting exactly where acked ends.
        // insert_non_acked: cursor=5, range(..=5) finds (2,5). re=5 > cursor=5? No.
        buf.loss(5, 5);
        assert!(buf.has_retransmits());

        let mut out2 = [0u8; 5];
        let (off, n, _fin) = buf.emit(&mut out2);
        assert_eq!((off, n), (5, 5));
    }

    #[test]
    fn send_loss_entirely_covered_by_noncontiguous_ack() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJ", false);
        let mut out = [0u8; 10];
        buf.emit(&mut out);

        buf.ack(5, 5);
        buf.loss(5, 3);
        assert!(!buf.has_retransmits());
    }

    #[test]
    fn send_blocked_at_set_and_cleared() {
        let mut buf = SendBuf::new(16);
        buf.write(b"ABCDEFGHIJKLMNOP", false);

        let mut out = [0u8; 16];
        buf.emit(&mut out);

        buf.ack(0, 16);
        buf.write(b"QRST", false);
        assert_eq!(buf.unsent(), 4);

        let (_, n, _) = buf.emit(&mut out);
        assert_eq!(n, 0);
        assert!(buf.is_blocked());
        assert_eq!(buf.blocked_at(), Some(16));

        buf.update_max_data(24);
        assert_eq!(buf.blocked_at(), None);
        assert!(!buf.is_blocked());

        let (off, n, _fin) = buf.emit(&mut out);
        assert_eq!((off, n), (16, 4));
    }

    #[test]
    fn send_write_past_window_into_buffer() {
        let mut buf = SendBuf::new(64);
        assert_eq!(buf.write(&[0xAA; 64], false), 64);

        let mut out = [0u8; 64];
        buf.emit(&mut out);
        buf.ack(0, 64);

        assert_eq!(buf.write(b"HELLO", false), 5);
        assert_eq!(buf.cap(), 59);

        let (_, n, _) = buf.emit(&mut out);
        assert_eq!(n, 0);
        assert!(buf.is_blocked());
    }

    #[test]
    fn send_window_default_is_capacity() {
        let buf = SendBuf::new(256);
        assert_eq!(buf.max_data(), 256);
    }

    #[test]
    fn send_update_max_data_only_increases() {
        let mut buf = SendBuf::new(64);
        assert_eq!(buf.max_data(), 64);

        buf.update_max_data(100);
        assert_eq!(buf.max_data(), 100);

        buf.update_max_data(80);
        assert_eq!(buf.max_data(), 100);
    }

    #[test]
    fn send_copy_out_wraps_in_deque() {
        // Force VecDeque internal wrap by cycling many times.
        let mut buf = SendBuf::new(4);
        for round in 0u64..10 {
            let data = [b'A' + (round as u8 % 26); 4];
            buf.write(&data, false);

            let mut out = [0u8; 4];
            let (off, n, _fin) = buf.emit(&mut out);
            assert_eq!(n, 4);
            assert_eq!(out, data);

            buf.ack(off, n);
            buf.update_max_data(buf.ack_off() + 4);
        }
        // After many cycles, VecDeque head has wrapped.
        // Verify data is still correct.
        buf.write(b"WXYZ", false);
        let mut out = [0u8; 4];
        let (_, n, _) = buf.emit(&mut out);
        assert_eq!(n, 4);
        assert_eq!(&out, b"WXYZ");
    }

    #[test]
    fn send_accessors() {
        let mut buf = SendBuf::new(16);
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.send_off(), 0);
        assert_eq!(buf.write_off(), 0);

        buf.write(b"hello", false);
        assert_eq!(buf.write_off(), 5);

        let mut out = [0u8; 5];
        buf.emit(&mut out);
        assert_eq!(buf.send_off(), 5);
    }

    // -- RecvBuf tests -------------------------------------------------------

    #[test]
    fn recv_in_order() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(buf.readable(), 5);

        let mut out = [0u8; 5];
        assert_eq!(buf.read(&mut out), 5);
        assert_eq!(&out, b"hello");
        assert_eq!(buf.read_off(), 5);
    }

    #[test]
    fn recv_out_of_order() {
        let mut buf = RecvBuf::new(64);
        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.readable(), 0);

        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.readable(), 10);

        let mut out = [0u8; 10];
        buf.read(&mut out);
        assert_eq!(&out, b"helloworld");
    }

    #[test]
    fn recv_duplicate() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        assert_eq!(buf.write(0, b"hello", false).unwrap(), 0);
    }

    #[test]
    fn recv_overlapping() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"helloworld", false).unwrap();
        assert_eq!(buf.write(3, b"loworl", false).unwrap(), 0);
    }

    #[test]
    fn recv_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);

        assert_eq!(buf.write(0, b"hell", false).unwrap(), 0);
    }

    #[test]
    fn recv_partial_below_read_off() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hel", false).unwrap();
        let mut out = [0u8; 3];
        buf.read(&mut out);

        buf.update_max_data();
        assert_eq!(buf.write(1, b"ello", false).unwrap(), 2);
        assert_eq!(buf.readable(), 2);
    }

    #[test]
    fn recv_flow_control() {
        let mut buf = RecvBuf::new(8);
        assert_eq!(buf.write(5, b"abcde", false), Err(RecvBufError::FlowControl));
    }

    #[test]
    fn recv_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"done", true).unwrap();
        let mut out = [0u8; 4];
        buf.read(&mut out);
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
        assert_eq!(buf.write(0, b"ABCDEFG", false).unwrap(), 5);
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

        assert_eq!(buf.write(1, b"BCDEFGHIJK", false).unwrap(), 6);
        assert_eq!(buf.readable(), 12);

        let mut out = [0u8; 12];
        buf.read(&mut out);
        assert_eq!(&out, b"ABCDEFGHIJKL");
    }

    #[test]
    fn recv_fin_size_mismatch() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert_eq!(
            buf.write(0, b"helloworld", true),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_data_past_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert_eq!(
            buf.write(3, b"loworld", false),
            Err(RecvBufError::FinalSizeMismatch)
        );
    }

    #[test]
    fn recv_same_fin_twice_ok() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        assert!(buf.write(0, b"hello", true).is_ok());
    }

    #[test]
    fn recv_read_nothing_readable() {
        let mut buf = RecvBuf::new(64);
        let mut out = [0u8; 4];
        assert_eq!(buf.read(&mut out), 0);

        buf.write(5, b"world", false).unwrap();
        assert_eq!(buf.read(&mut out), 0);
    }

    #[test]
    fn recv_write_empty_with_fin() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.write(5, b"", true).unwrap(), 0);
        assert!(!buf.is_finished());
    }

    #[test]
    fn recv_is_readable() {
        let mut buf = RecvBuf::new(64);
        assert!(!buf.is_readable());

        buf.write(0, b"hi", false).unwrap();
        assert!(buf.is_readable());
    }

    #[test]
    fn recv_flow_control_window() {
        let mut buf = RecvBuf::new(64);
        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.window(), 64);

        buf.write(0, b"hello", false).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);

        assert_eq!(buf.max_data(), 64);
        assert_eq!(buf.max_data_next(), 5 + 64);

        let big = vec![0u8; 59];
        buf.update_max_data();
        buf.write(5, &big, false).unwrap();
        let mut tmp = [0u8; 33];
        buf.read(&mut tmp); // read_off = 38. remaining = 69 - 38 = 31 < 32

        assert!(buf.should_update_max_data());
        buf.update_max_data();
        assert_eq!(buf.max_data(), 38 + 64);
    }

    #[test]
    fn recv_should_update_not_after_fin() {
        let mut buf = RecvBuf::new(64);
        buf.write(0, b"hello", true).unwrap();
        let mut out = [0u8; 5];
        buf.read(&mut out);
        assert!(!buf.should_update_max_data());
    }

    #[test]
    fn recv_read_frees_and_allows_more() {
        let mut buf = RecvBuf::new(8);
        buf.write(0, b"abcdefgh", false).unwrap();

        let mut out = [0u8; 4];
        buf.read(&mut out);

        buf.update_max_data();
        buf.write(8, b"ijkl", false).unwrap();
        assert_eq!(buf.readable(), 8);
    }

    #[test]
    fn recv_accessors() {
        let buf = RecvBuf::new(32);
        assert_eq!(buf.capacity(), 32);
        assert_eq!(buf.read_off(), 0);
    }

    #[test]
    fn recv_capacity_limits() {
        let mut buf = RecvBuf::new(16);
        buf.write(0, b"abcdefghijklmnop", false).unwrap();
        assert_eq!(
            buf.write(16, b"q", false),
            Err(RecvBufError::FlowControl)
        );
    }
}
