use std::collections::BTreeMap;

/// Assembles a message from out-of-order fragments.
///
/// Created with a maximum allowed size.  Fragments carry `(offset, data, fin)`.
/// The assembler grows its buffer as fragments arrive.  When `fin` is received,
/// the total size is known.  `is_complete()` returns true when all bytes
/// `[0, fin_off)` have been received.
pub struct MessageAssembler {
    pub(super) data: Vec<u8>,
    pub(super) received: BTreeMap<u64, u64>,
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

    /// Highest byte offset ever observed in a fragment (== `data.len()`).
    /// This is the receiver-side analogue of the sender's `max_offset_emitted`
    /// and the quantity the channel flow-control counts as "received" for
    /// per-message bookkeeping.
    pub fn max_offset_received(&self) -> u64 {
        self.data.len() as u64
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

    /// Highest byte offset ever handed out via `emit()` as a new fragment
    /// (retransmits do not advance this).  Used by the channel flow-control
    /// as the per-message "consumed" quantity, and as the `size` value in
    /// `ChannelEvict` frames.
    pub fn max_offset_emitted(&self) -> u64 {
        self.offset
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

    /// Mark a range as acknowledged. Removes it from retransmits.
    pub fn ack(&mut self, offset: u64, len: usize) {
        if len == 0 || self.retransmits.is_empty() {
            return;
        }
        let ack_end = offset + len as u64;

        // At most one range can start before offset and overlap; handle it first.
        if let Some((&rs, &re)) = self.retransmits.range(..offset).next_back() {
            if re > offset {
                self.retransmits.remove(&rs);
                self.retransmits.insert(rs, offset);
                if re > ack_end {
                    // This range spans both sides of the ACK; nothing else can overlap.
                    self.retransmits.insert(ack_end, re);
                    return;
                }
            }
        }

        // Drain ranges starting within [offset, ack_end). Only the last one can extend past ack_end.
        while let Some((&rs, &re)) = self.retransmits.range(offset..ack_end).next() {
            self.retransmits.remove(&rs);
            if re > ack_end {
                self.retransmits.insert(ack_end, re);
                break;
            }
        }
    }

    /// True when all data has been emitted and no retransmits pending.
    pub fn is_done(&self) -> bool {
        self.retransmits.is_empty() && self.offset >= self.data.len() as u64
    }

    pub fn has_retransmits(&self) -> bool {
        !self.retransmits.is_empty()
    }
}

