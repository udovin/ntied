use std::time::Instant;

use tracing::debug;

use crate::dht::DhtRecord;
use crate::node::{
    flush_connection, DhtPublishCollector, Shared,
};
use crate::wire::Frame;

pub(crate) async fn process_registry_frame(shared: &Shared, connection_id: u64, frame: &Frame) {
    match frame {
        Frame::DhtFindNode(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&connection_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let from = match from {
                Some(id) => id,
                None => return,
            };
            if let Some(dht) = &mut state.dht_handler {
                let reply = dht.handle_find_node(&from, msg);
                if let Some(entry) = state.connections.get_mut(&connection_id) {
                    entry.conn.queue_frame(reply);
                }
            }
            drop(state);
            flush_connection(shared, connection_id).await.ok();
        }
        Frame::DhtFindNodeReply(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&connection_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let from = match from {
                Some(id) => id,
                None => return,
            };
            let actions = if let Some(dht) = &mut state.dht_handler {
                dht.handle_find_node_reply(&from, msg.clone(), Instant::now())
            } else {
                Vec::new()
            };
            drop(state);
            crate::registry::client::process_dht_actions(shared, actions).await;
        }
        Frame::DhtQuery(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&connection_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let from = match from {
                Some(id) => id,
                None => return,
            };

            let local_found = state
                .dht_handler
                .as_ref()
                .map_or(false, |dht| dht.store().get(&msg.target).is_some());

            debug!(?msg.target, local_found, store_len = state.dht_handler.as_ref().map_or(0, |d| d.store().len()), "gw: DhtQuery handling");
            if local_found {
                if let Some(dht) = &mut state.dht_handler {
                    let reply = dht.handle_query(&from, msg);
                    crate::registry::client::queue_fragmented_query_reply(&mut state, connection_id, reply);
                }
                drop(state);
                flush_connection(shared, connection_id).await.ok();
            } else {
                let peer_sessions: Vec<u64> =
                    state.gateway_peers.values().map(|p| p.connection_id).collect();
                if peer_sessions.is_empty() {
                    if let Some(dht) = &mut state.dht_handler {
                        let reply = dht.handle_query(&from, msg);
                        crate::registry::client::queue_fragmented_query_reply(&mut state, connection_id, reply);
                    }
                    drop(state);
                    flush_connection(shared, connection_id).await.ok();
                } else {
                    let gw_req_id = state.next_gw_query_id;
                    state.next_gw_query_id = state.next_gw_query_id.wrapping_add(1);
                    state.pending_gw_queries.insert(
                        gw_req_id,
                        crate::node::PendingGwQuery {
                            client_connection_id: connection_id,
                            client_request_id: msg.request_id,
                            remaining_peers: peer_sessions.len(),
                        },
                    );
                    debug!(target_peer = ?msg.target, gw_req_id, peers = peer_sessions.len(), "gw: DhtQuery miss, forwarding to peers");
                    for &peer_sid in &peer_sessions {
                        if let Some(entry) = state.connections.get_mut(&peer_sid) {
                            entry
                                .conn
                                .queue_frame(Frame::DhtQuery(crate::wire::DhtQuery {
                                    target: msg.target,
                                    request_id: gw_req_id,
                                }));
                        }
                    }
                    drop(state);
                    for &peer_sid in &peer_sessions {
                        flush_connection(shared, peer_sid).await.ok();
                    }
                }
            }
        }
        Frame::DhtQueryReply(msg) => {
            let mut state = shared.state.lock().await;

            if let Some(pending) = state.pending_gw_queries.get(&msg.request_id).cloned() {
                if msg.status == 0 {
                    let assembled_data = if msg.fragment_total <= 1 {
                        Some(msg.data.clone())
                    } else {
                        let key = msg.request_id as u64 | 0xC000_0000_0000_0000;
                        let collector =
                            state.dht_publish_fragments.entry(key).or_insert_with(|| {
                                DhtPublishCollector {
                                    fragments: vec![None; msg.fragment_total as usize],
                                    received: 0,
                                    total: msg.fragment_total,
                                }
                            });
                        let idx = msg.fragment_index as usize;
                        if idx < collector.fragments.len() && collector.fragments[idx].is_none() {
                            collector.fragments[idx] = Some(msg.data.clone());
                            collector.received += 1;
                        }
                        if collector.received == collector.total {
                            let data: Vec<u8> = collector
                                .fragments
                                .iter()
                                .filter_map(|f| f.as_ref())
                                .flat_map(|f| f.iter().copied())
                                .collect();
                            state.dht_publish_fragments.remove(&key);
                            Some(data)
                        } else {
                            None
                        }
                    };

                    if let Some(data) = assembled_data {
                        debug!(
                            gw_req_id = msg.request_id,
                            data_len = data.len(),
                            "gw: peer returned record, forwarding to client"
                        );

                        if let Some(dht) = &mut state.dht_handler {
                            if let Ok(record) = DhtRecord::decode(&data) {
                                dht.store_mut().put(record);
                            }
                        }

                        let client_reply = crate::wire::DhtQueryReply {
                            request_id: pending.client_request_id,
                            status: 0,
                            fragment_index: 0,
                            fragment_total: 1,
                            data,
                        };
                        crate::registry::client::queue_fragmented_query_reply(
                            &mut state,
                            pending.client_connection_id,
                            Frame::DhtQueryReply(client_reply),
                        );
                        state.pending_gw_queries.remove(&msg.request_id);
                        drop(state);
                        flush_connection(shared, pending.client_connection_id)
                            .await
                            .ok();
                    }
                } else {
                    let remaining = {
                        let p = state.pending_gw_queries.get_mut(&msg.request_id).unwrap();
                        p.remaining_peers -= 1;
                        p.remaining_peers
                    };
                    if remaining == 0 {
                        let not_found = Frame::DhtQueryReply(crate::wire::DhtQueryReply {
                            request_id: pending.client_request_id,
                            status: 1,
                            fragment_index: 0,
                            fragment_total: 1,
                            data: Vec::new(),
                        });
                        if let Some(entry) = state.connections.get_mut(&pending.client_connection_id) {
                            entry.conn.queue_frame(not_found);
                        }
                        state.pending_gw_queries.remove(&msg.request_id);
                        drop(state);
                        flush_connection(shared, pending.client_connection_id)
                            .await
                            .ok();
                    }
                }
            } else {
                let from = state
                    .connections
                    .get(&connection_id)
                    .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
                let from = match from {
                    Some(id) => id,
                    None => return,
                };
                let actions = if let Some(dht) = &mut state.dht_handler {
                    dht.handle_query_reply(&from, msg.clone(), Instant::now())
                } else {
                    Vec::new()
                };
                drop(state);
                crate::registry::client::process_dht_actions(shared, actions).await;
            }
        }
        Frame::DhtStore(msg) => {
            let mut state = shared.state.lock().await;

            let assembled =
                if msg.fragment_total <= 1 {
                    Some(msg.data.clone())
                } else {
                    let key = connection_id | 0x4000_0000_0000_0000;
                    let collector = state.dht_publish_fragments.entry(key).or_insert_with(|| {
                        DhtPublishCollector {
                            fragments: vec![None; msg.fragment_total as usize],
                            received: 0,
                            total: msg.fragment_total,
                        }
                    });
                    let idx = msg.fragment_index as usize;
                    if idx < collector.fragments.len() && collector.fragments[idx].is_none() {
                        collector.fragments[idx] = Some(msg.data.clone());
                        collector.received += 1;
                    }
                    if collector.received == collector.total {
                        let data: Vec<u8> = collector
                            .fragments
                            .iter()
                            .filter_map(|f| f.as_ref())
                            .flat_map(|f| f.iter().copied())
                            .collect();
                        state.dht_publish_fragments.remove(&key);
                        Some(data)
                    } else {
                        None
                    }
                };

            if let Some(data) = assembled {
                let assembled_msg = crate::wire::DhtStore {
                    fragment_index: 0,
                    fragment_total: 1,
                    data,
                };
                if let Some(dht) = &mut state.dht_handler {
                    let result = dht.handle_store(&assembled_msg);
                    debug!(
                        ?result,
                        store_len = dht.store().len(),
                        "gw: DhtStore received"
                    );
                }
            }
        }
        Frame::DhtPublish(msg) => {
            let mut state = shared.state.lock().await;

            let assembled = if msg.fragment_total <= 1 {
                Some(msg.data.clone())
            } else {
                let collector = state
                    .dht_publish_fragments
                    .entry(connection_id)
                    .or_insert_with(|| DhtPublishCollector {
                        fragments: vec![None; msg.fragment_total as usize],
                        received: 0,
                        total: msg.fragment_total,
                    });
                let idx = msg.fragment_index as usize;
                if idx < collector.fragments.len() && collector.fragments[idx].is_none() {
                    collector.fragments[idx] = Some(msg.data.clone());
                    collector.received += 1;
                }
                if collector.received == collector.total {
                    let data: Vec<u8> = collector
                        .fragments
                        .iter()
                        .filter_map(|f| f.as_ref())
                        .flat_map(|f| f.iter().copied())
                        .collect();
                    state.dht_publish_fragments.remove(&connection_id);
                    Some(data)
                } else {
                    None
                }
            };

            if let Some(data) = assembled {
                let assembled_msg = crate::wire::DhtPublish {
                    fragment_index: 0,
                    fragment_total: 1,
                    data,
                };
                let actions = if let Some(dht) = &mut state.dht_handler {
                    let (_result, actions) = dht.handle_publish(&assembled_msg, Instant::now());
                    actions
                } else {
                    Vec::new()
                };
                drop(state);
                crate::registry::client::process_dht_actions(shared, actions).await;
            }
        }
        _ => {}
    }
}
