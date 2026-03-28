use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::crypto::PeerId;
use crate::dht::DhtNode;
use crate::dht::distance::{self, Distance};
use crate::dht::kbucket::{InsertResult, KBucketTable};
use crate::dht::record::DhtRecord;
use crate::dht::store::{PutResult, RecordStore};
use crate::wire::{DhtFindNode, DhtFindNodeReply, DhtQuery, DhtQueryReply, DhtStore, Frame};

const K: usize = 20;
const ALPHA: usize = 3;
const MAX_LOOKUPS: usize = 64;
const DEFAULT_MAX_RECORDS: usize = 10_000;

pub enum DhtAction {
    SendTo {
        peer_id: PeerId,
        frame: Frame,
    },
    QueryComplete {
        request_id: u32,
        record: Option<DhtRecord>,
    },
}

enum LookupKind {
    FindNode {
        on_complete: LookupGoal,
    },
    Query {
        query_sent: HashSet<PeerId>,
        query_pending: HashMap<u32, PeerId>,
    },
}

enum LookupGoal {
    Publish(DhtRecord),
    Refresh,
}

struct Lookup {
    target: PeerId,
    kind: LookupKind,
    queried: HashSet<PeerId>,
    candidates: Vec<CandidateEntry>,
    pending_find: HashMap<u32, PeerId>,
    converged: bool,
}

#[derive(Clone)]
struct CandidateEntry {
    node: DhtNode,
    dist: Distance,
}

pub struct DhtHandler {
    table: KBucketTable,
    store: RecordStore,
    lookups: HashMap<u32, Lookup>,
    next_request_id: u32,
}

impl DhtHandler {
    pub fn new(local_id: PeerId) -> Self {
        Self {
            table: KBucketTable::new(local_id),
            store: RecordStore::new(DEFAULT_MAX_RECORDS),
            lookups: HashMap::new(),
            next_request_id: 1,
        }
    }

    pub fn table(&self) -> &KBucketTable {
        &self.table
    }

    pub fn table_mut(&mut self) -> &mut KBucketTable {
        &mut self.table
    }

    pub fn store(&self) -> &RecordStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut RecordStore {
        &mut self.store
    }

    pub fn start_query(&mut self, target: PeerId, now: Instant) -> (u32, Vec<DhtAction>) {
        let request_id = self.alloc_request_id();
        let actions = self.begin_lookup(
            request_id,
            target,
            LookupKind::Query {
                query_sent: HashSet::new(),
                query_pending: HashMap::new(),
            },
            now,
        );
        (request_id, actions)
    }

    pub fn start_publish(&mut self, record: DhtRecord, now: Instant) -> (u32, Vec<DhtAction>) {
        let target = record.peer_id;
        let request_id = self.alloc_request_id();
        let actions = self.begin_lookup(
            request_id,
            target,
            LookupKind::FindNode {
                on_complete: LookupGoal::Publish(record),
            },
            now,
        );
        (request_id, actions)
    }

    pub fn start_refresh(&mut self, target: PeerId, now: Instant) -> (u32, Vec<DhtAction>) {
        let request_id = self.alloc_request_id();
        let actions = self.begin_lookup(
            request_id,
            target,
            LookupKind::FindNode {
                on_complete: LookupGoal::Refresh,
            },
            now,
        );
        (request_id, actions)
    }

    pub fn handle_find_node(&mut self, from: &PeerId, msg: &DhtFindNode) -> Frame {
        let closest = self.table.closest(&msg.target, K);
        Frame::DhtFindNodeReply(DhtFindNodeReply {
            request_id: msg.request_id,
            nodes: closest,
        })
    }

    pub fn handle_find_node_reply(
        &mut self,
        from: &PeerId,
        msg: DhtFindNodeReply,
        now: Instant,
    ) -> Vec<DhtAction> {
        let lookup_id = match self.find_lookup_by_find_request(msg.request_id) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.touch_node_from_reply(from, now);

        let local_id = *self.table.local_id();
        let filtered_nodes: Vec<DhtNode> = msg
            .nodes
            .iter()
            .filter(|n| n.peer_id != local_id)
            .cloned()
            .collect();

        for node in &filtered_nodes {
            self.table.insert(node.clone(), now);
        }

        let lookup = match self.lookups.get_mut(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        lookup.pending_find.remove(&msg.request_id);
        merge_candidates(&mut lookup.candidates, &filtered_nodes, &lookup.target);

        self.advance_lookup(lookup_id, now)
    }

    pub fn handle_query(&mut self, _from: &PeerId, msg: &DhtQuery) -> Frame {
        let (status, data) = match self.store.get(&msg.target) {
            Some(record) => (0u8, record.encode()),
            None => (1u8, Vec::new()),
        };

        Frame::DhtQueryReply(DhtQueryReply {
            request_id: msg.request_id,
            status,
            fragment_index: 0,
            fragment_total: 1,
            data,
        })
    }

    pub fn handle_query_reply(
        &mut self,
        from: &PeerId,
        msg: DhtQueryReply,
        now: Instant,
    ) -> Vec<DhtAction> {
        let lookup_id = match self.find_lookup_by_query_request(msg.request_id) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.touch_node_from_reply(from, now);

        if msg.status == 0 {
            if let Ok(record) = DhtRecord::decode(&msg.data) {
                if record.verify() && record.peer_id == self.lookups[&lookup_id].target {
                    self.store.put(record.clone());
                    self.lookups.remove(&lookup_id);
                    return vec![DhtAction::QueryComplete {
                        request_id: lookup_id,
                        record: Some(record),
                    }];
                }
            }
        }

        let lookup = match self.lookups.get_mut(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        if let LookupKind::Query { query_pending, .. } = &mut lookup.kind {
            query_pending.remove(&msg.request_id);
        }

        self.advance_lookup(lookup_id, now)
    }

    pub fn handle_store(&mut self, msg: &DhtStore) -> PutResult {
        match DhtRecord::decode(&msg.data) {
            Ok(record) => self.store.put(record),
            Err(_) => PutResult::InvalidSignature,
        }
    }

    pub fn handle_publish(
        &mut self,
        msg: &crate::wire::DhtPublish,
        now: Instant,
    ) -> (PutResult, Vec<DhtAction>) {
        let record = match DhtRecord::decode(&msg.data) {
            Ok(r) => r,
            Err(_) => return (PutResult::InvalidSignature, Vec::new()),
        };

        let put_result = self.store.put(record.clone());
        if put_result != PutResult::Stored {
            return (put_result, Vec::new());
        }

        let (_, actions) = self.start_publish(record, now);
        (PutResult::Stored, actions)
    }

    pub fn remove_expired(&mut self, now_unix: u64) {
        self.store.remove_expired(now_unix);
    }

    fn begin_lookup(
        &mut self,
        lookup_id: u32,
        target: PeerId,
        kind: LookupKind,
        now: Instant,
    ) -> Vec<DhtAction> {
        if self.lookups.len() >= MAX_LOOKUPS {
            return Vec::new();
        }

        let closest = self.table.closest(&target, K);
        let candidates = build_candidates(&closest, &target);

        let lookup = Lookup {
            target,
            kind,
            queried: HashSet::new(),
            candidates,
            pending_find: HashMap::new(),
            converged: false,
        };
        self.lookups.insert(lookup_id, lookup);

        self.advance_lookup(lookup_id, now)
    }

    fn advance_lookup(&mut self, lookup_id: u32, now: Instant) -> Vec<DhtAction> {
        let lookup = match self.lookups.get(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        if lookup.converged {
            return self.advance_post_converge(lookup_id, now);
        }

        let to_query = pick_alpha_unqueried(
            &lookup.candidates,
            &lookup.queried,
            &pending_peers(&lookup.pending_find),
            ALPHA.saturating_sub(lookup.pending_find.len()),
        );

        if to_query.is_empty() && lookup.pending_find.is_empty() {
            if let Some(l) = self.lookups.get_mut(&lookup_id) {
                l.converged = true;
            }
            return self.advance_post_converge(lookup_id, now);
        }

        let target = lookup.target;
        let mut actions = Vec::new();

        for node in to_query {
            let req_id = self.alloc_request_id();
            let peer_id = node.peer_id;

            if let Some(l) = self.lookups.get_mut(&lookup_id) {
                l.queried.insert(peer_id);
                l.pending_find.insert(req_id, peer_id);
            }

            actions.push(DhtAction::SendTo {
                peer_id,
                frame: Frame::DhtFindNode(DhtFindNode {
                    target,
                    request_id: req_id,
                }),
            });
        }

        actions
    }

    fn advance_post_converge(&mut self, lookup_id: u32, _now: Instant) -> Vec<DhtAction> {
        let lookup = match self.lookups.get(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        match &lookup.kind {
            LookupKind::FindNode { .. } => self.complete_find_node(lookup_id),
            LookupKind::Query { .. } => self.advance_query_phase(lookup_id),
        }
    }

    fn complete_find_node(&mut self, lookup_id: u32) -> Vec<DhtAction> {
        let lookup = match self.lookups.remove(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        let k_closest: Vec<DhtNode> = lookup
            .candidates
            .iter()
            .take(K)
            .map(|c| c.node.clone())
            .collect();

        match lookup.kind {
            LookupKind::FindNode {
                on_complete: LookupGoal::Publish(record),
            } => {
                let encoded = record.encode();
                k_closest
                    .iter()
                    .map(|node| DhtAction::SendTo {
                        peer_id: node.peer_id,
                        frame: Frame::DhtStore(DhtStore {
                            fragment_index: 0,
                            fragment_total: 1,
                            data: encoded.clone(),
                        }),
                    })
                    .collect()
            }
            LookupKind::FindNode {
                on_complete: LookupGoal::Refresh,
            } => Vec::new(),
            LookupKind::Query { .. } => Vec::new(),
        }
    }

    fn advance_query_phase(&mut self, lookup_id: u32) -> Vec<DhtAction> {
        let lookup = match self.lookups.get_mut(&lookup_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        let (query_sent, query_pending) = match &mut lookup.kind {
            LookupKind::Query {
                query_sent,
                query_pending,
            } => (query_sent, query_pending),
            _ => return Vec::new(),
        };

        if !query_pending.is_empty() {
            return Vec::new();
        }

        let target = lookup.target;
        let to_query: Vec<DhtNode> = lookup
            .candidates
            .iter()
            .filter(|c| !query_sent.contains(&c.node.peer_id))
            .take(K)
            .map(|c| c.node.clone())
            .collect();

        if to_query.is_empty() {
            self.lookups.remove(&lookup_id);
            return vec![DhtAction::QueryComplete {
                request_id: lookup_id,
                record: None,
            }];
        }

        let mut actions = Vec::new();
        for node in to_query {
            let req_id = self.alloc_request_id();

            let lookup = self.lookups.get_mut(&lookup_id).unwrap();
            if let LookupKind::Query {
                query_sent,
                query_pending,
            } = &mut lookup.kind
            {
                query_sent.insert(node.peer_id);
                query_pending.insert(req_id, node.peer_id);
            }

            actions.push(DhtAction::SendTo {
                peer_id: node.peer_id,
                frame: Frame::DhtQuery(DhtQuery {
                    target,
                    request_id: req_id,
                }),
            });
        }

        actions
    }

    fn find_lookup_by_find_request(&self, request_id: u32) -> Option<u32> {
        self.lookups
            .iter()
            .find(|(_, l)| l.pending_find.contains_key(&request_id))
            .map(|(&id, _)| id)
    }

    fn find_lookup_by_query_request(&self, request_id: u32) -> Option<u32> {
        self.lookups.iter().find(|(_, l)| {
            matches!(&l.kind, LookupKind::Query { query_pending, .. } if query_pending.contains_key(&request_id))
        }).map(|(&id, _)| id)
    }

    fn touch_node_from_reply(&mut self, from: &PeerId, now: Instant) {
        if self.table.contains(from) {
            self.table.insert(
                DhtNode {
                    peer_id: *from,
                    addrs: Vec::new(),
                },
                now,
            );
        }
    }

    fn alloc_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }
}

fn build_candidates(nodes: &[DhtNode], target: &PeerId) -> Vec<CandidateEntry> {
    let mut candidates: Vec<CandidateEntry> = nodes
        .iter()
        .map(|n| CandidateEntry {
            dist: distance::xor_distance(&n.peer_id, target),
            node: n.clone(),
        })
        .collect();
    candidates.sort_by(|a, b| a.dist.cmp(&b.dist));
    candidates
}

fn merge_candidates(candidates: &mut Vec<CandidateEntry>, new_nodes: &[DhtNode], target: &PeerId) {
    let existing: HashSet<PeerId> = candidates.iter().map(|c| c.node.peer_id).collect();
    for node in new_nodes {
        if existing.contains(&node.peer_id) {
            continue;
        }
        candidates.push(CandidateEntry {
            dist: distance::xor_distance(&node.peer_id, target),
            node: node.clone(),
        });
    }
    candidates.sort_by(|a, b| a.dist.cmp(&b.dist));
    candidates.truncate(K * 2);
}

fn pick_alpha_unqueried(
    candidates: &[CandidateEntry],
    queried: &HashSet<PeerId>,
    pending: &HashSet<PeerId>,
    count: usize,
) -> Vec<DhtNode> {
    candidates
        .iter()
        .filter(|c| !queried.contains(&c.node.peer_id) && !pending.contains(&c.node.peer_id))
        .take(count)
        .map(|c| c.node.clone())
        .collect()
}

fn pending_peers(pending: &HashMap<u32, PeerId>) -> HashSet<PeerId> {
    pending.values().copied().collect()
}
