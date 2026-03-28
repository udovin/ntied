use std::time::Instant;

use crate::crypto::PeerId;
use crate::dht::DhtNode;
use crate::dht::distance::{self, Distance};

const K: usize = 20;
const BUCKET_COUNT: usize = 256;

struct BucketEntry {
    node: DhtNode,
    last_seen: Instant,
}

struct KBucket {
    entries: Vec<BucketEntry>,
}

impl KBucket {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_full(&self) -> bool {
        self.entries.len() >= K
    }

    fn contains(&self, peer_id: &PeerId) -> bool {
        self.entries.iter().any(|e| e.node.peer_id == *peer_id)
    }

    fn insert(&mut self, node: DhtNode, now: Instant) -> InsertResult {
        if let Some(pos) = self.position(&node.peer_id) {
            self.entries[pos].node = node;
            self.entries[pos].last_seen = now;
            let entry = self.entries.remove(pos);
            self.entries.push(entry);
            return InsertResult::Updated;
        }
        if self.is_full() {
            return InsertResult::Full {
                oldest: self.entries[0].node.peer_id,
            };
        }
        self.entries.push(BucketEntry {
            node,
            last_seen: now,
        });
        InsertResult::Inserted
    }

    fn remove(&mut self, peer_id: &PeerId) -> bool {
        if let Some(pos) = self.position(peer_id) {
            self.entries.remove(pos);
            return true;
        }
        false
    }

    fn evict_oldest(&mut self) {
        if !self.entries.is_empty() {
            self.entries.remove(0);
        }
    }

    fn nodes(&self) -> impl Iterator<Item = &DhtNode> {
        self.entries.iter().map(|e| &e.node)
    }

    fn oldest_peer_id(&self) -> Option<PeerId> {
        self.entries.first().map(|e| e.node.peer_id)
    }

    fn position(&self, peer_id: &PeerId) -> Option<usize> {
        self.entries.iter().position(|e| e.node.peer_id == *peer_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InsertResult {
    Inserted,
    Updated,
    Full { oldest: PeerId },
}

pub struct KBucketTable {
    local_id: PeerId,
    buckets: Vec<KBucket>,
}

impl KBucketTable {
    pub fn new(local_id: PeerId) -> Self {
        let mut buckets = Vec::with_capacity(BUCKET_COUNT);
        for _ in 0..BUCKET_COUNT {
            buckets.push(KBucket::new());
        }
        Self { local_id, buckets }
    }

    pub fn local_id(&self) -> &PeerId {
        &self.local_id
    }

    pub fn insert(&mut self, node: DhtNode, now: Instant) -> InsertResult {
        if node.peer_id == self.local_id {
            return InsertResult::Updated;
        }
        let Some(idx) = distance::bucket_index(&self.local_id, &node.peer_id) else {
            return InsertResult::Updated;
        };
        self.buckets[idx].insert(node, now)
    }

    pub fn remove(&mut self, peer_id: &PeerId) {
        if let Some(idx) = distance::bucket_index(&self.local_id, peer_id) {
            self.buckets[idx].remove(peer_id);
        }
    }

    pub fn evict_and_insert(&mut self, evict: &PeerId, node: DhtNode, now: Instant) {
        if let Some(idx) = distance::bucket_index(&self.local_id, evict) {
            if self.buckets[idx].contains(evict) {
                self.buckets[idx].remove(evict);
                self.buckets[idx].insert(node, now);
            }
        }
    }

    pub fn closest(&self, target: &PeerId, count: usize) -> Vec<DhtNode> {
        let mut candidates: Vec<(Distance, &DhtNode)> = Vec::new();

        for bucket in &self.buckets {
            for node in bucket.nodes() {
                let dist = distance::xor_distance(&node.peer_id, target);
                candidates.push((dist, node));
            }
        }

        candidates.sort_by(|(da, _), (db, _)| da.cmp(db));
        candidates
            .into_iter()
            .take(count)
            .map(|(_, node)| node.clone())
            .collect()
    }

    pub fn contains(&self, peer_id: &PeerId) -> bool {
        let Some(idx) = distance::bucket_index(&self.local_id, peer_id) else {
            return false;
        };
        self.buckets[idx].contains(peer_id)
    }

    pub fn bucket_len(&self, index: usize) -> usize {
        if index < BUCKET_COUNT {
            self.buckets[index].len()
        } else {
            0
        }
    }

    pub fn total_nodes(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }

    pub fn bucket_needs_refresh(
        &self,
        index: usize,
        now: Instant,
        stale_after: std::time::Duration,
    ) -> bool {
        if index >= BUCKET_COUNT {
            return false;
        }
        let bucket = &self.buckets[index];
        if bucket.entries.is_empty() {
            return false;
        }
        bucket
            .entries
            .iter()
            .all(|e| now.duration_since(e.last_seen) > stale_after)
    }

    pub fn stale_bucket_indices(
        &self,
        now: Instant,
        stale_after: std::time::Duration,
    ) -> Vec<usize> {
        (0..BUCKET_COUNT)
            .filter(|&i| self.bucket_needs_refresh(i, now, stale_after))
            .collect()
    }
}
