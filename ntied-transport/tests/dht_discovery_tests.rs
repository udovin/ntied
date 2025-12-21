//! Tests for DHT discovery module.
//!
//! These tests focus on unit testing helper functions and basic DHT discovery functionality.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Re-implementation of address encoding for testing purposes.
fn encode_socket_addr(addr: SocketAddrV4) -> Vec<u8> {
    let mut buf = Vec::with_capacity(6);
    buf.extend_from_slice(&addr.ip().octets());
    buf.extend_from_slice(&addr.port().to_be_bytes());
    buf
}

/// Re-implementation of address decoding for testing purposes.
fn decode_socket_addr(data: &[u8]) -> Option<SocketAddrV4> {
    if data.len() < 6 {
        return None;
    }
    let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
    let port = u16::from_be_bytes([data[4], data[5]]);
    Some(SocketAddrV4::new(ip, port))
}

// STUN constants (same as in dht_discovery.rs)
const STUN_MAGIC_COOKIE: u32 = 0x2112A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;

/// Build STUN binding request (same as in dht_discovery.rs).
fn build_stun_request(transaction_id: &[u8; 12]) -> Vec<u8> {
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    request.extend_from_slice(&0u16.to_be_bytes()); // Message length = 0
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(transaction_id);
    request
}

/// Parse STUN binding response (same as in dht_discovery.rs).
fn parse_stun_response(data: &[u8], expected_tid: &[u8; 12]) -> Option<SocketAddr> {
    if data.len() < 20 {
        return None;
    }

    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != STUN_BINDING_RESPONSE {
        return None;
    }

    let magic = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if magic != STUN_MAGIC_COOKIE {
        return None;
    }

    if &data[8..20] != expected_tid {
        return None;
    }

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    if data.len() < 20 + msg_len {
        return None;
    }

    let mut pos = 20;
    while pos + 4 <= data.len() {
        let attr_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let attr_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if pos + attr_len > data.len() {
            break;
        }

        if attr_type == STUN_ATTR_XOR_MAPPED_ADDRESS && attr_len >= 8 {
            let family = data[pos + 1];
            if family == 0x01 {
                // IPv4
                let xor_port = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                let port = xor_port ^ (STUN_MAGIC_COOKIE >> 16) as u16;

                let xor_ip = u32::from_be_bytes([
                    data[pos + 4],
                    data[pos + 5],
                    data[pos + 6],
                    data[pos + 7],
                ]);
                let ip = xor_ip ^ STUN_MAGIC_COOKIE;

                return Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)));
            }
        } else if attr_type == STUN_ATTR_MAPPED_ADDRESS && attr_len >= 8 {
            let family = data[pos + 1];
            if family == 0x01 {
                // IPv4
                let port = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                let ip = Ipv4Addr::new(data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]);
                return Some(SocketAddr::V4(SocketAddrV4::new(ip, port)));
            }
        }

        // Align to 4-byte boundary
        pos += (attr_len + 3) & !3;
    }

    None
}

// ============================================================================
// Address encoding/decoding tests
// ============================================================================

#[test]
fn test_encode_decode_socket_addr() {
    let addr = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 100), 12345);
    let encoded = encode_socket_addr(addr);

    assert_eq!(encoded.len(), 6);
    assert_eq!(&encoded[0..4], &[192, 168, 1, 100]);
    assert_eq!(&encoded[4..6], &12345u16.to_be_bytes());

    let decoded = decode_socket_addr(&encoded).unwrap();
    assert_eq!(decoded, addr);
}

#[test]
fn test_encode_decode_various_addresses() {
    let test_cases = [
        SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0),
        SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 8080),
        SocketAddrV4::new(Ipv4Addr::new(255, 255, 255, 255), 65535),
        SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 1),
    ];

    for addr in test_cases {
        let encoded = encode_socket_addr(addr);
        let decoded = decode_socket_addr(&encoded).unwrap();
        assert_eq!(decoded, addr, "Round-trip failed for {:?}", addr);
    }
}

#[test]
fn test_decode_socket_addr_too_short() {
    assert!(decode_socket_addr(&[]).is_none());
    assert!(decode_socket_addr(&[1, 2, 3, 4, 5]).is_none());
}

// ============================================================================
// STUN protocol tests
// ============================================================================

#[test]
fn test_build_stun_request() {
    let tid: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let request = build_stun_request(&tid);

    assert_eq!(request.len(), 20);

    // Check message type (Binding Request)
    assert_eq!(&request[0..2], &STUN_BINDING_REQUEST.to_be_bytes());

    // Check message length (0)
    assert_eq!(&request[2..4], &[0, 0]);

    // Check magic cookie
    assert_eq!(&request[4..8], &STUN_MAGIC_COOKIE.to_be_bytes());

    // Check transaction ID
    assert_eq!(&request[8..20], &tid);
}

#[test]
fn test_parse_stun_response_too_short() {
    let tid: [u8; 12] = [0; 12];
    assert!(parse_stun_response(&[], &tid).is_none());
    assert!(parse_stun_response(&[0; 19], &tid).is_none());
}

#[test]
fn test_parse_stun_response_wrong_type() {
    let tid: [u8; 12] = [0; 12];
    let mut response = vec![0; 20];
    // Set wrong message type
    response[0..2].copy_from_slice(&0x0111u16.to_be_bytes());
    response[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());

    assert!(parse_stun_response(&response, &tid).is_none());
}

#[test]
fn test_parse_stun_response_wrong_magic() {
    let tid: [u8; 12] = [0; 12];
    let mut response = vec![0; 20];
    response[0..2].copy_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    // Wrong magic cookie
    response[4..8].copy_from_slice(&0x12345678u32.to_be_bytes());

    assert!(parse_stun_response(&response, &tid).is_none());
}

#[test]
fn test_parse_stun_response_wrong_tid() {
    let tid: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let wrong_tid: [u8; 12] = [0; 12];
    let mut response = vec![0; 20];
    response[0..2].copy_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    response[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    response[8..20].copy_from_slice(&wrong_tid);

    assert!(parse_stun_response(&response, &tid).is_none());
}

#[test]
fn test_parse_stun_response_xor_mapped_address() {
    let tid: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    // Build a valid STUN response with XOR-MAPPED-ADDRESS
    let expected_ip = Ipv4Addr::new(203, 0, 113, 1);
    let expected_port: u16 = 54321;

    // XOR the values
    let xor_port = expected_port ^ (STUN_MAGIC_COOKIE >> 16) as u16;
    let ip_u32 = u32::from(expected_ip);
    let xor_ip = ip_u32 ^ STUN_MAGIC_COOKIE;

    let mut response = Vec::new();
    // Header
    response.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    response.extend_from_slice(&12u16.to_be_bytes()); // Message length
    response.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    response.extend_from_slice(&tid);

    // XOR-MAPPED-ADDRESS attribute
    response.extend_from_slice(&STUN_ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    response.extend_from_slice(&8u16.to_be_bytes()); // Attribute length
    response.push(0); // Reserved
    response.push(0x01); // Family: IPv4
    response.extend_from_slice(&xor_port.to_be_bytes());
    response.extend_from_slice(&xor_ip.to_be_bytes());

    let result = parse_stun_response(&response, &tid);
    assert!(result.is_some());

    let addr = result.unwrap();
    match addr {
        SocketAddr::V4(v4) => {
            assert_eq!(*v4.ip(), expected_ip);
            assert_eq!(v4.port(), expected_port);
        }
        _ => panic!("Expected IPv4 address"),
    }
}

#[test]
fn test_parse_stun_response_mapped_address() {
    let tid: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    let expected_ip = Ipv4Addr::new(192, 168, 1, 1);
    let expected_port: u16 = 8080;

    let mut response = Vec::new();
    // Header
    response.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    response.extend_from_slice(&12u16.to_be_bytes()); // Message length
    response.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    response.extend_from_slice(&tid);

    // MAPPED-ADDRESS attribute
    response.extend_from_slice(&STUN_ATTR_MAPPED_ADDRESS.to_be_bytes());
    response.extend_from_slice(&8u16.to_be_bytes()); // Attribute length
    response.push(0); // Reserved
    response.push(0x01); // Family: IPv4
    response.extend_from_slice(&expected_port.to_be_bytes());
    response.extend_from_slice(&expected_ip.octets());

    let result = parse_stun_response(&response, &tid);
    assert!(result.is_some());

    let addr = result.unwrap();
    match addr {
        SocketAddr::V4(v4) => {
            assert_eq!(*v4.ip(), expected_ip);
            assert_eq!(v4.port(), expected_port);
        }
        _ => panic!("Expected IPv4 address"),
    }
}

// ============================================================================
// DHT key derivation tests
// ============================================================================

#[test]
fn test_get_dht_key_for_public_key() {
    use ntied_crypto::PrivateKey;
    use ntied_transport::dht_discovery::get_dht_key_for_public_key;

    let private_key = PrivateKey::generate().unwrap();
    let public_key = private_key.public_key();

    let key1 = get_dht_key_for_public_key(&public_key).unwrap();
    let key2 = get_dht_key_for_public_key(&public_key).unwrap();

    // Same public key should produce same DHT key
    assert_eq!(key1, key2);

    // Different public key should produce different DHT key
    let another_private_key = PrivateKey::generate().unwrap();
    let another_public_key = another_private_key.public_key();
    let another_key = get_dht_key_for_public_key(&another_public_key).unwrap();

    assert_ne!(key1, another_key);
}

// ============================================================================
// DhtDiscoveryFactory tests
// ============================================================================

#[test]
fn test_dht_discovery_factory_creation() {
    use ntied_transport::dht_discovery::DhtDiscoveryFactory;

    // Test basic creation
    let _factory = DhtDiscoveryFactory::new();

    // Test with bootstrap nodes
    let _factory_with_bootstrap =
        DhtDiscoveryFactory::with_bootstrap(vec!["127.0.0.1:6881".to_string()]);
}

// ============================================================================
// Integration tests for DhtDiscovery
// ============================================================================

mod integration {
    use std::net::SocketAddr;
    use std::time::Duration;

    use ntied_crypto::PrivateKey;
    use ntied_transport::Transport;
    use ntied_transport::dht_discovery::DhtDiscoveryFactory;

    fn init_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(format!(
                "{}=trace,ntied_transport=trace,ntied_server=debug",
                module_path!()
            ))
            .try_init();
    }

    /// Test that DhtDiscovery can be created with real STUN.
    #[tokio::test]
    async fn test_dht_discovery_creation() {
        init_tracing();

        let private_key = PrivateKey::generate().unwrap();

        // Create transport with DHT discovery using real STUN
        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

        let factory = DhtDiscoveryFactory::new();

        let result = Transport::bind_with_discovery(bind_addr, private_key, factory).await;

        assert!(
            result.is_ok(),
            "Failed to create transport with DHT discovery: {:?}",
            result.err()
        );

        let transport = result.unwrap();
        assert_ne!(transport.local_addr().port(), 0);
        tracing::info!(local_addr = ?transport.local_addr(), "Transport created with DHT discovery");
    }

    /// Test that two peers can discover each other using real DHT/STUN.
    #[tokio::test]
    async fn test_dht_two_peers_discovery() {
        init_tracing();

        let private_key1 = PrivateKey::generate().unwrap();
        let public_key1 = private_key1.public_key();
        let private_key2 = PrivateKey::generate().unwrap();
        let public_key2 = private_key2.public_key();

        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

        // Create first transport with real DHT/STUN
        let factory1 = DhtDiscoveryFactory::new();
        let transport1 = Transport::bind_with_discovery(bind_addr, private_key1, factory1)
            .await
            .expect("Failed to create transport1");

        let transport1_addr = transport1.local_addr();
        tracing::info!(?transport1_addr, ?public_key1, "Transport 1 created");

        // Create second transport with default DHT bootstrap
        let factory2 = DhtDiscoveryFactory::new();
        let transport2 = Transport::bind_with_discovery(bind_addr, private_key2, factory2)
            .await
            .expect("Failed to create transport2");

        let transport2_addr = transport2.local_addr();
        tracing::info!(?transport2_addr, ?public_key2, "Transport 2 created");

        // Give DHT time to bootstrap and publish addresses via STUN
        tokio::time::sleep(Duration::from_secs(15)).await;

        // Try to connect - this exercises the DHT discovery path
        let connect_result =
            tokio::time::timeout(Duration::from_secs(30), transport1.connect(&public_key2)).await;

        match connect_result {
            Ok(Ok(conn)) => {
                tracing::info!("Connection established successfully via DHT!");
                drop(conn);
            }
            Ok(Err(e)) => {
                tracing::error!(?e, "Connection failed");
                panic!("DHT connection failed: {:?}", e);
            }
            Err(_) => {
                panic!("Connection timed out");
            }
        }

        drop(transport1);
        drop(transport2);
    }

    /// Test full connection cycle through DHT discovery with data exchange.
    ///
    /// This test creates two transports using DHT discovery and verifies they can
    /// discover each other via real STUN/DHT, establish a connection, and exchange data.
    #[tokio::test]
    async fn test_dht_connect_and_exchange_data() {
        init_tracing();

        let private_key1 = PrivateKey::generate().unwrap();
        let public_key1 = private_key1.public_key();
        let private_key2 = PrivateKey::generate().unwrap();
        let public_key2 = private_key2.public_key();

        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

        // Create transport1 with real DHT discovery (uses STUN for public address)
        let factory1 = DhtDiscoveryFactory::new();
        let transport1 = Transport::bind_with_discovery(bind_addr, private_key1, factory1)
            .await
            .expect("Failed to create transport1");
        let transport1_addr = transport1.local_addr();
        tracing::info!(?transport1_addr, ?public_key1, "Transport 1 created");

        // Create transport2 with default DHT bootstrap
        let factory2 = DhtDiscoveryFactory::new();
        let transport2 = Transport::bind_with_discovery(bind_addr, private_key2, factory2)
            .await
            .expect("Failed to create transport2");
        let transport2_addr = transport2.local_addr();
        tracing::info!(?transport2_addr, ?public_key2, "Transport 2 created");

        // Give DHT time to bootstrap and publish addresses via STUN
        tokio::time::sleep(Duration::from_secs(15)).await;

        // Spawn connect and accept tasks concurrently
        let pk2_clone = public_key2.clone();
        let connect_handle = tokio::spawn(async move { transport1.connect(&pk2_clone).await });

        let accept_handle = tokio::spawn(async move { transport2.accept().await });

        // Wait with timeout (longer for real network operations)
        let results = tokio::time::timeout(Duration::from_secs(30), async {
            let conn_result = connect_handle.await;
            let accept_result = accept_handle.await;
            (conn_result, accept_result)
        })
        .await;

        match results {
            Ok((Ok(Ok(conn1)), Ok(Ok(conn2)))) => {
                tracing::info!("Connection established via DHT discovery!");

                // Verify bidirectional data exchange
                let msg1 = "Hello from peer 1 via DHT!";
                conn1.send(msg1).await.expect("Send from conn1 failed");

                let recv1: String = conn2
                    .recv()
                    .await
                    .expect("Recv on conn2 failed")
                    .try_into()
                    .expect("Convert failed");
                assert_eq!(recv1, msg1);

                let msg2 = "Reply from peer 2!";
                conn2.send(msg2).await.expect("Send from conn2 failed");

                let recv2: String = conn1
                    .recv()
                    .await
                    .expect("Recv on conn1 failed")
                    .try_into()
                    .expect("Convert failed");
                assert_eq!(recv2, msg2);

                tracing::info!("Bidirectional data exchange verified!");
            }
            Ok((Ok(Err(e)), _)) => {
                panic!("Connect failed: {:?}", e);
            }
            Ok((_, Ok(Err(e)))) => {
                panic!("Accept failed: {:?}", e);
            }
            Ok((Err(e), _)) => panic!("Connect task panicked: {:?}", e),
            Ok((_, Err(e))) => panic!("Accept task panicked: {:?}", e),
            Err(_) => {
                panic!("Test timed out waiting for connection");
            }
        }
    }
}
