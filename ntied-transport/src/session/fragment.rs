/// A generic buffer for assembling fragmented cryptographic frames.
///
/// Handles out-of-order delivery, duplicates, and automatically resets its
/// internal state if a new sequence of fragments with a different total length
/// is detected.
#[derive(Default)]
pub struct FragmentCollector {
    total: Option<u8>,
    fragments: Vec<Option<Vec<u8>>>,
    received_count: u8,
}

impl FragmentCollector {
    /// Creates a new, empty fragment collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a fragment to the collection.
    ///
    /// If this fragment completes the entire message payload, the assembled byte
    /// sequence is returned, and the internal state is automatically reset.
    /// Returns `None` if the payload is not yet complete.
    pub fn add_fragment(&mut self, index: u8, total: u8, data: &[u8]) -> Option<Vec<u8>> {
        if total == 0 || index >= total {
            return None;
        }

        if let Some(current_total) = self.total {
            if current_total != total {
                self.reset();
            }
        }

        if self.total.is_none() {
            self.total = Some(total);
            self.fragments = vec![None; total as usize];
        }

        if self.fragments[index as usize].is_none() {
            self.fragments[index as usize] = Some(data.to_vec());
            self.received_count += 1;
        }

        if self.received_count == total {
            let payload = self.assemble_payload();
            self.reset();
            Some(payload)
        } else {
            None
        }
    }

    /// Resets the internal state, discarding any partially assembled data.
    pub fn reset(&mut self) {
        self.total = None;
        self.fragments.clear();
        self.received_count = 0;
    }

    fn assemble_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        for fragment in &self.fragments {
            if let Some(data) = fragment {
                payload.extend_from_slice(data);
            }
        }
        payload
    }
}
