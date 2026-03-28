use std::net::SocketAddr;
use std::time::{Duration, Instant};

use super::*;
use crate::crypto::{PeerId, PrivateKey};
use crate::wire::{
    DhtFindNode, DhtFindNodeReply, DhtQuery, DhtQueryReply, DhtStore, Frame, Reader, Writer,
};

fn test_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], port))
}

fn make_gateway_info(port: u16) -> GatewayInfo {
    let gw_id = PrivateKey::generate().public_key().peer_id();
    GatewayInfo {
        gateway_peer_id: gw_id,
        addrs: vec![test_addr(port)],
        latency_hint: 50,
    }
}

#[test]
fn dht_node_roundtrip() {
    let node = DhtNode {
        peer_id: PrivateKey::generate().public_key().peer_id(),
        addrs: vec![test_addr(3000), test_addr(3001)],
    };
    let mut w = Writer::new();
    node.encode(&mut w);
    let mut r = Reader::new(w.as_bytes());
    let decoded = DhtNode::decode(&mut r).unwrap();
    assert_eq!(decoded.peer_id, node.peer_id);
    assert_eq!(decoded.addrs, node.addrs);
}

#[test]
fn dht_record_sign_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(4000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    assert!(record.verify());
}

#[test]
fn dht_record_encode_decode_roundtrip() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(5000), make_gateway_info(5001)],
        RoutingPolicy::Open,
        42,
        100_000,
        &identity,
    );

    let encoded = record.encode();
    let decoded = DhtRecord::decode(&encoded).unwrap();

    assert!(decoded.verify());
    assert_eq!(decoded.peer_id, record.peer_id);
    assert_eq!(decoded.version, 42);
    assert_eq!(decoded.expires_at, 100_000);
    assert_eq!(decoded.gateways.len(), 2);
}

#[test]
fn dht_record_gateway_restricted_roundtrip() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();
    let gw_id = PrivateKey::generate().public_key().peer_id();

    let record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(6000)],
        RoutingPolicy::GatewayRestricted(vec![gw_id]),
        1,
        9999,
        &identity,
    );

    let encoded = record.encode();
    let decoded = DhtRecord::decode(&encoded).unwrap();

    assert!(decoded.verify());
    match &decoded.routing_policy {
        RoutingPolicy::GatewayRestricted(gws) => {
            assert_eq!(gws.len(), 1);
            assert_eq!(gws[0], gw_id);
        }
        _ => panic!("expected GatewayRestricted"),
    }
}

#[test]
fn dht_record_tampered_version_fails_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let mut record = DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(7000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    record.version = 999;
    assert!(!record.verify());
}

#[test]
fn dht_record_wrong_peer_id_fails_verify() {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let wrong_peer_id = PrivateKey::generate().public_key().peer_id();

    let record = DhtRecord::sign(
        wrong_peer_id,
        pk,
        vec![make_gateway_info(8000)],
        RoutingPolicy::Open,
        1,
        9999,
        &identity,
    );

    assert!(!record.verify());
}

// ---------------------------------------------------------------------------
// Distance tests
// ---------------------------------------------------------------------------

fn make_peer_id() -> PeerId {
    PrivateKey::generate().public_key().peer_id()
}

fn make_dht_node(peer_id: PeerId) -> DhtNode {
    DhtNode {
        peer_id,
        addrs: vec![test_addr(9000)],
    }
}

#[test]
fn xor_distance_self_is_zero() {
    let id = make_peer_id();
    let dist = xor_distance(&id, &id);
    assert!(dist.iter().all(|&b| b == 0));
}

#[test]
fn xor_distance_is_symmetric() {
    let a = make_peer_id();
    let b = make_peer_id();
    assert_eq!(xor_distance(&a, &b), xor_distance(&b, &a));
}

#[test]
fn leading_zeros_all_zero() {
    let id = make_peer_id();
    let dist = xor_distance(&id, &id);
    let lz = leading_zeros(&dist);
    assert_eq!(lz, dist.len() * 8);
}

#[test]
fn bucket_index_self_is_none() {
    let id = make_peer_id();
    assert_eq!(bucket_index(&id, &id), None);
}

#[test]
fn bucket_index_returns_valid_range() {
    let a = make_peer_id();
    let b = make_peer_id();
    if let Some(idx) = bucket_index(&a, &b) {
        assert!(idx < 256);
    }
}

#[test]
fn is_closer_consistent() {
    let target = make_peer_id();
    let a = make_peer_id();
    let b = make_peer_id();
    let a_closer = is_closer(&a, &b, &target);
    let b_closer = is_closer(&b, &a, &target);
    assert!(a_closer || b_closer || xor_distance(&a, &target) == xor_distance(&b, &target));
}

#[test]
fn sort_by_distance_orders_correctly() {
    let target = make_peer_id();
    let mut ids: Vec<PeerId> = (0..10).map(|_| make_peer_id()).collect();
    sort_by_distance(&mut ids, &target);
    for i in 1..ids.len() {
        assert!(xor_distance(&ids[i - 1], &target) <= xor_distance(&ids[i], &target));
    }
}

// ---------------------------------------------------------------------------
// K-bucket tests
// ---------------------------------------------------------------------------

#[test]
fn kbucket_table_insert_and_contains() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let node = make_dht_node(make_peer_id());
    let now = Instant::now();

    let result = table.insert(node.clone(), now);
    assert_eq!(result, InsertResult::Inserted);
    assert!(table.contains(&node.peer_id));
    assert_eq!(table.total_nodes(), 1);
}

#[test]
fn kbucket_table_insert_self_returns_updated() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let result = table.insert(make_dht_node(local), Instant::now());
    assert_eq!(result, InsertResult::Updated);
    assert_eq!(table.total_nodes(), 0);
}

#[test]
fn kbucket_table_update_existing() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let peer = make_peer_id();
    let now = Instant::now();

    table.insert(make_dht_node(peer), now);
    let result = table.insert(make_dht_node(peer), now);
    assert_eq!(result, InsertResult::Updated);
    assert_eq!(table.total_nodes(), 1);
}

#[test]
fn kbucket_table_remove() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let peer = make_peer_id();
    let now = Instant::now();

    table.insert(make_dht_node(peer), now);
    assert!(table.contains(&peer));
    table.remove(&peer);
    assert!(!table.contains(&peer));
    assert_eq!(table.total_nodes(), 0);
}

#[test]
fn kbucket_table_closest_returns_k_or_fewer() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let now = Instant::now();

    for _ in 0..30 {
        table.insert(make_dht_node(make_peer_id()), now);
    }

    let target = make_peer_id();
    let closest = table.closest(&target, 20);
    assert!(closest.len() <= 20);
    assert!(closest.len() <= table.total_nodes());

    for i in 1..closest.len() {
        let d_prev = xor_distance(&closest[i - 1].peer_id, &target);
        let d_curr = xor_distance(&closest[i].peer_id, &target);
        assert!(d_prev <= d_curr);
    }
}

#[test]
fn kbucket_table_evict_and_insert() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let now = Instant::now();

    let peer_a = make_peer_id();
    let idx_a = bucket_index(&local, &peer_a).unwrap();

    // find a peer_b that lands in the same bucket as peer_a
    let peer_b = loop {
        let candidate = make_peer_id();
        if bucket_index(&local, &candidate) == Some(idx_a) {
            break candidate;
        }
    };

    table.insert(make_dht_node(peer_a), now);
    assert!(table.contains(&peer_a));

    table.evict_and_insert(&peer_a, make_dht_node(peer_b), now);
    assert!(!table.contains(&peer_a));
    assert!(table.contains(&peer_b));
}

#[test]
fn kbucket_table_bucket_refresh_detection() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let now = Instant::now();
    let stale = Duration::from_secs(3600);

    let node = make_dht_node(make_peer_id());
    let idx = bucket_index(&local, &node.peer_id).unwrap();
    table.insert(node, now);

    assert!(!table.bucket_needs_refresh(idx, now + Duration::from_secs(10), stale));
    assert!(table.bucket_needs_refresh(idx, now + stale + Duration::from_secs(1), stale));
}

#[test]
fn kbucket_table_stale_bucket_indices() {
    let local = make_peer_id();
    let mut table = KBucketTable::new(local);
    let now = Instant::now();
    let stale = Duration::from_secs(60);

    for _ in 0..5 {
        table.insert(make_dht_node(make_peer_id()), now);
    }

    let stale_at = now + stale + Duration::from_secs(1);
    let indices = table.stale_bucket_indices(stale_at, stale);
    assert!(!indices.is_empty());
}

// ---------------------------------------------------------------------------
// Store tests
// ---------------------------------------------------------------------------

fn make_valid_record(version: u64, expires_at: u64) -> DhtRecord {
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();
    DhtRecord::sign(
        peer_id,
        pk,
        vec![make_gateway_info(9000)],
        RoutingPolicy::Open,
        version,
        expires_at,
        &identity,
    )
}

#[test]
fn store_put_and_get() {
    let mut store = RecordStore::new(100);
    let record = make_valid_record(1, 99999);
    let peer_id = record.peer_id;

    assert_eq!(store.put(record), PutResult::Stored);
    assert!(store.get(&peer_id).is_some());
    assert_eq!(store.len(), 1);
}

#[test]
fn store_rejects_stale_version() {
    let mut store = RecordStore::new(100);
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let r1 = DhtRecord::sign(
        peer_id, pk.clone(), vec![make_gateway_info(9000)],
        RoutingPolicy::Open, 5, 99999, &identity,
    );
    let r2 = DhtRecord::sign(
        peer_id, pk, vec![make_gateway_info(9001)],
        RoutingPolicy::Open, 3, 99999, &identity,
    );

    assert_eq!(store.put(r1), PutResult::Stored);
    assert_eq!(store.put(r2), PutResult::Stale);
}

#[test]
fn store_accepts_newer_version() {
    let mut store = RecordStore::new(100);
    let identity = PrivateKey::generate();
    let pk = identity.public_key();
    let peer_id = pk.peer_id();

    let r1 = DhtRecord::sign(
        peer_id, pk.clone(), vec![make_gateway_info(9000)],
        RoutingPolicy::Open, 1, 99999, &identity,
    );
    let r2 = DhtRecord::sign(
        peer_id, pk, vec![make_gateway_info(9001)],
        RoutingPolicy::Open, 2, 99999, &identity,
    );

    assert_eq!(store.put(r1), PutResult::Stored);
    assert_eq!(store.put(r2), PutResult::Stored);
    assert_eq!(store.get(&peer_id).unwrap().version, 2);
}

#[test]
fn store_rejects_when_full() {
    let mut store = RecordStore::new(2);
    let r1 = make_valid_record(1, 99999);
    let r2 = make_valid_record(1, 99999);
    let r3 = make_valid_record(1, 99999);

    assert_eq!(store.put(r1), PutResult::Stored);
    assert_eq!(store.put(r2), PutResult::Stored);
    assert_eq!(store.put(r3), PutResult::StoreFull);
}

#[test]
fn store_remove_expired() {
    let mut store = RecordStore::new(100);
    let r1 = make_valid_record(1, 100);
    let r2 = make_valid_record(1, 200);

    store.put(r1);
    store.put(r2);
    assert_eq!(store.len(), 2);

    store.remove_expired(150);
    assert_eq!(store.len(), 1);
}

#[test]
fn store_rejects_invalid_signature() {
    let mut store = RecordStore::new(100);
    let mut record = make_valid_record(1, 99999);
    record.version = 999; // tamper
    assert_eq!(store.put(record), PutResult::InvalidSignature);
}

// ---------------------------------------------------------------------------
// DhtHandler / protocol tests
// ---------------------------------------------------------------------------

#[test]
fn handler_handle_find_node_returns_closest() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..10 {
        handler.table_mut().insert(make_dht_node(make_peer_id()), now);
    }

    let from = make_peer_id();
    let target = make_peer_id();
    let msg = DhtFindNode {
        target,
        request_id: 42,
    };

    let reply = handler.handle_find_node(&from, &msg);
    match reply {
        Frame::DhtFindNodeReply(r) => {
            assert_eq!(r.request_id, 42);
            assert!(!r.nodes.is_empty());
            assert!(r.nodes.len() <= 20);
        }
        _ => panic!("expected DhtFindNodeReply"),
    }
}

#[test]
fn handler_handle_query_miss() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let from = make_peer_id();
    let target = make_peer_id();

    let reply = handler.handle_query(
        &from,
        &DhtQuery {
            target,
            request_id: 1,
        },
    );
    match reply {
        Frame::DhtQueryReply(r) => {
            assert_eq!(r.status, 1);
            assert!(r.data.is_empty());
        }
        _ => panic!("expected DhtQueryReply"),
    }
}

#[test]
fn handler_handle_store_valid_record() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let record = make_valid_record(1, 99999);
    let peer_id = record.peer_id;
    let encoded = record.encode();

    let result = handler.handle_store(&DhtStore {
        fragment_index: 0,
        fragment_total: 1,
        data: encoded,
    });
    assert_eq!(result, PutResult::Stored);
    assert!(handler.store().get(&peer_id).is_some());
}

#[test]
fn handler_handle_query_hit() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let record = make_valid_record(1, 99999);
    let target = record.peer_id;
    let encoded = record.encode();

    handler.handle_store(&DhtStore {
        fragment_index: 0,
        fragment_total: 1,
        data: encoded,
    });

    let from = make_peer_id();
    let reply = handler.handle_query(
        &from,
        &DhtQuery {
            target,
            request_id: 7,
        },
    );
    match reply {
        Frame::DhtQueryReply(r) => {
            assert_eq!(r.status, 0);
            assert_eq!(r.request_id, 7);
            let decoded = DhtRecord::decode(&r.data).unwrap();
            assert_eq!(decoded.peer_id, target);
        }
        _ => panic!("expected DhtQueryReply"),
    }
}

#[test]
fn handler_start_query_emits_find_node_actions() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..5 {
        handler.table_mut().insert(make_dht_node(make_peer_id()), now);
    }

    let target = make_peer_id();
    let (req_id, actions) = handler.start_query(target, now);
    assert!(req_id > 0);
    assert!(!actions.is_empty());
    assert!(actions.len() <= 3);

    for action in &actions {
        match action {
            DhtAction::SendTo { frame, .. } => match frame {
                Frame::DhtFindNode(f) => assert_eq!(f.target, target),
                _ => panic!("expected DhtFindNode frame"),
            },
            _ => panic!("expected SendTo action"),
        }
    }
}

#[test]
fn handler_start_publish_emits_find_node_actions() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..5 {
        handler.table_mut().insert(make_dht_node(make_peer_id()), now);
    }

    let record = make_valid_record(1, 99999);
    let target = record.peer_id;
    let (_, actions) = handler.start_publish(record, now);

    assert!(!actions.is_empty());
    for action in &actions {
        match action {
            DhtAction::SendTo { frame, .. } => match frame {
                Frame::DhtFindNode(f) => assert_eq!(f.target, target),
                _ => panic!("expected DhtFindNode"),
            },
            _ => panic!("expected SendTo"),
        }
    }
}

#[test]
fn handler_lookup_converges_with_no_new_nodes() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..3 {
        let p = make_peer_id();
        handler.table_mut().insert(make_dht_node(p), now);
    }

    let target = make_peer_id();
    let (_lookup_id, actions) = handler.start_query(target, now);

    let mut all_actions: Vec<DhtAction> = actions;
    for _ in 0..10 {
        let find_replies: Vec<(PeerId, DhtFindNodeReply)> = all_actions
            .iter()
            .filter_map(|a| match a {
                DhtAction::SendTo {
                    peer_id,
                    frame: Frame::DhtFindNode(f),
                } => Some((
                    *peer_id,
                    DhtFindNodeReply {
                        request_id: f.request_id,
                        nodes: vec![],
                    },
                )),
                _ => None,
            })
            .collect();

        if find_replies.is_empty() {
            break;
        }

        all_actions.clear();
        for (from, reply) in find_replies {
            all_actions.extend(handler.handle_find_node_reply(&from, reply, now));
        }
    }

    let has_terminal = all_actions.iter().any(|a| {
        matches!(
            a,
            DhtAction::QueryComplete { .. } | DhtAction::SendTo { frame: Frame::DhtQuery(_), .. }
        )
    });
    assert!(has_terminal, "lookup should converge to query phase or complete");
}

#[test]
fn handler_publish_converges_and_stores() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..5 {
        handler.table_mut().insert(make_dht_node(make_peer_id()), now);
    }

    let record = make_valid_record(1, 99999);
    let (_, actions) = handler.start_publish(record, now);

    let mut all_actions = actions;
    for _ in 0..10 {
        let find_replies: Vec<(PeerId, DhtFindNodeReply)> = all_actions
            .iter()
            .filter_map(|a| match a {
                DhtAction::SendTo {
                    peer_id,
                    frame: Frame::DhtFindNode(f),
                } => Some((
                    *peer_id,
                    DhtFindNodeReply {
                        request_id: f.request_id,
                        nodes: vec![],
                    },
                )),
                _ => None,
            })
            .collect();

        if find_replies.is_empty() {
            break;
        }

        all_actions.clear();
        for (from, reply) in find_replies {
            all_actions.extend(handler.handle_find_node_reply(&from, reply, now));
        }
    }

    let store_count = all_actions
        .iter()
        .filter(|a| matches!(a, DhtAction::SendTo { frame: Frame::DhtStore(_), .. }))
        .count();
    assert!(store_count > 0, "publish should send DhtStore after convergence");
}

#[test]
fn handler_refresh_converges_silently() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    for _ in 0..3 {
        handler.table_mut().insert(make_dht_node(make_peer_id()), now);
    }

    let target = make_peer_id();
    let (_, actions) = handler.start_refresh(target, now);

    let mut all_actions = actions;
    for _ in 0..10 {
        let find_replies: Vec<(PeerId, DhtFindNodeReply)> = all_actions
            .iter()
            .filter_map(|a| match a {
                DhtAction::SendTo {
                    peer_id,
                    frame: Frame::DhtFindNode(f),
                } => Some((
                    *peer_id,
                    DhtFindNodeReply {
                        request_id: f.request_id,
                        nodes: vec![],
                    },
                )),
                _ => None,
            })
            .collect();

        if find_replies.is_empty() {
            break;
        }

        all_actions.clear();
        for (from, reply) in find_replies {
            all_actions.extend(handler.handle_find_node_reply(&from, reply, now));
        }
    }

    let has_store = all_actions
        .iter()
        .any(|a| matches!(a, DhtAction::SendTo { frame: Frame::DhtStore(_), .. }));
    assert!(!has_store, "refresh should not send DhtStore");
}

#[test]
fn handler_query_reply_found_returns_complete() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();

    let responder = make_peer_id();
    handler.table_mut().insert(make_dht_node(responder), now);

    let record = make_valid_record(1, 99999);
    let target = record.peer_id;
    let encoded = record.encode();

    let (_lookup_id, actions) = handler.start_query(target, now);

    let find_req = actions.iter().find_map(|a| match a {
        DhtAction::SendTo {
            peer_id,
            frame: Frame::DhtFindNode(f),
        } => Some((*peer_id, f.request_id)),
        _ => None,
    });

    if let Some((from, req_id)) = find_req {
        let actions2 = handler.handle_find_node_reply(
            &from,
            DhtFindNodeReply {
                request_id: req_id,
                nodes: vec![],
            },
            now,
        );

        let query_req = actions2.iter().find_map(|a| match a {
            DhtAction::SendTo {
                peer_id,
                frame: Frame::DhtQuery(q),
            } => Some((*peer_id, q.request_id)),
            _ => None,
        });

        if let Some((qfrom, qreq_id)) = query_req {
            let actions3 = handler.handle_query_reply(
                &qfrom,
                DhtQueryReply {
                    request_id: qreq_id,
                    status: 0,
                    fragment_index: 0,
                    fragment_total: 1,
                    data: encoded,
                },
                now,
            );

            let complete = actions3.iter().find(|a| matches!(a, DhtAction::QueryComplete { .. }));
            assert!(complete.is_some(), "should emit QueryComplete on successful query reply");
            if let Some(DhtAction::QueryComplete { record: Some(r), .. }) = complete {
                assert_eq!(r.peer_id, target);
            }
        }
    }
}

#[test]
fn handler_remove_expired_cleans_store() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);

    let record = make_valid_record(1, 100);
    let encoded = record.encode();
    handler.handle_store(&DhtStore {
        fragment_index: 0,
        fragment_total: 1,
        data: encoded,
    });
    assert_eq!(handler.store().len(), 1);

    handler.remove_expired(200);
    assert_eq!(handler.store().len(), 0);
}

#[test]
fn handler_start_query_no_seeds_returns_empty() {
    let local = make_peer_id();
    let mut handler = DhtHandler::new(local);
    let now = Instant::now();
    let target = make_peer_id();

    let (_, actions) = handler.start_query(target, now);
    let has_complete = actions
        .iter()
        .any(|a| matches!(a, DhtAction::QueryComplete { record: None, .. }));
    assert!(has_complete, "query with no seeds should immediately complete with None");
}
