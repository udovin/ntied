use std::net::SocketAddr;
use std::time::Instant;

use tracing::{debug, info, warn};

use crate::crypto::{compute_transcript_hash, EncryptionKeys, EphemeralPrivateKey, PeerId};
use crate::connection::PeerConnection;
use crate::node::{
    build_auth_payload, find_session_by_receiver, flush_connection, flush_connection_locked,
    send_packets, short_pid, ConnEntry, Shared, TransportPath,
};
use crate::session::{Role, Session};
use crate::wire::packet::{Data, Packet};
use crate::wire::{Frame, GatewayPacket, KeyExchangeInit, KeyExchangeResponse};

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
            info!(me = %short_pid(shared), src = ?pkt.src_peer_id, initiator_sid = init.initiator_connection_id, "gateway_packet: KeyExchangeInit");
            handle_key_exchange_init_relayed(shared, Box::new(init), pkt.src_peer_id).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            info!(me = %short_pid(shared), src = ?pkt.src_peer_id, initiator_sid = resp.initiator_connection_id, "gateway_packet: KeyExchangeResponse");
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
    {
        let state = shared.state.lock().await;
        if state.connections.values().any(|e| {
            e.conn.remote_connection_id() == init.initiator_connection_id
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
        Some(g) => g.connection_id,
        None => return,
    };

    let local_sid = state.next_connection_id;
    state.next_connection_id += 1;

    let response = KeyExchangeResponse {
        responder_connection_id: local_sid,
        initiator_connection_id: init.initiator_connection_id,
        kem_ciphertext: *ct,
    };
    let resp_bytes = response.encode();
    info!(me = %short_pid(shared), local_sid, initiator_sid = init.initiator_connection_id, src = ?src_peer_id, gw, "handle_init_relayed: sending response via GW");

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
        init.initiator_connection_id,
        false,
        auth_payload,
    );

    let path = TransportPath::Relayed {
        gateway_connection_id: gw,
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

    let connection_id = match find_session_by_receiver(&state, data.receiver_connection_id) {
        Some(id) => id,
        None => {
            warn!(
                receiver_sid = data.receiver_connection_id,
                "relayed_data: no session found"
            );
            return;
        }
    };

    let entry = match state.connections.get_mut(&connection_id) {
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

    let packets = entry.conn.poll_packets(now);
    let path = entry.path.clone();

    if got_close {
        state.connections.remove(&connection_id);
    }
    if !was_established && is_established && !is_local_initiator {
        if !state.accept_queue.contains(&connection_id) {
            debug!(connection_id, "accept_queue: push (relayed_data)");
            state.accept_queue.push_back(connection_id);
        }
    }
    drop(state);

    send_packets(shared, &path, &packets).await;

    for frame in unhandled {
        Box::pin(crate::node::process_unhandled_frame(shared, connection_id, frame)).await;
    }

    if !was_established && is_established {
        info!(me = %short_pid(shared), connection_id, is_local_initiator, "ESTABLISHED (relayed)");
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
