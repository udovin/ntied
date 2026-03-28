use std::collections::HashMap;

use crate::crypto::PeerId;
use crate::dht::DhtRecord;

pub struct RecordStore {
    records: HashMap<PeerId, DhtRecord>,
    max_records: usize,
}

impl RecordStore {
    pub fn new(max_records: usize) -> Self {
        Self {
            records: HashMap::new(),
            max_records,
        }
    }

    pub fn get(&self, peer_id: &PeerId) -> Option<&DhtRecord> {
        self.records.get(peer_id)
    }

    pub fn put(&mut self, record: DhtRecord) -> PutResult {
        if !record.verify() {
            return PutResult::InvalidSignature;
        }

        if let Some(existing) = self.records.get(&record.peer_id) {
            if record.version <= existing.version {
                return PutResult::Stale;
            }
        }

        if !self.records.contains_key(&record.peer_id) && self.records.len() >= self.max_records {
            return PutResult::StoreFull;
        }

        self.records.insert(record.peer_id, record);
        PutResult::Stored
    }

    pub fn remove(&mut self, peer_id: &PeerId) -> bool {
        self.records.remove(peer_id).is_some()
    }

    pub fn remove_expired(&mut self, now_unix: u64) {
        self.records.retain(|_, r| r.expires_at > now_unix);
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PeerId, &DhtRecord)> {
        self.records.iter()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PutResult {
    Stored,
    Stale,
    InvalidSignature,
    StoreFull,
}
