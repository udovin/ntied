use std::time::Instant;

use tracing::debug;

use crate::node::{
    flush_connection, ConnEntry, GatewayPeer, RegisteredClient, Shared, TransportPath,
    GATEWAY_PEER_FLAG,
};
use crate::wire::{Frame, GatewayPacket};

pub(crate) async fn process_relay_frame(shared: &Shared, connection_id: u64, frame: &Frame) {
    match frame {
        Frame::GatewayRegister(reg) => {
            let mut state = shared.state.lock().await;
            let external_addr = match state.connections.get(&connection_id) {
                Some(entry) => match &entry.path {
                    TransportPath::Direct { addr } => *addr,
                    _ => return,
                },
                None => return,
            };

            if reg.flags & GATEWAY_PEER_FLAG != 0 {
                debug!(peer_id = ?reg.peer_id, connection_id, "gw: peer gateway registered");
                state.gateway_peers.insert(
                    reg.peer_id,
                    GatewayPeer {
                        connection_id,
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
                debug!(peer_id = ?reg.peer_id, connection_id, "gw: client registered");
                state.gateway_clients.insert(
                    reg.peer_id,
                    RegisteredClient {
                        connection_id,
                        external_addr,
                    },
                );
            }

            if let Some(entry) = state.connections.get_mut(&connection_id) {
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
            flush_connection(shared, connection_id).await.ok();
        }
        Frame::GatewayPacket(pkt) => {
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
                if let Some(entry) = state.connections.get_mut(&dest.connection_id) {
                    entry.conn.queue_frame(deliver);
                }
                drop(state);
                flush_connection(shared, dest.connection_id).await.ok();
            } else {
                tracing::warn!(dest = ?pkt.dest_peer_id, "gw: dest not found locally");
            }
        }
        Frame::HolePunchRequest(req) => {
            let state = shared.state.lock().await;
            let requester_addr = state
                .connections
                .get(&connection_id)
                .and_then(|e| match &e.path {
                    TransportPath::Direct { addr } => Some(*addr),
                    _ => None,
                });
            let requester_peer_id = state
                .connections
                .get(&connection_id)
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
            if let Some(entry) = state.connections.get_mut(&target.connection_id) {
                entry.conn.queue_frame(notify);
            }
            drop(state);
            flush_connection(shared, target.connection_id).await.ok();
        }
        _ => {}
    }
}
