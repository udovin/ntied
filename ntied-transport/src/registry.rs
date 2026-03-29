// RegistryService: DHT (k-buckets, store, lookup)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use crate::crypto::PeerId;
use crate::dht::DhtAction;
use crate::node::{flush_connection, RouteInfo, Shared, TransportState};
use crate::wire::{DhtQueryReply, DhtStore, Frame};

pub const DHT_QUERY_TIMEOUT: Duration = Duration::from_secs(15);
pub const DHT_MAX_FRAGMENT: usize = 1000;

pub struct DhtDiscovery {
    pub(crate) shared: Arc<Shared>,
}

impl DhtDiscovery {
    pub async fn resolve(&self, peer_id: &PeerId) -> Option<RouteInfo> {
        let (tx, rx) = oneshot::channel();
        let request_id = {
            let mut state = self.shared.state.lock().await;
            let gw = state.gateway.as_ref()?;
            let gw_session_id = gw.session_id;
            let request_id = state.next_dht_request_id;
            state.next_dht_request_id = state.next_dht_request_id.wrapping_add(1);
            state.pending_dht_queries.insert(request_id, tx);
            if let Some(entry) = state.connections.get_mut(&gw_session_id) {
                entry
                    .conn
                    .queue_frame(Frame::DhtQuery(crate::wire::DhtQuery {
                        target: *peer_id,
                        request_id,
                    }));
            }
            drop(state);
            flush_connection(&self.shared, gw_session_id).await.ok();
            request_id
        };

        let result = tokio::time::timeout(DHT_QUERY_TIMEOUT, rx).await;
        match result {
            Ok(Ok(Some(record))) => {
                if let Some(gw_info) = record.gateways.first() {
                    if let Some(addr) = gw_info.addrs.first() {
                        return Some(RouteInfo::Relayed {
                            gateway_peer_id: gw_info.gateway_peer_id,
                            gateway_addr: *addr,
                        });
                    }
                }
                None
            }
            _ => {
                let mut state = self.shared.state.lock().await;
                state.pending_dht_queries.remove(&request_id);
                None
            }
        }
    }
}

pub(crate) async fn process_dht_actions(shared: &Shared, actions: Vec<DhtAction>) {
    for action in actions {
        match action {
            DhtAction::SendTo { peer_id, frame } => {
                let state = shared.state.lock().await;
                let target_session = state
                    .gateway_clients
                    .get(&peer_id)
                    .map(|c| c.session_id)
                    .or_else(|| state.gateway_peers.get(&peer_id).map(|p| p.session_id));
                drop(state);

                if let Some(sid) = target_session {
                    let mut state = shared.state.lock().await;
                    if let Some(entry) = state.connections.get_mut(&sid) {
                        // Fragment large DhtStore frames
                        if let Frame::DhtStore(store) = &frame {
                            if store.data.len() > DHT_MAX_FRAGMENT {
                                let chunks: Vec<Vec<u8>> = store
                                    .data
                                    .chunks(DHT_MAX_FRAGMENT)
                                    .map(|c| c.to_vec())
                                    .collect();
                                let total = chunks.len() as u8;
                                for (i, data) in chunks.into_iter().enumerate() {
                                    entry.conn.queue_frame(Frame::DhtStore(
                                        DhtStore {
                                            fragment_index: i as u8,
                                            fragment_total: total,
                                            data,
                                        },
                                    ));
                                }
                            } else {
                                entry.conn.queue_frame(frame);
                            }
                        } else {
                            entry.conn.queue_frame(frame);
                        }
                    }
                    drop(state);
                    flush_connection(shared, sid).await.ok();
                }
            }
            DhtAction::QueryComplete { .. } => {}
        }
    }
}

pub(crate) fn queue_fragmented_query_reply(state: &mut TransportState, session_id: u64, reply: Frame) {
    if let Frame::DhtQueryReply(qr) = reply {
        let entry = match state.connections.get_mut(&session_id) {
            Some(e) => e,
            None => return,
        };
        if qr.data.len() <= DHT_MAX_FRAGMENT {
            entry.conn.queue_frame(Frame::DhtQueryReply(qr));
        } else {
            let chunks: Vec<Vec<u8>> = qr
                .data
                .chunks(DHT_MAX_FRAGMENT)
                .map(|c| c.to_vec())
                .collect();
            let total = chunks.len() as u8;
            for (i, data) in chunks.into_iter().enumerate() {
                entry
                    .conn
                    .queue_frame(Frame::DhtQueryReply(DhtQueryReply {
                        request_id: qr.request_id,
                        status: qr.status,
                        fragment_index: i as u8,
                        fragment_total: total,
                        data,
                    }));
            }
        }
    }
}
