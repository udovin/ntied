use std::net::SocketAddr;
use std::sync::Once;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey, RelayNode};
use ntied_transport::relay::protocol::{RelayMessage, PURPOSE_RELAY};

static TRACING_INIT: Once = Once::new();

fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_target(false)
            .with_test_writer()
            .init();
    });
}

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

#[tokio::test]
async fn two_nodes_handshake() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);
    assert!(conn_a.peer_id().await.is_some());
    assert!(conn_b.peer_id().await.is_some());
}

#[tokio::test]
async fn connect_and_stream() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello world").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello world");
}

#[tokio::test]
async fn bidirectional_streams() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // A → B
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"from A").await.unwrap();

    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"from A");

    // B → A
    let sb2 = conn_b.open_stream(2).await.unwrap();
    sb2.send(b"from B").await.unwrap();

    let (sa2, _) = conn_a.accept_stream().await.unwrap();
    let data2 = sa2.recv().await.unwrap();
    assert_eq!(data2, b"from B");
}

#[tokio::test]
async fn multi_message_exchange() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let sa = conn_a.open_stream(1).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();

    for i in 0..10 {
        let msg = format!("message {i}");
        sa.send(msg.as_bytes()).await.unwrap();
        let data = sb.recv().await.unwrap();
        assert_eq!(data, msg.as_bytes());
    }
}

#[tokio::test]
async fn accept_does_not_return_initiator_connection() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let conn_a = node_a.connect(addr_b).await.unwrap();
    assert!(conn_a.is_established().await);

    // node_b.accept() should return the connection
    let accept = tokio::time::timeout(Duration::from_secs(2), node_b.accept()).await;
    assert!(accept.is_ok(), "accept should return the responder connection");

    // node_a.accept() should NOT return anything (initiator doesn't get accept)
    let accept_a = tokio::time::timeout(Duration::from_millis(500), node_a.accept()).await;
    assert!(accept_a.is_err(), "initiator should not get accept");
}

#[tokio::test]
async fn connection_close() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let _conn_b = accept.await.unwrap();

    conn_a.close().await.unwrap();
    // Connection should be closed gracefully
}

#[tokio::test]
async fn datagram_channel() {
    init_tracing();
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();

    let addr_b = node_b.local_addr().unwrap();

    let accept = tokio::spawn(async move { node_b.accept().await.unwrap() });
    let conn_a = node_a.connect(addr_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let da = conn_a.open_datagram(99).await.unwrap();
    da.send(b"datagram hello").await.unwrap();

    let (db, purpose) = conn_b.accept_datagram().await.unwrap();
    assert_eq!(purpose, 99);
    let data = db.recv().await.unwrap();
    assert_eq!(data, b"datagram hello");
}

// ── Relay tests ──

#[tokio::test]
async fn relay_two_clients_tunnel() {
    init_tracing();

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    // Client A connects to relay
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    let conn_a = node_a.connect(relay_addr).await.unwrap();
    let relay_ch_a = conn_a.open_datagram(PURPOSE_RELAY).await.unwrap();

    // Client B connects to relay
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    let conn_b = node_b.connect(relay_addr).await.unwrap();
    let relay_ch_b = conn_b.open_datagram(PURPOSE_RELAY).await.unwrap();

    // Both receive welcome
    let welcome_a = relay_ch_a.recv().await.unwrap();
    assert!(matches!(RelayMessage::decode(&welcome_a), Some(RelayMessage::Welcome { .. })));

    let welcome_b = relay_ch_b.recv().await.unwrap();
    assert!(matches!(RelayMessage::decode(&welcome_b), Some(RelayMessage::Welcome { .. })));

    // A sends tunnel message to B
    let msg = RelayMessage::Tunnel {
        peer_id: peer_id_b,
        data: b"hello from A".to_vec(),
    };
    relay_ch_a.send(&msg.encode()).await.unwrap();

    // B receives tunnel message from A
    let received = relay_ch_b.recv().await.unwrap();
    let decoded = RelayMessage::decode(&received).unwrap();
    match decoded {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_a, "should be from A");
            assert_eq!(data, b"hello from A");
        }
        _ => panic!("expected Tunnel message"),
    }

    // B sends back to A
    let reply = RelayMessage::Tunnel {
        peer_id: peer_id_a,
        data: b"hello from B".to_vec(),
    };
    relay_ch_b.send(&reply.encode()).await.unwrap();

    // A receives reply from B
    let received = relay_ch_a.recv().await.unwrap();
    let decoded = RelayMessage::decode(&received).unwrap();
    match decoded {
        RelayMessage::Tunnel { peer_id, data } => {
            assert_eq!(peer_id, peer_id_b, "should be from B");
            assert_eq!(data, b"hello from B");
        }
        _ => panic!("expected Tunnel message"),
    }

    relay_task.abort();
}

#[tokio::test]
async fn relay_connect_peer_and_stream() {
    init_tracing();

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_task = tokio::spawn(async move { relay.run().await });

    // Peer A attaches to relay
    let id_a = PrivateKey::generate();
    let peer_id_a = id_a.public_key().peer_id();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    // Peer B attaches to relay
    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Node::bind(localhost(), id_b).await.unwrap();
    node_b.attach_relay(relay_addr).await.unwrap();

    // Small delay for relay registration
    tokio::time::sleep(Duration::from_millis(100)).await;

    // B accepts, A connects to B through relay
    let accept = tokio::spawn(async move {
        node_b.accept().await.unwrap()
    });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);

    // Open stream A → B
    let sa = conn_a.open_stream(42).await.unwrap();
    sa.send(b"hello via relay").await.unwrap();

    let (sb, purpose) = conn_b.accept_stream().await.unwrap();
    assert_eq!(purpose, 42);
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"hello via relay");

    // Reply B → A
    let sb2 = conn_b.open_stream(99).await.unwrap();
    sb2.send(b"reply from B").await.unwrap();

    let (sa2, purpose2) = conn_a.accept_stream().await.unwrap();
    assert_eq!(purpose2, 99);
    let data2 = sa2.recv().await.unwrap();
    assert_eq!(data2, b"reply from B");

    relay_task.abort();
}

#[tokio::test]
async fn relay_connection_survives_relay_restart() {
    init_tracing();

    // Start relay 1
    let relay1 = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay1_addr = relay1.local_addr().unwrap();
    let relay1_task = tokio::spawn(async move { relay1.run().await });

    // Peer A and B — keep references accessible
    let id_a = PrivateKey::generate();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.attach_relay(relay1_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = std::sync::Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay1_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Establish connection A → B through relay
    let node_b2 = node_b.clone();
    let accept_b = tokio::spawn(async move { node_b2.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept_b.await.unwrap();

    // Verify it works before crash
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"before crash").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"before crash");

    // Kill relay 1
    relay1_task.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Start relay 2 on new port
    let relay2 = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay2_addr = relay2.local_addr().unwrap();
    let _relay2_task = tokio::spawn(async move { relay2.run().await });

    // Re-attach both peers to new relay
    node_a.attach_relay(relay2_addr).await.unwrap();
    node_b.attach_relay(relay2_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send data through existing stream — should work via new relay
    sa.send(b"after recovery").await.unwrap();

    let data2 = tokio::time::timeout(Duration::from_secs(5), sb.recv()).await;
    match data2 {
        Ok(Ok(d)) => assert_eq!(d, b"after recovery"),
        Ok(Err(e)) => panic!("recv error after relay restart: {e}"),
        Err(_) => panic!("timeout waiting for data after relay restart"),
    }
}

#[tokio::test]
async fn relay_to_direct_migration() {
    init_tracing();

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    // Peer A and B attach to relay
    let id_a = PrivateKey::generate();
    let node_a = Node::bind(localhost(), id_a).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = std::sync::Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect A → B through relay
    let node_b2 = node_b.clone();
    let accept = tokio::spawn(async move { node_b2.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    // Verify relayed
    assert!(conn_a.is_relayed().await, "should start as relayed");
    assert!(conn_b.is_relayed().await, "should start as relayed");

    // Send data through relay first
    let sa = conn_a.open_stream(1).await.unwrap();
    sa.send(b"via relay").await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let data = sb.recv().await.unwrap();
    assert_eq!(data, b"via relay");

    // Initiate direct migration
    conn_a.try_direct().await.unwrap();

    // Give time for hole punch notify + probing + direct path detection
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send more data — should go direct now
    sa.send(b"via direct").await.unwrap();
    let data2 = sb.recv().await.unwrap();
    assert_eq!(data2, b"via direct");

    // Check that at least one side switched to direct
    // (Both should eventually, but timing may vary)
    let a_direct = !conn_a.is_relayed().await;
    let b_direct = !conn_b.is_relayed().await;
    assert!(
        a_direct || b_direct,
        "at least one side should have switched to direct"
    );
}

// ── Stress test: multiple channels through relay ──

use std::collections::HashSet;

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for &b in data {
        sum = sum.wrapping_add(b as u32).wrapping_mul(31);
    }
    sum
}

fn make_payload(seq: u32, size: usize) -> Vec<u8> {
    let mut inner = Vec::with_capacity(4 + size + 4);
    inner.extend_from_slice(&seq.to_be_bytes());
    for i in 0..size {
        inner.push(((seq as usize * 7 + i * 13) & 0xFF) as u8);
    }
    let cs = checksum(&inner);
    inner.extend_from_slice(&cs.to_be_bytes());
    inner
}

/// Wrap payload with length prefix for stream (byte stream has no message boundaries)
fn frame_message(payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Read a length-prefixed message from a stream buffer
struct StreamReader {
    buffer: Vec<u8>,
}

impl StreamReader {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    fn try_read(&mut self) -> Option<Vec<u8>> {
        if self.buffer.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        if self.buffer.len() < 4 + len {
            return None;
        }
        let msg = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(..4 + len);
        Some(msg)
    }
}

fn verify_payload(data: &[u8]) -> Option<u32> {
    if data.len() < 8 {
        return None;
    }
    let seq = u32::from_be_bytes(data[..4].try_into().ok()?);
    let expected_cs = u32::from_be_bytes(data[data.len() - 4..].try_into().ok()?);
    let actual_cs = checksum(&data[..data.len() - 4]);
    if actual_cs != expected_cs {
        return None;
    }
    Some(seq)
}

#[tokio::test(flavor = "multi_thread")]
async fn stress_multiple_channels_through_relay() {
    init_tracing();

    const NUM_STREAMS: usize = 5;
    const NUM_DATAGRAMS: usize = 3;
    const STREAM_MESSAGES: u32 = 100;
    const DATAGRAM_MESSAGES: u32 = 100;
    const STREAM_PAYLOAD_SIZE: usize = 200;
    const DATAGRAM_PAYLOAD_SIZE: usize = 500;

    // Start relay
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    // Peer A and B
    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = std::sync::Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let node_b2 = node_b.clone();
    let accept = tokio::spawn(async move { node_b2.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    assert!(conn_a.is_established().await);
    assert!(conn_b.is_established().await);

    let mut send_tasks = Vec::new();
    let mut recv_tasks = Vec::new();

    // Open stream channels (reliable, ordered)
    for i in 0..NUM_STREAMS {
        let purpose = 0x100 + i as u16;
        let sa = conn_a.open_stream(purpose).await.unwrap();
        let (sb, got_purpose) = conn_b.accept_stream().await.unwrap();
        assert_eq!(got_purpose, purpose);

        // Sender — length-prefixed messages
        send_tasks.push(tokio::spawn(async move {
            for seq in 0..STREAM_MESSAGES {
                let payload = make_payload(seq, STREAM_PAYLOAD_SIZE);
                let framed = frame_message(&payload);
                sa.send(&framed).await.unwrap();
            }
        }));

        // Receiver — reassemble from byte stream
        let stream_idx = i;
        recv_tasks.push(tokio::spawn(async move {
            let mut reader = StreamReader::new();
            let mut expected_seq = 0u32;
            while expected_seq < STREAM_MESSAGES {
                // Try to extract messages from buffer
                while let Some(msg) = reader.try_read() {
                    match verify_payload(&msg) {
                        Some(seq) => {
                            assert_eq!(
                                seq, expected_seq,
                                "stream {stream_idx}: expected seq {expected_seq}, got {seq}"
                            );
                            expected_seq += 1;
                        }
                        None => {
                            panic!(
                                "stream {stream_idx} seq {expected_seq}: corrupted payload, len={}",
                                msg.len()
                            );
                        }
                    }
                }
                if expected_seq >= STREAM_MESSAGES {
                    break;
                }
                // Need more data
                match tokio::time::timeout(Duration::from_secs(20), sb.recv()).await {
                    Ok(Ok(data)) => reader.push(&data),
                    Ok(Err(e)) => panic!("stream {stream_idx} seq {expected_seq}: recv error: {e}"),
                    Err(_) => panic!("stream {stream_idx} seq {expected_seq}: recv timeout 20s"),
                }
            }
            expected_seq
        }));
    }

    // Open datagram channels (unreliable)
    for i in 0..NUM_DATAGRAMS {
        let purpose = 0x200 + i as u16;
        let da = conn_a.open_datagram(purpose).await.unwrap();
        let (db, got_purpose) = conn_b.accept_datagram().await.unwrap();
        assert_eq!(got_purpose, purpose);

        // Sender
        send_tasks.push(tokio::spawn(async move {
            for seq in 0..DATAGRAM_MESSAGES {
                let payload = make_payload(seq, DATAGRAM_PAYLOAD_SIZE);
                da.send(&payload).await.unwrap();
                // Small delay to avoid flooding
                if seq % 10 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }));

        // Receiver — collect unique sequence numbers (order doesn't matter)
        let dg_idx = i;
        recv_tasks.push(tokio::spawn(async move {
            let mut received = HashSet::new();
            loop {
                let result = tokio::time::timeout(Duration::from_secs(10), db.recv()).await;
                match result {
                    Ok(Ok(data)) => {
                        if let Some(seq) = verify_payload(&data) {
                            received.insert(seq);
                        } else {
                            panic!("datagram {dg_idx}: corrupted payload");
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => break, // timeout without data = done
                }
            }
            received.len() as u32
        }));
    }

    // Wait for all senders
    for task in send_tasks {
        task.await.unwrap();
    }

    // Small delay for last packets to arrive
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Wait for all receivers
    let mut stream_results = Vec::new();
    let mut datagram_results = Vec::new();

    for (i, task) in recv_tasks.into_iter().enumerate() {
        let kind = if i < NUM_STREAMS { "stream" } else { "datagram" };
        let result = tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap_or_else(|_| panic!("receiver {i} ({kind}) timed out after 30s"))
            .unwrap();
        if i < NUM_STREAMS {
            stream_results.push(result);
        } else {
            datagram_results.push(result);
        }
    }

    // Verify streams: all messages received in order
    for (i, count) in stream_results.iter().enumerate() {
        assert_eq!(
            *count, STREAM_MESSAGES,
            "stream {i}: expected {STREAM_MESSAGES} messages, got {count}"
        );
    }

    // Verify datagrams: at least some messages received (unreliable)
    for (i, count) in datagram_results.iter().enumerate() {
        assert!(
            *count > 0,
            "datagram {i}: received 0 messages out of {DATAGRAM_MESSAGES}"
        );
        tracing::info!(
            "datagram {}: received {}/{} messages ({}%)",
            i,
            count,
            DATAGRAM_MESSAGES,
            count * 100 / DATAGRAM_MESSAGES
        );
    }

    tracing::info!(
        "Stress test passed: {} streams × {} msgs + {} datagrams × {} msgs through relay",
        NUM_STREAMS,
        STREAM_MESSAGES,
        NUM_DATAGRAMS,
        DATAGRAM_MESSAGES,
    );
}

#[tokio::test]
async fn relay_datagram_simple() {
    init_tracing();

    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = std::sync::Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let nb = node_b.clone();
    let accept = tokio::spawn(async move { nb.accept().await.unwrap() });
    let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
    let conn_b = accept.await.unwrap();

    let da = conn_a.open_datagram(99).await.unwrap();
    let (db, purpose) = conn_b.accept_datagram().await.unwrap();
    assert_eq!(purpose, 99);

    for i in 0..10u32 {
        da.send(&i.to_be_bytes()).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut received = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(2), db.recv()).await {
            Ok(Ok(_)) => received += 1,
            _ => break,
        }
    }
    eprintln!("relay_datagram_simple: received {received}/10");
    assert!(received > 0, "received 0 datagrams through relay");
}
