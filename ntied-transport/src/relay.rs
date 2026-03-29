// RelayService: client registration, packet forwarding

use std::net::SocketAddr;
use std::time::Instant;

use tracing::{debug, info, warn};

use crate::crypto::{compute_transcript_hash, EncryptionKeys, EphemeralPrivateKey, PeerId};
use crate::dht::DhtRecord;
use crate::net::PeerConnection;
use crate::node::{
    build_auth_payload, find_session_by_receiver, flush_connection, flush_connection_locked,
    send_packets, short_pid, ConnEntry, DhtPublishCollector, GatewayPeer, RegisteredClient, Shared,
    TransportPath, GATEWAY_PEER_FLAG,
};
use crate::session::{Role, Session};
use crate::wire::packet::{Data, Packet};
use crate::wire::{Frame, GatewayPacket, KeyExchangeInit, KeyExchangeResponse};

pub(crate) async fn process_gateway_server_frame(shared: &Shared, session_id: u64, frame: &Frame) {
    match frame {
        Frame::GatewayRegister(reg) => {
            let mut state = shared.state.lock().await;
            let external_addr = match state.connections.get(&session_id) {
                Some(entry) => match &entry.path {
                    TransportPath::Direct { addr } => *addr,
                    _ => return,
                },
                None => return,
            };

            if reg.flags & GATEWAY_PEER_FLAG != 0 {
                // Gateway peer registration
                debug!(peer_id = ?reg.peer_id, session_id, "gw: peer gateway registered");
                state.gateway_peers.insert(
                    reg.peer_id,
                    GatewayPeer {
                        session_id,
                        addr: external_addr,
                    },
                );
                if let Some(dht) = &mut state.dht_handler {
                    dht.table_mut().insert(
                        crate::dht::DhtNode {
                            peer_id: reg.peer_id,
                            addrs: vec![external_addr],
                        },
                        Instant::now(),
                    );
                }
            } else {
                // Client registration
                debug!(peer_id = ?reg.peer_id, session_id, "gw: client registered");
                state.gateway_clients.insert(
                    reg.peer_id,
                    RegisteredClient {
                        session_id,
                        external_addr,
                    },
                );
            }

            if let Some(entry) = state.connections.get_mut(&session_id) {
                entry.conn.queue_frame(Frame::GatewayRegisterAck(
                    crate::wire::GatewayRegisterAck {
                        status: 0,
                        relay_mtu: (crate::wire::packet::INITIAL_MTU
                            - crate::wire::packet::PACKET_OVERHEAD
                            - 36) as u16,
                    },
                ));
            }
            drop(state);
            flush_connection(shared, session_id).await.ok();
        }
        Frame::GatewayPacket(pkt) => {
            // Simple local delivery only.
            // Client connects directly to the relay where the target peer is.
            // No relay-to-relay forwarding.
            let state = shared.state.lock().await;
            let dest_client = state.gateway_clients.get(&pkt.dest_peer_id).cloned();
            drop(state);

            if let Some(dest) = dest_client {
                let deliver = Frame::GatewayPacket(GatewayPacket {
                    dest_peer_id: pkt.dest_peer_id,
                    src_peer_id: pkt.src_peer_id,
                    inner: pkt.inner.clone(),
                });
                let mut state = shared.state.lock().await;
                if let Some(entry) = state.connections.get_mut(&dest.session_id) {
                    entry.conn.queue_frame(deliver);
                }
                drop(state);
                flush_connection(shared, dest.session_id).await.ok();
            } else {
                warn!(dest = ?pkt.dest_peer_id, "gw: dest not found locally");
            }
        }
        Frame::DhtFindNode(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&session_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let from = match from {
                Some(id) => id,
                None => return,
            };
            if let Some(dht) = &mut state.dht_handler {
                let reply = dht.handle_find_node(&from, msg);
                if let Some(entry) = state.connections.get_mut(&session_id) {
                    entry.conn.queue_frame(reply);
                }
            }
            drop(state);
            flush_connection(shared, session_id).await.ok();
        }
        Frame::DhtFindNodeReply(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&session_id)
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
            crate::node::process_dht_actions(shared, actions).await;
        }
        Frame::DhtQuery(msg) => {
            let mut state = shared.state.lock().await;
            let from = state
                .connections
                .get(&session_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let from = match from {
                Some(id) => id,
                None => return,
            };

            // Check local store first
            let local_found = state
                .dht_handler
                .as_ref()
                .map_or(false, |dht| dht.store().get(&msg.target).is_some());

            debug!(?msg.target, local_found, store_len = state.dht_handler.as_ref().map_or(0, |d| d.store().len()), "gw: DhtQuery handling");
            if local_found {
                // Found locally — reply directly
                if let Some(dht) = &mut state.dht_handler {
                    let reply = dht.handle_query(&from, msg);
                    crate::node::queue_fragmented_query_reply(&mut state, session_id, reply);
                }
                drop(state);
                flush_connection(shared, session_id).await.ok();
            } else {
                // Not found locally — forward query to peer GWs
                let peer_sessions: Vec<u64> =
                    state.gateway_peers.values().map(|p| p.session_id).collect();
                if peer_sessions.is_empty() {
                    // No peers — reply not found
                    if let Some(dht) = &mut state.dht_handler {
                        let reply = dht.handle_query(&from, msg);
                        crate::node::queue_fragmented_query_reply(&mut state, session_id, reply);
                    }
                    drop(state);
                    flush_connection(shared, session_id).await.ok();
                } else {
                    let gw_req_id = state.next_gw_query_id;
                    state.next_gw_query_id = state.next_gw_query_id.wrapping_add(1);
                    state.pending_gw_queries.insert(
                        gw_req_id,
                        crate::node::PendingGwQuery {
                            client_session_id: session_id,
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

            // Check if this is a response to a forwarded GW query
            if let Some(pending) = state.pending_gw_queries.get(&msg.request_id).cloned() {
                if msg.status == 0 {
                    // Reassemble fragmented reply from peer
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

                        // Cache in local store
                        if let Some(dht) = &mut state.dht_handler {
                            if let Ok(record) = DhtRecord::decode(&data) {
                                dht.store_mut().put(record);
                            }
                        }

                        // Reply to the original client (will be re-fragmented for client)
                        let client_reply = crate::wire::DhtQueryReply {
                            request_id: pending.client_request_id,
                            status: 0,
                            fragment_index: 0,
                            fragment_total: 1,
                            data,
                        };
                        crate::node::queue_fragmented_query_reply(
                            &mut state,
                            pending.client_session_id,
                            Frame::DhtQueryReply(client_reply),
                        );
                        state.pending_gw_queries.remove(&msg.request_id);
                        drop(state);
                        flush_connection(shared, pending.client_session_id)
                            .await
                            .ok();
                    }
                } else {
                    // Not found on this peer
                    let remaining = {
                        let p = state.pending_gw_queries.get_mut(&msg.request_id).unwrap();
                        p.remaining_peers -= 1;
                        p.remaining_peers
                    };
                    if remaining == 0 {
                        // All peers replied not-found — send not-found to client
                        let not_found = Frame::DhtQueryReply(crate::wire::DhtQueryReply {
                            request_id: pending.client_request_id,
                            status: 1,
                            fragment_index: 0,
                            fragment_total: 1,
                            data: Vec::new(),
                        });
                        if let Some(entry) = state.connections.get_mut(&pending.client_session_id) {
                            entry.conn.queue_frame(not_found);
                        }
                        state.pending_gw_queries.remove(&msg.request_id);
                        drop(state);
                        flush_connection(shared, pending.client_session_id)
                            .await
                            .ok();
                    }
                }
            } else {
                // Regular DHT query reply (for internal lookups)
                let from = state
                    .connections
                    .get(&session_id)
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
                crate::node::process_dht_actions(shared, actions).await;
            }
        }
        Frame::DhtStore(msg) => {
            let mut state = shared.state.lock().await;

            // Reassemble fragmented DhtStore
            let assembled =
                if msg.fragment_total <= 1 {
                    Some(msg.data.clone())
                } else {
                    let key = session_id | 0x4000_0000_0000_0000;
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

            // Reassemble fragmented DhtPublish
            let assembled = if msg.fragment_total <= 1 {
                Some(msg.data.clone())
            } else {
                let collector = state
                    .dht_publish_fragments
                    .entry(session_id)
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
                    state.dht_publish_fragments.remove(&session_id);
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
                crate::node::process_dht_actions(shared, actions).await;
            }
        }
        Frame::HolePunchRequest(req) => {
            let state = shared.state.lock().await;
            let requester_addr = state
                .connections
                .get(&session_id)
                .and_then(|e| match &e.path {
                    TransportPath::Direct { addr } => Some(*addr),
                    _ => None,
                });
            let requester_peer_id = state
                .connections
                .get(&session_id)
                .and_then(|e| e.conn.peer_public_key().map(|pk| pk.peer_id()));
            let target_client = state.gateway_clients.get(&req.target_peer_id).cloned();
            drop(state);

            let requester_id = match requester_peer_id {
                Some(id) => id,
                None => return,
            };
            let req_addr = match requester_addr {
                Some(a) => a,
                None => return,
            };
            let target = match target_client {
                Some(c) => c,
                None => return,
            };

            let notify = Frame::HolePunchNotify(crate::wire::HolePunchNotify {
                requester_peer_id: requester_id,
                addrs: vec![req_addr],
            });

            let mut state = shared.state.lock().await;
            if let Some(entry) = state.connections.get_mut(&target.session_id) {
                entry.conn.queue_frame(notify);
            }
            drop(state);
            flush_connection(shared, target.session_id).await.ok();
        }
        _ => {}
    }
}

pub(crate) async fn process_gateway_packet_client(shared: &Shared, pkt: GatewayPacket) {
    let packet = match Packet::decode(&pkt.inner) {
        Ok(p) => p,
        Err(e) => {
            warn!(src = ?pkt.src_peer_id, inner_len = pkt.inner.len(), ?e, "gateway_packet: failed to decode inner packet");
            return;
        }
    };

    match packet {
        Packet::KeyExchangeInit(init) => {
            info!(me = %short_pid(shared), src = ?pkt.src_peer_id, initiator_sid = init.initiator_session_id, "gateway_packet: KeyExchangeInit");
            handle_key_exchange_init_relayed(shared, Box::new(init), pkt.src_peer_id).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            info!(me = %short_pid(shared), src = ?pkt.src_peer_id, initiator_sid = resp.initiator_session_id, "gateway_packet: KeyExchangeResponse");
            crate::node::handle_key_exchange_response(
                shared,
                Box::new(resp),
                SocketAddr::from(([0, 0, 0, 0], 0)),
            )
            .await;
        }
        Packet::Data(data) => {
            process_relayed_data(shared, data).await;
        }
        _ => {}
    }
}

pub(crate) async fn handle_key_exchange_init_relayed(
    shared: &Shared,
    init: Box<KeyExchangeInit>,
    src_peer_id: PeerId,
) {
    // Dedup: if we already have a responder session for this (src, initiator_sid),
    // this is a retransmission — skip to avoid creating duplicate sessions with
    // different keys, which would break auth exchange.
    {
        let state = shared.state.lock().await;
        if state.connections.values().any(|e| {
            e.conn.remote_session_id() == init.initiator_session_id
                && !e.is_local_initiator
                && matches!(&e.path, TransportPath::Relayed { dest_peer_id, .. } if *dest_peer_id == src_peer_id)
        }) {
            return;
        }
    }

    let resp_eph = Box::new(EphemeralPrivateKey::generate());
    let (ct, resp_ss) = match resp_eph.encapsulate(&init.ephemeral_public_key) {
        Some(pair) => pair,
        None => return,
    };
    let ct = Box::new(ct);

    let keys = EncryptionKeys::new(&resp_ss, &init.ephemeral_public_key, &ct);
    let th = compute_transcript_hash(&init.ephemeral_public_key, &ct);

    let mut state = shared.state.lock().await;
    let gw = match &state.gateway {
        Some(g) => g.session_id,
        None => return,
    };

    let local_sid = state.next_session_id;
    state.next_session_id += 1;

    let response = KeyExchangeResponse {
        responder_session_id: local_sid,
        initiator_session_id: init.initiator_session_id,
        kem_ciphertext: *ct,
    };
    let resp_bytes = response.encode();
    info!(me = %short_pid(shared), local_sid, initiator_sid = init.initiator_session_id, src = ?src_peer_id, gw, "handle_init_relayed: sending response via GW");

    if let Some(gw_entry) = state.connections.get_mut(&gw) {
        gw_entry
            .conn
            .queue_frame(Frame::GatewayPacket(GatewayPacket {
                dest_peer_id: src_peer_id.clone(),
                src_peer_id: shared.identity.public_key().peer_id(),
                inner: resp_bytes,
            }));
    }

    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = PeerConnection::new(
        session,
        local_sid,
        init.initiator_session_id,
        false,
        auth_payload,
    );

    let path = TransportPath::Relayed {
        gateway_session_id: gw,
        dest_peer_id: src_peer_id,
    };

    let entry = ConnEntry {
        path,
        conn: Box::new(conn),
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: false,
        intent: crate::wire::packet::INTENT_PEER_SESSION,
    };
    state.connections.insert(local_sid, entry);

    flush_connection_locked(&mut state, shared, gw).await;

    let packets = state
        .connections
        .get_mut(&local_sid)
        .unwrap()
        .conn
        .poll_packets(Instant::now());

    let relay_path = state.connections.get(&local_sid).unwrap().path.clone();
    drop(state);

    send_packets(shared, &relay_path, &packets).await;
}

pub(crate) async fn process_relayed_data(shared: &Shared, data: Data) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let session_id = match find_session_by_receiver(&state, data.receiver_session_id) {
        Some(id) => id,
        None => {
            warn!(
                receiver_sid = data.receiver_session_id,
                "relayed_data: no session found"
            );
            return;
        }
    };

    let entry = match state.connections.get_mut(&session_id) {
        Some(e) => e,
        None => return,
    };

    let was_established = entry.conn.is_established();
    let had_close = entry.conn.got_connection_close();
    let unhandled = entry.conn.on_data_packet(data, now);
    entry.last_recv = now;
    let is_established = entry.conn.is_established();
    let has_new_stream = entry.conn.has_pending_accept();
    let got_close = !had_close && entry.conn.got_connection_close();
    let is_local_initiator = entry.is_local_initiator;
    if got_close {
        entry.closed = true;
    }

    // Flush response packets immediately (auth frames, ACKs, etc.)
    let packets = entry.conn.poll_packets(now);
    let path = entry.path.clone();

    if got_close {
        state.connections.remove(&session_id);
    }
    if !was_established && is_established && !is_local_initiator {
        if !state.accept_queue.contains(&session_id) {
            debug!(session_id, "accept_queue: push (relayed_data)");
            state.accept_queue.push_back(session_id);
        }
    }
    drop(state);

    // Send response packets via relay
    send_packets(shared, &path, &packets).await;

    // Process unhandled frames (use Box::pin to break async recursion cycle)
    for frame in unhandled {
        Box::pin(crate::node::process_unhandled_frame(shared, session_id, frame)).await;
    }

    if !was_established && is_established {
        info!(me = %short_pid(shared), session_id, is_local_initiator, "ESTABLISHED (relayed)");
        if !is_local_initiator {
            shared.accept_notify.notify_waiters();
        }
        shared.established_notify.notify_waiters();
    }
    shared.data_notify.notify_waiters();
    if has_new_stream {
        shared.stream_notify.notify_waiters();
    }
}
