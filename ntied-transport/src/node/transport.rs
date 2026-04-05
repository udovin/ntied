use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::connection::Connection as InnerConnection;
use crate::crypto::{EncryptionKeys, KemPrivateKey, PeerId, PrivateKey, compute_transcript_hash};
use crate::relay::protocol::RelayMessage;
use crate::session::{Role, Session};
use crate::wire::packet::{Data, Packet};
use crate::wire::{KeyExchangeInit, KeyExchangeResponse};

use super::handle::{Connection, DatagramChannel, channel_err_to_io};
use super::state::*;
use super::{
    CONNECTION_TIMEOUT, DIRECT_PROBE_TIMEOUT, DIRECT_TIMEOUT, FLUSH_INTERVAL, PING_INTERVAL,
    RECV_BUF_SIZE,
};

pub(crate) async fn recv_loop(weak: std::sync::Weak<Shared>) {
    let mut buf = vec![0u8; RECV_BUF_SIZE].into_boxed_slice();
    let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let shared = match weak.upgrade() {
            Some(s) => s,
            None => break,
        };
        tokio::select! {
            result = shared.socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, addr)) => {
                        let path = SendPath::Direct { addr };
                        process_packet(&shared, &buf[..len], path).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            }
            _ = flush_interval.tick() => {
                flush_all(&shared).await;
            }
        }
    }
}

/// Background task that reads from the relay datagram channel and processes
/// tunneled packets as if they arrived from the network.
pub(crate) async fn relay_listener_loop(weak: std::sync::Weak<Shared>, datagram: DatagramChannel, relay_addr: SocketAddr) {
    loop {
        let data = match datagram.recv().await {
            Ok(d) => d,
            Err(_) => break,
        };

        let shared = match weak.upgrade() {
            Some(s) => s,
            None => break,
        };

        let msg = match RelayMessage::decode(&data) {
            Some(m) => m,
            None => continue,
        };

        match msg {
            RelayMessage::Tunnel {
                peer_id,
                data: inner,
            } => {
                let path = SendPath::Relayed { peer_id };
                process_packet(&shared, &inner, path).await;
            }
            RelayMessage::HolePunchNotify { requester, addrs } => {
                debug!(%requester, ?addrs, "relay listener: received hole punch notify");
                if let Some(addr) = addrs.first().copied() {
                    let mut state = shared.state.lock().await;
                    for entry in state.connections.values_mut() {
                        let is_match = match &entry.send_path {
                            SendPath::Relayed { peer_id } => *peer_id == requester,
                            _ => entry.relay_peer_id.as_ref() == Some(&requester),
                        };
                        if is_match {
                            debug!(%requester, %addr, "setting direct_addr from hole punch notify");
                            entry.direct_addr = Some(addr);
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(shared) = weak.upgrade() {
        let mut relay = shared.relay.lock().await;
        let is_current = relay.as_ref().map_or(false, |r| r.relay_addr == relay_addr);
        if is_current {
            *relay = None;
            shared.accept_notify.notify_waiters();
        }
    }
    warn!("relay listener: disconnected");
}

async fn process_packet(shared: &Shared, buf: &[u8], send_path: SendPath) {
    let packet = match Packet::decode(buf) {
        Ok(p) => Box::new(p),
        Err(_) => return,
    };

    match *packet {
        Packet::KeyExchangeInit(init) => {
            handle_key_exchange_init(shared, Box::new(init), send_path).await;
        }
        Packet::KeyExchangeResponse(resp) => {
            handle_key_exchange_response(shared, Box::new(resp)).await;
        }
        Packet::Data(data) => {
            handle_data(shared, data, send_path).await;
        }
    }
}

async fn handle_key_exchange_init(
    shared: &Shared,
    init: Box<KeyExchangeInit>,
    send_path: SendPath,
) {
    let resp_eph = Box::new(KemPrivateKey::generate());
    let (ct, resp_ss) = match resp_eph.encapsulate(&init.kem_public_key) {
        Some(pair) => pair,
        None => return,
    };
    let ct = Box::new(ct);

    let keys = EncryptionKeys::new(&resp_ss, &init.kem_public_key, &ct);
    let th = compute_transcript_hash(&init.kem_public_key, &ct);

    let mut state = shared.state.lock().await;
    let local_sid = state.next_connection_id;
    state.next_connection_id += 1;

    let response = Box::new(KeyExchangeResponse {
        responder_connection_id: local_sid,
        initiator_connection_id: init.initiator_connection_id,
        kem_ciphertext: *ct,
    });

    let response_bytes = response.encode();

    match &send_path {
        SendPath::Direct { addr } => {
            let _ = shared.socket.send_to(&response_bytes, *addr).await;
        }
        SendPath::Relayed { peer_id } => {
            drop(state);
            let _ = send_via_relay(shared, peer_id, &response_bytes).await;
            let mut state2 = shared.state.lock().await;
            let session = Session::new(Role::Responder, 1, keys, th);
            let auth_payload = build_auth_payload(&shared.identity, &th);

            let conn = Box::new(InnerConnection::new(
                session,
                local_sid,
                init.initiator_connection_id,
                false,
                auth_payload,
            ));

            let entry = ConnEntry {
                send_path: send_path.clone(),
                relay_peer_id: Some(*peer_id),
                direct_addr: None,
                last_direct_recv: None,
                conn,
                last_recv: Instant::now(),
                last_ping_sent: Instant::now(),
                closed: false,
                is_local_initiator: false,
            };
            state2.connections.insert(local_sid, entry);

            let packets = state2
                .connections
                .get_mut(&local_sid)
                .unwrap()
                .poll_packets(Instant::now());
            drop(state2);

            send_packets(shared, &send_path, &packets).await;
            return;
        }
    }

    let session = Session::new(Role::Responder, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = Box::new(InnerConnection::new(
        session,
        local_sid,
        init.initiator_connection_id,
        false,
        auth_payload,
    ));

    let direct_addr = match &send_path {
        SendPath::Direct { addr } => Some(*addr),
        _ => None,
    };
    let entry = ConnEntry {
        send_path: send_path.clone(),
        relay_peer_id: None,
        direct_addr,
        last_direct_recv: direct_addr.map(|_| Instant::now()),
        conn,
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: false,
    };
    state.connections.insert(local_sid, entry);

    let packets = state
        .connections
        .get_mut(&local_sid)
        .unwrap()
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &send_path, &packets).await;
}

pub(crate) async fn handle_key_exchange_response(
    shared: &Shared,
    resp: Box<KeyExchangeResponse>,
) {
    let mut state = shared.state.lock().await;

    let pending = match state.pending_connects.remove(&resp.initiator_connection_id) {
        Some(p) => p,
        None => return,
    };

    let send_path = pending.send_path;
    let relay_peer_id = pending.relay_peer_id;

    let init_pk = Box::new(pending.ephemeral_key.public_key());
    let init_ss = match pending.ephemeral_key.decapsulate(&resp.kem_ciphertext) {
        Some(ss) => ss,
        None => return,
    };

    let keys = EncryptionKeys::new(&init_ss, &init_pk, &resp.kem_ciphertext);
    let th = compute_transcript_hash(&init_pk, &resp.kem_ciphertext);
    let session = Session::new(Role::Initiator, 1, keys, th);
    let auth_payload = build_auth_payload(&shared.identity, &th);

    let conn = Box::new(InnerConnection::new(
        session,
        resp.initiator_connection_id,
        resp.responder_connection_id,
        true,
        auth_payload,
    ));

    let direct_addr = match &send_path {
        SendPath::Direct { addr } => Some(*addr),
        _ => None,
    };
    let entry = ConnEntry {
        send_path: send_path.clone(),
        relay_peer_id,
        direct_addr,
        last_direct_recv: direct_addr.map(|_| Instant::now()),
        conn,
        last_recv: Instant::now(),
        last_ping_sent: Instant::now(),
        closed: false,
        is_local_initiator: true,
    };
    state
        .connections
        .insert(resp.initiator_connection_id, entry);

    let packets = state
        .connections
        .get_mut(&resp.initiator_connection_id)
        .unwrap()
        .poll_packets(Instant::now());
    drop(state);

    send_packets(shared, &send_path, &packets).await;
}

async fn handle_data(shared: &Shared, data: Data, recv_path: SendPath) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let receiver_sid = data.receiver_connection_id;
    let connection_id = match find_session_by_receiver(&state, receiver_sid) {
        Some(id) => id,
        None => {
            debug!(me = %short_pid(shared), receiver_sid, "handle_data: unknown session");
            return;
        }
    };

    let (was_established, is_established, has_new_stream, got_close, packets, entry_send_path, direct_addr, is_local_initiator) = {
        let entry = match state.connections.get_mut(&connection_id) {
            Some(e) => e,
            None => return,
        };

        if let SendPath::Direct { addr } = &recv_path {
            let addr = *addr;
            entry.direct_addr = Some(addr);
            entry.last_direct_recv = Some(now);

            if matches!(entry.send_path, SendPath::Relayed { .. }) {
                if let SendPath::Relayed { peer_id } = &entry.send_path {
                    entry.relay_peer_id = Some(*peer_id);
                }
                debug!(connection_id, %addr, "direct path detected, switching from relay to direct");
                entry.send_path = SendPath::Direct { addr };
            } else if let SendPath::Direct { addr: ref mut current } = entry.send_path {
                *current = addr;
            }
        }

        let was_established = entry.is_established();
        let had_close = entry.got_connection_close();
        entry.conn.on_data_packet(data, now);
        entry.last_recv = now;
        let is_established = entry.is_established();
        let has_new_stream = entry.has_pending_accept();
        let got_close = !had_close && entry.got_connection_close();
        let is_local_initiator = entry.is_local_initiator;
        if got_close {
            entry.closed = true;
        }
        let packets = entry.poll_packets(now);
        let entry_send_path = entry.send_path.clone();
        let direct_addr = entry.direct_addr;

        (
            was_established,
            is_established,
            has_new_stream,
            got_close,
            packets,
            entry_send_path,
            direct_addr,
            is_local_initiator,
        )
    };

    if got_close {
        state.connections.remove(&connection_id);
    }

    if !was_established && is_established && !is_local_initiator {
        debug!(connection_id, "accept_queue: push (handle_data)");
        state.accept_queue.push_back(connection_id);
    }
    drop(state);

    send_packets_with_probe(shared, &entry_send_path, direct_addr, &packets).await;

    if !was_established && is_established {
        info!(me = %short_pid(shared), connection_id, is_local_initiator, "ESTABLISHED");
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

pub(crate) async fn flush_all(shared: &Shared) {
    let now = Instant::now();
    let mut state = shared.state.lock().await;

    let closes: Vec<u64> = shared.pending_close.lock().unwrap().drain(..).collect();
    for connection_id in closes {
        if let Some(entry) = state.connections.get_mut(&connection_id) {
            if !entry.closed {
                entry.closed = true;
                entry.queue_connection_close(0);
            }
        }
    }

    let mut timed_out: Vec<u64> = Vec::new();
    for (&sid, entry) in state.connections.iter_mut() {
        if entry.closed {
            continue;
        }
        if entry.is_established() && now.duration_since(entry.last_recv) > CONNECTION_TIMEOUT {
            warn!(sid, elapsed_secs = now.duration_since(entry.last_recv).as_secs(), "connection timed out");
            timed_out.push(sid);
            continue;
        }
        if let SendPath::Direct { .. } = &entry.send_path {
            if let Some(relay_pid) = &entry.relay_peer_id {
                if let Some(last) = entry.last_direct_recv {
                    if now.duration_since(last) > DIRECT_TIMEOUT {
                        debug!(sid, "direct path timed out, falling back to relay");
                        entry.send_path = SendPath::Relayed {
                            peer_id: *relay_pid,
                        };
                    }
                }
            }
        }
        if entry.is_established() && now.duration_since(entry.last_ping_sent) > PING_INTERVAL {
            let ping_id = shared.ping_counter.fetch_add(1, Ordering::Relaxed);
            entry.queue_ping(ping_id);
            entry.last_ping_sent = now;
        }
    }
    for sid in &timed_out {
        state.connections.remove(sid);
    }

    let mut to_send: Vec<(SendPath, Option<SocketAddr>, Option<PeerId>, Vec<Data>)> = Vec::new();
    let mut to_remove: Vec<u64> = Vec::new();

    for (&sid, entry) in state.connections.iter_mut() {
        let packets = entry.poll_packets(now);
        if !packets.is_empty() {
            let relay_probe = if let SendPath::Direct { .. } = &entry.send_path {
                if let (Some(relay_pid), Some(last)) =
                    (&entry.relay_peer_id, entry.last_direct_recv)
                {
                    if now.duration_since(last) > DIRECT_PROBE_TIMEOUT {
                        Some(*relay_pid)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            to_send.push((entry.send_path.clone(), entry.direct_addr, relay_probe, packets));
        }
        if entry.closed && !entry.has_pending() {
            to_remove.push(sid);
        }
    }

    for sid in &to_remove {
        state.connections.remove(sid);
    }
    drop(state);

    if !timed_out.is_empty() || !to_remove.is_empty() {
        shared.data_notify.notify_waiters();
        shared.stream_notify.notify_waiters();
    }

    for (path, direct_addr, relay_probe, packets) in &to_send {
        send_packets_with_probe(shared, path, *direct_addr, packets).await;
        if let Some(relay_pid) = relay_probe {
            for data in packets {
                let encoded = data.encode();
                if let Err(e) = send_via_relay(shared, relay_pid, &encoded).await {
                    debug!(%relay_pid, "probe relay fallback: {e}");
                }
            }
        }
    }
}

pub(crate) fn short_pid(shared: &Shared) -> String {
    let full = format!("{:?}", shared.identity.public_key().peer_id());
    full.chars()
        .skip(full.len().saturating_sub(6))
        .take(4)
        .collect()
}

pub(crate) async fn flush_connection(shared: &Shared, connection_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (send_path, direct_addr, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let packets = entry.poll_packets(now);
        (entry.send_path.clone(), entry.direct_addr, packets)
    };

    send_packets_with_probe(shared, &send_path, direct_addr, &packets).await;
    Ok(())
}

pub(crate) fn find_session_by_receiver(
    state: &TransportState,
    receiver_connection_id: u64,
) -> Option<u64> {
    state
        .connections
        .iter()
        .find(|(_, e)| e.local_connection_id() == receiver_connection_id)
        .map(|(&id, _)| id)
}

pub(crate) fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();
    let sig = identity.sign(transcript_hash);
    let mut payload = Vec::new();
    payload.extend_from_slice(&pk.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());
    payload
}

/// Send encoded data packets via the appropriate path (direct UDP or relay tunnel).
pub(crate) async fn send_packets(shared: &Shared, send_path: &SendPath, packets: &[Data]) {
    send_packets_with_probe(shared, send_path, None, packets).await;
}

/// Send data packets via the primary path. When the primary path is relayed
/// and a `direct_addr` is known, also send probing packets directly so the
/// remote peer can detect the direct path.
pub(crate) async fn send_packets_with_probe(
    shared: &Shared,
    send_path: &SendPath,
    direct_addr: Option<SocketAddr>,
    packets: &[Data],
) {
    match send_path {
        SendPath::Direct { addr } => {
            for data in packets {
                let _ = shared.socket.send_to(&data.encode(), *addr).await;
            }
        }
        SendPath::Relayed { peer_id } => {
            for data in packets {
                let encoded = data.encode();
                if let Err(e) = send_via_relay(shared, peer_id, &encoded).await {
                    debug!(%peer_id, "send_packets relay: {e}");
                }
                if let Some(addr) = direct_addr {
                    let _ = shared.socket.send_to(&encoded, addr).await;
                }
            }
        }
    }
}

/// Send raw bytes to a peer through the attached relay's tunnel.
///
/// This function writes to the relay datagram channel and flushes the relay
/// connection using direct UDP sends only (never recursing through the
/// generic `send_packets` path).
pub(crate) async fn send_via_relay(shared: &Shared, peer_id: &PeerId, data: &[u8]) -> io::Result<()> {
    let relay = shared.relay.lock().await;
    let relay_state = relay.as_ref().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "no relay attached")
    })?;

    let msg = RelayMessage::Tunnel {
        peer_id: *peer_id,
        data: data.to_vec(),
    };
    let encoded_msg = msg.encode();
    let relay_conn_id = relay_state.datagram.connection_id;
    let relay_ch_id = relay_state.datagram.channel_id;
    drop(relay);

    {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&relay_conn_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "relay connection gone"))?;
        entry
            .conn
            .write_datagram(relay_ch_id, &encoded_msg)
            .map_err(channel_err_to_io)?;
    }

    flush_connection_direct(shared, relay_conn_id).await
}

/// Flush a connection using direct UDP only. Used for the relay connection
/// itself to avoid recursion in send_packets.
pub(crate) async fn flush_connection_direct(shared: &Shared, connection_id: u64) -> io::Result<()> {
    let now = Instant::now();
    let (addr, packets) = {
        let mut state = shared.state.lock().await;
        let entry = state
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "connection gone"))?;
        let addr = match &entry.send_path {
            SendPath::Direct { addr } => *addr,
            SendPath::Relayed { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "flush_connection_direct called on relayed connection",
                ));
            }
        };
        let packets = entry.poll_packets(now);
        (addr, packets)
    };

    for data in &packets {
        let _ = shared.socket.send_to(&data.encode(), addr).await;
    }
    Ok(())
}

/// Wait for a connection to become established, with timeout.
pub(crate) async fn wait_for_established(shared: &Arc<Shared>, connection_id: u64) -> io::Result<Connection> {
    use std::sync::atomic::AtomicBool;
    use super::HANDSHAKE_TIMEOUT;

    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        tokio::select! {
            _ = shared.established_notify.notified() => {
                let state = shared.state.lock().await;
                if let Some(entry) = state.connections.get(&connection_id) {
                    if entry.is_established() {
                        return Ok(Connection {
                            shared: shared.clone(),
                            connection_id,
                            closed: AtomicBool::new(false),
                        });
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let mut state = shared.state.lock().await;
                state.pending_connects.remove(&connection_id);
                state.connections.remove(&connection_id);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "handshake timed out",
                ));
            }
        }
    }
}
