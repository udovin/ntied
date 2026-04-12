use std::collections::BTreeMap;

/// Assembles a message from out-of-order fragments.
///
/// Created with a maximum allowed size.  Fragments carry `(offset, data, fin)`.
/// The assembler grows its buffer as fragments arrive.  When `fin` is received,
/// the total size is known.  `is_complete()` returns true when all bytes
/// `[0, fin_off)` have been received.
pub struct MessageAssembler {
    data: Vec<u8>,
    received: BTreeMap<u64, u64>,
    fin_off: Option<u64>,
    max_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssemblerError {
    /// Fragment would make the message exceed `max_len`.
    TooLarge,
    /// A different `fin` offset was already established.
    FinalSizeMismatch,
}

impl MessageAssembler {
    pub fn new(max_len: u64) -> Self {
        Self {
            data: Vec::new(),
            received: BTreeMap::new(),
            fin_off: None,
            max_len,
        }
    }

    /// Write a fragment.  Returns the number of new bytes stored.
    pub fn write(
        &mut self,
        offset: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, AssemblerError> {
        if data.is_empty() && !fin {
            return Ok(0);
        }

        let end = offset + data.len() as u64;

        if fin {
            match self.fin_off {
                Some(existing) if existing != end => {
                    return Err(AssemblerError::FinalSizeMismatch);
                }
                None => {
                    if end > self.max_len {
                        return Err(AssemblerError::TooLarge);
                    }
                    self.fin_off = Some(end);
                }
                _ => {}
            }
        } else if let Some(fin_off) = self.fin_off {
            if end > fin_off {
                return Err(AssemblerError::FinalSizeMismatch);
            }
        } else if end > self.max_len {
            return Err(AssemblerError::TooLarge);
        }

        if data.is_empty() {
            return Ok(0);
        }

        // Fast path: fully covered already.
        if let Some((&_rs, &re)) = self.received.range(..=offset).next_back() {
            if re >= end {
                return Ok(0);
            }
        }

        // Append path (in-order): extend without zeroing.
        if offset as usize == self.data.len() {
            self.data.extend_from_slice(data);
        } else {
            if end as usize > self.data.len() {
                self.data.resize(end as usize, 0);
            }
            self.data[offset as usize..end as usize].copy_from_slice(data);
        }

        Ok(self.insert_range(offset, end))
    }

    /// True when fin has been received and all bytes `[0, fin_off)` are present.
    pub fn is_complete(&self) -> bool {
        let Some(fin_off) = self.fin_off else {
            return false;
        };
        matches!(
            self.received.first_key_value(),
            Some((&0, &end)) if end >= fin_off
        )
    }

    /// Take the assembled message.  Truncates to `fin_off` if known.
    pub fn take(mut self) -> Vec<u8> {
        if let Some(fin_off) = self.fin_off {
            self.data.truncate(fin_off as usize);
        }
        self.data
    }

    /// Current allocated buffer size in bytes.
    pub fn allocated(&self) -> usize {
        self.data.len()
    }

    /// The final message size, if fin has been received.
    pub fn fin_off(&self) -> Option<u64> {
        self.fin_off
    }

    /// Reset the assembler, discarding all received data.
    pub fn reset(&mut self) {
        self.data.clear();
        self.received.clear();
        self.fin_off = None;
    }

    fn insert_range(&mut self, start: u64, end: u64) -> usize {
        let mut merged_start = start;
        let mut merged_end = end;
        let mut already_covered: u64 = 0;

        if let Some((&rs, &re)) = self.received.range(..start).next_back() {
            if re >= start {
                already_covered += re.min(end) - start;
                merged_start = rs;
                merged_end = merged_end.max(re);
                self.received.remove(&rs);
            }
        }

        loop {
            let Some((&rs, &re)) = self.received.range(start..=merged_end).next() else {
                break;
            };
            let overlap_start = rs.max(start);
            let overlap_end = re.min(end);
            if overlap_end > overlap_start {
                already_covered += overlap_end - overlap_start;
            }
            merged_end = merged_end.max(re);
            self.received.remove(&rs);
        }

        self.received.insert(merged_start, merged_end);

        (end - start - already_covered) as usize
    }
}

/// Splits a message into fragments for transmission.
///
/// Owns the message data.  `emit()` writes the next fragment into a buffer.
/// Retransmits (via `loss()`) are emitted before new data.
/// The last fragment has `fin = true`.
pub struct MessageFragmenter {
    data: Vec<u8>,
    offset: u64,
    retransmits: BTreeMap<u64, u64>,
}

impl MessageFragmenter {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            offset: 0,
            retransmits: BTreeMap::new(),
        }
    }

    /// Total message length.
    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    /// Emit the next fragment into `out`.  Returns `(offset, bytes_written, fin)`.
    ///
    /// Retransmits are emitted first.
    /// Returns `None` when everything has been emitted.
    pub fn emit(&mut self, out: &mut [u8]) -> Option<(u64, usize, bool)> {
        if out.is_empty() {
            return None;
        }

        if let Some((&start, &end)) = self.retransmits.first_key_value() {
            let n = out.len().min((end - start) as usize);
            let emit_end = start + n as u64;

            self.retransmits.remove(&start);
            if emit_end < end {
                self.retransmits.insert(emit_end, end);
            }

            let s = start as usize;
            out[..n].copy_from_slice(&self.data[s..s + n]);
            let fin = emit_end == self.data.len() as u64 && self.retransmits.is_empty();
            return Some((start, n, fin));
        }

        if self.offset >= self.data.len() as u64 {
            return None;
        }
        let start = self.offset as usize;
        let n = out.len().min(self.data.len() - start);
        out[..n].copy_from_slice(&self.data[start..start + n]);
        self.offset += n as u64;

        let fin = self.offset == self.data.len() as u64 && self.retransmits.is_empty();
        Some((start as u64, n, fin))
    }

    /// Mark a range as lost, to be re-emitted.
    pub fn loss(&mut self, offset: u64, len: usize) {
        if len == 0 {
            return;
        }
        let end = (offset + len as u64).min(self.data.len() as u64);
        let start = offset.min(end);
        if start >= end {
            return;
        }

        let mut merged_start = start;
        let mut merged_end = end;

        if let Some((&rs, &re)) = self.retransmits.range(..start).next_back() {
            if re >= start {
                merged_start = rs;
                merged_end = merged_end.max(re);
                self.retransmits.remove(&rs);
            }
        }

        loop {
            let Some((&rs, &re)) = self.retransmits.range(start..=merged_end).next() else {
                break;
            };
            merged_end = merged_end.max(re);
            self.retransmits.remove(&rs);
        }

        self.retransmits.insert(merged_start, merged_end);
    }

    /// True when all data has been emitted and no retransmits pending.
    pub fn is_done(&self) -> bool {
        self.retransmits.is_empty() && self.offset >= self.data.len() as u64
    }

    pub fn has_retransmits(&self) -> bool {
        !self.retransmits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_in_order() {
        let mut a = MessageAssembler::new(1024);
        assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(a.write(5, b"world", true).unwrap(), 5);
        assert!(a.is_complete());
        assert_eq!(a.take(), b"helloworld");
    }

    #[test]
    fn assembler_out_of_order() {
        let mut a = MessageAssembler::new(1024);
        assert_eq!(a.write(5, b"world", true).unwrap(), 5);
        assert!(!a.is_complete());
        assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
        assert!(a.is_complete());
        assert_eq!(a.take(), b"helloworld");
    }

    #[test]
    fn assembler_duplicate() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"hello", true).unwrap();
        assert_eq!(a.write(0, b"hello", true).unwrap(), 0);
    }

    #[test]
    fn assembler_overlap() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"helloworld", true).unwrap();
        assert_eq!(a.write(3, b"loworl", false).unwrap(), 0);
    }

    #[test]
    fn assembler_too_large() {
        let mut a = MessageAssembler::new(5);
        assert_eq!(
            a.write(0, b"toolarge!", true),
            Err(AssemblerError::TooLarge)
        );
    }

    #[test]
    fn assembler_too_large_no_fin() {
        let mut a = MessageAssembler::new(5);
        assert_eq!(
            a.write(0, b"toolarge!", false),
            Err(AssemblerError::TooLarge)
        );
    }

    #[test]
    fn assembler_fin_mismatch() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"hello", true).unwrap();
        assert_eq!(
            a.write(0, b"helloworld", true),
            Err(AssemblerError::FinalSizeMismatch)
        );
    }

    #[test]
    fn assembler_data_past_fin() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"hello", true).unwrap(); // fin_off = 5
        assert_eq!(
            a.write(3, b"loworld", false),
            Err(AssemblerError::FinalSizeMismatch)
        );
    }

    #[test]
    fn assembler_empty_write() {
        let mut a = MessageAssembler::new(1024);
        assert_eq!(a.write(0, b"", false).unwrap(), 0);
    }

    #[test]
    fn assembler_empty_fin() {
        let mut a = MessageAssembler::new(1024);
        assert_eq!(a.write(0, b"hello", false).unwrap(), 5);
        assert_eq!(a.write(5, b"", true).unwrap(), 0);
        assert!(a.is_complete());
        assert_eq!(a.take(), b"hello");
    }

    #[test]
    fn assembler_bridges_ranges() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"AB", false).unwrap();
        a.write(5, b"FG", false).unwrap();
        assert_eq!(a.write(1, b"BCDEF", false).unwrap(), 3);
        assert_eq!(a.received.len(), 1);
    }

    #[test]
    fn assembler_non_adjacent_prev() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"AB", false).unwrap();
        a.write(5, b"FG", false).unwrap();
        assert_eq!(a.received.len(), 2);
    }

    #[test]
    fn assembler_fin_off() {
        let mut a = MessageAssembler::new(1024);
        assert_eq!(a.fin_off(), None);
        a.write(0, b"hi", true).unwrap();
        assert_eq!(a.fin_off(), Some(2));
    }

    #[test]
    fn assembler_take_incomplete_with_fin() {
        let mut a = MessageAssembler::new(1024);
        a.write(5, b"world", true).unwrap();
        assert!(!a.is_complete());
        let data = a.take();
        assert_eq!(data.len(), 10); // truncated to fin_off
    }

    #[test]
    fn assembler_incomplete_partial_from_zero() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"hel", false).unwrap();
        a.write(5, b"world", true).unwrap(); // fin_off = 10
        // received = {0:3, 5:10}. First range starts at 0 but ends at 3 < 10.
        assert!(!a.is_complete());
    }

    #[test]
    fn assembler_take_without_fin() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"partial", false).unwrap();
        assert!(!a.is_complete());
        let data = a.take();
        assert_eq!(data.len(), 7); // no truncation, raw buffer size
    }

    #[test]
    fn assembler_reset() {
        let mut a = MessageAssembler::new(1024);
        a.write(0, b"hello", true).unwrap();
        a.reset();
        assert!(!a.is_complete());
        assert_eq!(a.fin_off(), None);
    }

    #[test]
    fn assembler_grows_buffer() {
        let mut a = MessageAssembler::new(1024);
        // First fragment at offset 100 — buffer grows to 105.
        a.write(100, b"hello", false).unwrap();
        assert!(a.data.len() >= 105);
        // Then fill start.
        a.write(0, &vec![0u8; 100], false).unwrap();
        a.write(105, b"!", true).unwrap();
        assert!(a.is_complete());
    }

    #[test]
    fn fragmenter_basic() {
        let mut f = MessageFragmenter::new(b"helloworld".to_vec());
        assert_eq!(f.len(), 10);

        let mut out = [0u8; 5];
        let (off, n, fin) = f.emit(&mut out).unwrap();
        assert_eq!((off, n), (0, 5));
        assert!(!fin);
        assert_eq!(&out, b"hello");

        let (off, n, fin) = f.emit(&mut out).unwrap();
        assert_eq!((off, n), (5, 5));
        assert!(fin);
        assert_eq!(&out, b"world");

        assert!(f.emit(&mut out).is_none());
        assert!(f.is_done());
    }

    #[test]
    fn fragmenter_small_chunks() {
        let mut f = MessageFragmenter::new(b"abcdefgh".to_vec());
        let mut out = [0u8; 3];
        let mut fragments = Vec::new();
        while let Some((off, n, fin)) = f.emit(&mut out) {
            fragments.push((off, n, fin));
        }
        assert_eq!(fragments.len(), 3);
        assert!(!fragments[0].2); // not fin
        assert!(!fragments[1].2); // not fin
        assert!(fragments[2].2);  // fin on last
    }

    #[test]
    fn fragmenter_retransmit() {
        let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
        let mut tmp = [0u8; 5];
        while f.emit(&mut tmp).is_some() {}
        assert!(f.is_done());

        f.loss(0, 5);
        assert!(!f.is_done());

        let mut out = [0u8; 5];
        let (off, n, fin) = f.emit(&mut out).unwrap();
        assert_eq!((off, n), (0, 5));
        assert!(!fin); // not last — [5..10) already sent
        assert_eq!(&out, b"ABCDE");
        assert!(f.is_done());
    }

    #[test]
    fn fragmenter_retransmit_last_carries_fin() {
        let mut f = MessageFragmenter::new(b"ABCDE".to_vec());
        let mut tmp = [0u8; 5];
        f.emit(&mut tmp); // emit all
        assert!(f.is_done());

        f.loss(0, 5);
        let (_, _, fin) = f.emit(&mut tmp).unwrap();
        assert!(fin); // retransmit of entire message carries fin
    }

    #[test]
    fn fragmenter_partial_retransmit() {
        let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
        let mut tmp = [0u8; 10];
        while f.emit(&mut tmp).is_some() {}

        f.loss(0, 10);

        let mut small = [0u8; 3];
        let (off, n, _) = f.emit(&mut small).unwrap();
        assert_eq!((off, n), (0, 3));

        let mut rest = [0u8; 10];
        let (off, n, fin) = f.emit(&mut rest).unwrap();
        assert_eq!((off, n), (3, 7));
        assert!(fin);
    }

    #[test]
    fn fragmenter_loss_bridges() {
        let mut f = MessageFragmenter::new(b"ABCDEFGHIJ".to_vec());
        let mut tmp = [0u8; 10];
        while f.emit(&mut tmp).is_some() {}

        f.loss(0, 3);
        f.loss(5, 3);
        f.loss(2, 5);

        let mut out = [0u8; 10];
        let (off, n, _) = f.emit(&mut out).unwrap();
        assert_eq!((off, n), (0, 8));
    }

    #[test]
    fn fragmenter_loss_past_end() {
        let mut f = MessageFragmenter::new(b"hello".to_vec());
        let mut tmp = [0u8; 5];
        while f.emit(&mut tmp).is_some() {}
        f.loss(10, 5);
        assert!(f.is_done());
    }

    #[test]
    fn fragmenter_loss_clamped() {
        let mut f = MessageFragmenter::new(b"hello".to_vec());
        let mut tmp = [0u8; 5];
        while f.emit(&mut tmp).is_some() {}
        f.loss(3, 100);

        let mut out = [0u8; 10];
        let (off, n, fin) = f.emit(&mut out).unwrap();
        assert_eq!((off, n), (3, 2));
        assert!(fin);
    }

    #[test]
    fn fragmenter_loss_zero_len() {
        let mut f = MessageFragmenter::new(b"hello".to_vec());
        let mut tmp = [0u8; 5];
        while f.emit(&mut tmp).is_some() {}
        f.loss(0, 0);
        assert!(f.is_done());
    }

    #[test]
    fn fragmenter_emit_empty_buf() {
        let mut f = MessageFragmenter::new(b"hello".to_vec());
        assert!(f.emit(&mut []).is_none());
    }

    #[test]
    fn roundtrip() {
        let msg = b"The quick brown fox jumps over the lazy dog".to_vec();
        let mut frag = MessageFragmenter::new(msg.clone());
        let mut asm = MessageAssembler::new(1024);

        let mut buf = [0u8; 10];
        while let Some((off, n, fin)) = frag.emit(&mut buf) {
            asm.write(off, &buf[..n], fin).unwrap();
        }
        assert!(asm.is_complete());
        assert_eq!(asm.take(), msg);
    }
}
