use ntied_transport::{
    Address, ServerConnectRequest, ServerConnectResponse, ServerErrorResponse,
    ServerIncomingConnectionResponse, ServerRegisterRequest, ServerRegisterResponse, ServerRequest,
    ServerResponse,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Test serialization and deserialization of ServerRequest::Heartbeat
#[test]
fn test_server_request_heartbeat() {
    let request = ServerRequest::Heartbeat;
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Heartbeat => {
            // Success - heartbeat has no fields to check
        }
        _ => panic!("Expected Heartbeat request"),
    }

    // Empty bytes should also deserialize to Heartbeat
    let empty_bytes = vec![];
    let deserialized_empty = ServerRequest::deserialize(&empty_bytes).unwrap();
    match deserialized_empty {
        ServerRequest::Heartbeat => {
            // Success
        }
        _ => panic!("Expected Heartbeat request from empty bytes"),
    }
}

/// Test serialization and deserialization of ServerRequest::Register
#[test]
fn test_server_request_register() {
    let request_id = 12345u32;
    let public_key = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let address = Address::from_bytes([10u8; 33]);

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: public_key.clone(),
        address,
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, request_id);
            assert_eq!(r.public_key, public_key);
            assert_eq!(r.address, address);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test serialization and deserialization of ServerRequest::Connect
#[test]
fn test_server_request_connect() {
    let request_id = 67890u32;
    let address = Address::from_bytes([20u8; 33]);
    let source_id = 42;

    let connect_request = ServerConnectRequest {
        request_id,
        address,
        source_id,
    };

    let request = ServerRequest::Connect(connect_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.address, address);
            assert_eq!(c.source_id, source_id);
        }
        _ => panic!("Expected Connect request"),
    }
}

/// Test serialization and deserialization of ServerResponse::Heartbeat
#[test]
fn test_server_response_heartbeat() {
    let response = ServerResponse::Heartbeat;
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Heartbeat => {
            // Success - heartbeat has no fields to check
        }
        _ => panic!("Expected Heartbeat response"),
    }

    // Empty bytes should also deserialize to Heartbeat
    let empty_bytes = vec![];
    let deserialized_empty = ServerResponse::deserialize(&empty_bytes).unwrap();
    match deserialized_empty {
        ServerResponse::Heartbeat => {
            // Success
        }
        _ => panic!("Expected Heartbeat response from empty bytes"),
    }
}

/// Test serialization and deserialization of ServerResponse::Register
#[test]
fn test_server_response_register() {
    let request_id = 11111u32;

    let register_response = ServerRegisterResponse { request_id };

    let response = ServerResponse::Register(register_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Register(r) => {
            assert_eq!(r.request_id, request_id);
        }
        _ => panic!("Expected Register response"),
    }
}

/// Test serialization and deserialization of ServerResponse::RegisterError
#[test]
fn test_server_response_register_error() {
    let request_id = 22222u32;
    let error_code = 404u16;

    let error_response = ServerErrorResponse {
        request_id,
        code: error_code,
    };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.request_id, request_id);
            assert_eq!(e.code, error_code);
        }
        _ => panic!("Expected RegisterError response"),
    }
}

/// Test serialization and deserialization of ServerResponse::Connect
#[test]
fn test_server_response_connect() {
    let request_id = 33333u32;
    let public_key = vec![10, 20, 30, 40, 50];
    let address = Address::from_bytes([30u8; 33]);
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 8080);

    let connect_response = ServerConnectResponse {
        request_id,
        public_key: public_key.clone(),
        address,
        addr: socket_addr,
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.address, address);
            assert_eq!(c.addr, socket_addr);
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test serialization and deserialization of ServerResponse::ConnectError
#[test]
fn test_server_response_connect_error() {
    let request_id = 44444u32;
    let error_code = 500u16;

    let error_response = ServerErrorResponse {
        request_id,
        code: error_code,
    };

    let response = ServerResponse::ConnectError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::ConnectError(e) => {
            assert_eq!(e.request_id, request_id);
            assert_eq!(e.code, error_code);
        }
        _ => panic!("Expected ConnectError response"),
    }
}

/// Test serialization and deserialization of ServerResponse::IncomingConnection
#[test]
fn test_server_response_incoming_connection() {
    let public_key = vec![100, 101, 102, 103, 104, 105];
    let address = Address::from_bytes([40u8; 33]);
    let socket_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 9999);
    let source_id = 42;

    let incoming_response = ServerIncomingConnectionResponse {
        public_key: public_key.clone(),
        address,
        addr: socket_addr,
        source_id,
    };

    let response = ServerResponse::IncomingConnection(incoming_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::IncomingConnection(i) => {
            assert_eq!(i.public_key, public_key);
            assert_eq!(i.address, address);
            assert_eq!(i.addr, socket_addr);
            assert_eq!(i.source_id, source_id);
        }
        _ => panic!("Expected IncomingConnection response"),
    }
}

/// Test ServerRequest with large public key
#[test]
fn test_server_request_large_public_key() {
    let request_id = 55555u32;
    let large_public_key = vec![42u8; 10000]; // 10KB public key
    let address = Address::from_bytes([50u8; 33]);

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: large_public_key.clone(),
        address,
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, request_id);
            assert_eq!(r.public_key, large_public_key);
            assert_eq!(r.public_key.len(), 10000);
            assert_eq!(r.address, address);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test ServerResponse with IPv6 address
#[test]
fn test_server_response_with_ipv6() {
    let request_id = 66666u32;
    let public_key = vec![60, 61, 62];
    let address = Address::from_bytes([60u8; 33]);
    let ipv6_addr = SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        443,
    );

    let connect_response = ServerConnectResponse {
        request_id,
        public_key: public_key.clone(),
        address,
        addr: ipv6_addr,
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.address, address);
            assert_eq!(c.addr, ipv6_addr);
            assert!(c.addr.is_ipv6());
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test invalid request type deserialization
#[test]
fn test_server_request_invalid_type() {
    let invalid_bytes = vec![99]; // Invalid request type
    let result = ServerRequest::deserialize(&invalid_bytes);
    assert!(result.is_err());
}

/// Test invalid response type deserialization
#[test]
fn test_server_response_invalid_type() {
    let invalid_bytes = vec![99]; // Invalid response type
    let result = ServerResponse::deserialize(&invalid_bytes);
    assert!(result.is_err());
}

/// Test ServerRequest deserialization with insufficient data
#[test]
fn test_server_request_insufficient_data() {
    // Register request with missing data
    let mut bytes = vec![1]; // Register type
    bytes.extend_from_slice(&12345u32.to_be_bytes()); // request_id
    // Missing public_key and address

    let result = ServerRequest::deserialize(&bytes);
    assert!(result.is_err());
}

/// Test ServerResponse deserialization with insufficient data
#[test]
fn test_server_response_insufficient_data() {
    // Connect response with missing data
    let mut bytes = vec![3]; // Connect type
    bytes.extend_from_slice(&12345u32.to_be_bytes()); // request_id
    // Missing public_key, address and socket addr

    let result = ServerResponse::deserialize(&bytes);
    assert!(result.is_err());
}

/// Test request type discrimination
#[test]
fn test_server_request_type_discrimination() {
    let requests = vec![
        (ServerRequest::Heartbeat, "Heartbeat"),
        (
            ServerRequest::Register(ServerRegisterRequest {
                request_id: 1,
                public_key: vec![1],
                address: Address::from_bytes([1u8; 33]),
            }),
            "Register",
        ),
        (
            ServerRequest::Connect(ServerConnectRequest {
                request_id: 2,
                address: Address::from_bytes([2u8; 33]),
                source_id: 42,
            }),
            "Connect",
        ),
    ];

    for (request, expected_type) in requests {
        let serialized = request.serialize();
        let deserialized = ServerRequest::deserialize(&serialized).unwrap();

        let actual_type = match deserialized {
            ServerRequest::Heartbeat => "Heartbeat",
            ServerRequest::Register(_) => "Register",
            ServerRequest::Connect(_) => "Connect",
        };

        assert_eq!(actual_type, expected_type);
    }
}

/// Test response type discrimination
#[test]
fn test_server_response_type_discrimination() {
    let responses = vec![
        (ServerResponse::Heartbeat, "Heartbeat"),
        (
            ServerResponse::Register(ServerRegisterResponse { request_id: 1 }),
            "Register",
        ),
        (
            ServerResponse::RegisterError(ServerErrorResponse {
                request_id: 2,
                code: 100,
            }),
            "RegisterError",
        ),
        (
            ServerResponse::Connect(ServerConnectResponse {
                request_id: 3,
                public_key: vec![3],
                address: Address::from_bytes([3u8; 33]),
                addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080),
            }),
            "Connect",
        ),
        (
            ServerResponse::ConnectError(ServerErrorResponse {
                request_id: 4,
                code: 200,
            }),
            "ConnectError",
        ),
        (
            ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
                public_key: vec![5],
                address: Address::from_bytes([5u8; 33]),
                addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 9090),
                source_id: 42,
            }),
            "IncomingConnection",
        ),
    ];

    for (response, expected_type) in responses {
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        let actual_type = match deserialized {
            ServerResponse::Heartbeat => "Heartbeat",
            ServerResponse::Register(_) => "Register",
            ServerResponse::RegisterError(_) => "RegisterError",
            ServerResponse::Connect(_) => "Connect",
            ServerResponse::ConnectError(_) => "ConnectError",
            ServerResponse::IncomingConnection(_) => "IncomingConnection",
        };

        assert_eq!(actual_type, expected_type);
    }
}

/// Test maximum request ID values
#[test]
fn test_max_request_id_values() {
    let max_request_id = u32::MAX;
    let address = Address::from_bytes([70u8; 33]);

    // Test with Register request
    let register_request = ServerRegisterRequest {
        request_id: max_request_id,
        public_key: vec![70, 71, 72],
        address,
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, max_request_id);
        }
        _ => panic!("Expected Register request"),
    }

    // Test with Connect response
    let connect_response = ServerConnectResponse {
        request_id: max_request_id,
        public_key: vec![73, 74, 75],
        address,
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 53),
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, max_request_id);
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test maximum error code values
#[test]
fn test_max_error_code_values() {
    let request_id = 77777u32;
    let max_error_code = u16::MAX;

    let error_response = ServerErrorResponse {
        request_id,
        code: max_error_code,
    };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.request_id, request_id);
            assert_eq!(e.code, max_error_code);
        }
        _ => panic!("Expected RegisterError response"),
    }
}

/// Test round-trip serialization with all address patterns
#[test]
fn test_address_patterns_in_messages() {
    // All zeros address
    let zeros_address = Address::from_bytes([0u8; 33]);

    // All ones address
    let ones_address = Address::from_bytes([0xFFu8; 33]);

    // Alternating pattern address
    let mut alternating = [0u8; 33];
    for i in 0..33 {
        alternating[i] = if i % 2 == 0 { 0xAA } else { 0x55 };
    }
    let alt_address = Address::from_bytes(alternating);

    let addresses = vec![zeros_address, ones_address, alt_address];

    for (idx, address) in addresses.iter().enumerate() {
        let request_id = (idx + 1) as u32 * 10000;

        // Test in Connect request
        let connect_request = ServerConnectRequest {
            request_id,
            address: *address,
            source_id: 42,
        };

        let request = ServerRequest::Connect(connect_request);
        let serialized = request.serialize();
        let deserialized = ServerRequest::deserialize(&serialized).unwrap();

        match deserialized {
            ServerRequest::Connect(c) => {
                assert_eq!(c.address, *address);
            }
            _ => panic!("Expected Connect request"),
        }

        // Test in IncomingConnection response
        let incoming_response = ServerIncomingConnectionResponse {
            public_key: vec![80 + idx as u8],
            address: *address,
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, idx as u8 + 1)), 8000),
            source_id: 42,
        };

        let response = ServerResponse::IncomingConnection(incoming_response);
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::IncomingConnection(i) => {
                assert_eq!(i.address, *address);
            }
            _ => panic!("Expected IncomingConnection response"),
        }
    }
}

/// Test empty public key handling
#[test]
fn test_empty_public_key() {
    let request_id = 88888u32;
    let empty_public_key = vec![];
    let address = Address::from_bytes([80u8; 33]);

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: empty_public_key.clone(),
        address,
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, request_id);
            assert_eq!(r.public_key, empty_public_key);
            assert!(r.public_key.is_empty());
            assert_eq!(r.address, address);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test various socket address port numbers
#[test]
fn test_various_port_numbers() {
    let ports = vec![0, 80, 443, 8080, 32768, 65535];
    let public_key = vec![90, 91, 92];
    let address = Address::from_bytes([90u8; 33]);
    let source_id = 100;

    for port in ports {
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);

        let incoming_response = ServerIncomingConnectionResponse {
            public_key: public_key.clone(),
            address,
            addr: socket_addr,
            source_id,
        };

        let response = ServerResponse::IncomingConnection(incoming_response);
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::IncomingConnection(i) => {
                assert_eq!(i.addr.port(), port);
                assert_eq!(i.addr, socket_addr);
                assert_eq!(i.source_id, source_id);
            }
            _ => panic!("Expected IncomingConnection response"),
        }
    }
}

/// Test serialization with minimum values for all fields
#[test]
fn test_minimum_values() {
    let min_request_id = 0u32;
    let min_error_code = 0u16;
    let min_address = Address::from_bytes([0u8; 33]);
    let min_socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);

    // Test minimum values in RegisterError
    let error_response = ServerErrorResponse {
        request_id: min_request_id,
        code: min_error_code,
    };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.request_id, min_request_id);
            assert_eq!(e.code, min_error_code);
        }
        _ => panic!("Expected RegisterError response"),
    }

    // Test minimum values in Connect response
    let connect_response = ServerConnectResponse {
        request_id: min_request_id,
        public_key: vec![],
        address: min_address,
        addr: min_socket_addr,
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, min_request_id);
            assert!(c.public_key.is_empty());
            assert_eq!(c.address, min_address);
            assert_eq!(c.addr, min_socket_addr);
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test serialization consistency with repeated operations
#[test]
fn test_serialization_consistency() {
    let request = ServerRequest::Register(ServerRegisterRequest {
        request_id: 999999,
        public_key: vec![1, 2, 3, 4, 5],
        address: Address::from_bytes([123u8; 33]),
    });

    // Serialize multiple times and ensure consistency
    let serialized1 = request.serialize();
    let serialized2 = request.serialize();
    let serialized3 = request.serialize();

    assert_eq!(serialized1, serialized2);
    assert_eq!(serialized2, serialized3);

    // Deserialize and re-serialize should produce the same bytes
    let deserialized = ServerRequest::deserialize(&serialized1).unwrap();
    let reserialized = deserialized.serialize();
    assert_eq!(serialized1, reserialized);
}

/// Test mixed IPv4 and IPv6 addresses in the same message flow
#[test]
fn test_mixed_ip_versions() {
    let ipv4_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)), 8080);
    let ipv6_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 8080);

    let responses = vec![
        ServerResponse::Connect(ServerConnectResponse {
            request_id: 1,
            public_key: vec![1],
            address: Address::from_bytes([1u8; 33]),
            addr: ipv4_addr,
        }),
        ServerResponse::Connect(ServerConnectResponse {
            request_id: 2,
            public_key: vec![2],
            address: Address::from_bytes([2u8; 33]),
            addr: ipv6_addr,
        }),
    ];

    for response in responses {
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match (&response, deserialized) {
            (ServerResponse::Connect(orig), ServerResponse::Connect(deser)) => {
                assert_eq!(orig.addr, deser.addr);
                assert_eq!(orig.addr.is_ipv4(), deser.addr.is_ipv4());
                assert_eq!(orig.addr.is_ipv6(), deser.addr.is_ipv6());
            }
            _ => panic!("Unexpected response type"),
        }
    }
}

/// Test with maximum length public key (up to u16::MAX)
#[test]
fn test_maximum_public_key_size() {
    // Create a public key at the maximum size (u16::MAX bytes)
    let max_size = u16::MAX as usize;
    let large_public_key = vec![0xAB; max_size];

    let register_request = ServerRegisterRequest {
        request_id: 12345,
        public_key: large_public_key.clone(),
        address: Address::from_bytes([0xCD; 33]),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();

    // Verify serialization succeeded and size is as expected
    // 1 byte (type) + 4 bytes (request_id) + 2 bytes (length) + max_size bytes (public_key) + 30 bytes (address)
    assert_eq!(serialized.len(), 1 + 4 + 2 + max_size + 33);

    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key.len(), max_size);
            assert_eq!(r.public_key, large_public_key);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test error codes across the full range
#[test]
fn test_error_code_range() {
    let error_codes = vec![
        0u16,  // Minimum
        100,   // Common HTTP-like code
        404,   // Not found
        500,   // Server error
        1000,  // Custom range
        32767, // Mid-range
        65535, // Maximum
    ];

    for code in error_codes {
        let error_response = ServerErrorResponse {
            request_id: code as u32,
            code,
        };

        let response = ServerResponse::ConnectError(error_response);
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::ConnectError(e) => {
                assert_eq!(e.code, code);
            }
            _ => panic!("Expected ConnectError response"),
        }
    }
}

/// Test handling of localhost addresses
#[test]
fn test_localhost_addresses() {
    let localhost_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3000);
    let localhost_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 3000);

    let responses = vec![
        ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
            public_key: vec![10, 20, 30],
            address: Address::from_bytes([100u8; 33]),
            addr: localhost_v4,
            source_id: 1,
        }),
        ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
            public_key: vec![40, 50, 60],
            address: Address::from_bytes([200u8; 33]),
            addr: localhost_v6,
            source_id: 2,
        }),
    ];

    for response in responses {
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match (&response, deserialized) {
            (
                ServerResponse::IncomingConnection(orig),
                ServerResponse::IncomingConnection(deser),
            ) => {
                assert_eq!(orig.addr, deser.addr);
                assert!(deser.addr.ip().is_loopback());
            }
            _ => panic!("Unexpected response type"),
        }
    }
}

/// Test special IPv6 addresses
#[test]
fn test_special_ipv6_addresses() {
    let special_addrs = vec![
        // IPv6 loopback
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 8080),
        // IPv6 unspecified
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0)), 8080),
        // IPv6 link-local
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 8080),
        // IPv6 multicast
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)), 8080),
        // IPv4-mapped IPv6
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0001)),
            8080,
        ),
    ];

    for addr in special_addrs {
        let response = ServerResponse::Connect(ServerConnectResponse {
            request_id: 1,
            public_key: vec![1, 2, 3],
            address: Address::from_bytes([50u8; 33]),
            addr,
        });

        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::Connect(c) => {
                assert_eq!(c.addr, addr);
            }
            _ => panic!("Expected Connect response"),
        }
    }
}

/// Test sequential request IDs
#[test]
fn test_sequential_request_ids() {
    let mut requests = Vec::new();

    // Generate sequential requests
    for i in 0..100u32 {
        requests.push(ServerRequest::Register(ServerRegisterRequest {
            request_id: i,
            public_key: vec![i as u8],
            address: Address::from_bytes([i as u8; 33]),
        }));
    }

    // Serialize and deserialize each request
    for (idx, request) in requests.iter().enumerate() {
        let serialized = request.serialize();
        let deserialized = ServerRequest::deserialize(&serialized).unwrap();

        match deserialized {
            ServerRequest::Register(r) => {
                assert_eq!(r.request_id, idx as u32);
                assert_eq!(r.public_key, vec![idx as u8]);
            }
            _ => panic!("Expected Register request"),
        }
    }
}

/// Test boundary port numbers
#[test]
fn test_boundary_port_numbers() {
    let boundary_ports = vec![
        0,     // Minimum port
        1,     // System port start
        1023,  // Last system port
        1024,  // First user port
        49151, // Last registered port
        49152, // First dynamic port
        65535, // Maximum port
    ];

    for port in boundary_ports {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port);

        let response = ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
            public_key: vec![1],
            address: Address::from_bytes([1u8; 33]),
            addr,
            source_id: 3,
        });

        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::IncomingConnection(i) => {
                assert_eq!(i.addr.port(), port);
            }
            _ => panic!("Expected IncomingConnection response"),
        }
    }
}

/// Test request with corrupted length field
#[test]
fn test_corrupted_length_field() {
    // Create a valid Register request first
    let mut bytes = vec![1]; // Register type
    bytes.extend_from_slice(&12345u32.to_be_bytes()); // request_id

    // Add corrupted length field (says 1000 bytes but only provides 5)
    bytes.extend_from_slice(&1000u16.to_be_bytes()); // Wrong length
    bytes.extend_from_slice(&[1, 2, 3, 4, 5]); // Actual data

    let result = ServerRequest::deserialize(&bytes);
    assert!(result.is_err());
}

/// Test response with all fields at maximum values
#[test]
fn test_all_maximum_values() {
    let max_request_id = u32::MAX;
    let max_port = u16::MAX;
    let max_address = Address::from_bytes([0xFF; 33]);
    let max_public_key = vec![0xFF; 1000]; // Reasonably large public key

    let response = ServerResponse::Connect(ServerConnectResponse {
        request_id: max_request_id,
        public_key: max_public_key.clone(),
        address: max_address,
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)), max_port),
    });

    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, max_request_id);
            assert_eq!(c.public_key, max_public_key);
            assert_eq!(c.address, max_address);
            assert_eq!(c.addr.port(), max_port);
        }
        _ => panic!("Expected Connect response"),
    }
}
