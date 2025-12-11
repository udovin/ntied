use ntied_transport::{
    ServerConnectRequest, ServerConnectResponse, ServerErrorResponse,
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

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: public_key.clone(),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, request_id);
            assert_eq!(r.public_key, public_key);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test serialization and deserialization of ServerRequest::Connect
#[test]
fn test_server_request_connect() {
    let request_id = 67890u32;
    let public_key = vec![20u8; 33];
    let source_id = 42;

    let connect_request = ServerConnectRequest {
        request_id,
        public_key: public_key.clone(),
        connection_id: source_id,
    };

    let request = ServerRequest::Connect(connect_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.connection_id, source_id);
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
    let code = 404u16;

    let error_response = ServerErrorResponse { request_id, code };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.request_id, request_id);
            assert_eq!(e.code, code);
        }
        _ => panic!("Expected RegisterError response"),
    }
}

/// Test serialization and deserialization of ServerResponse::Connect
#[test]
fn test_server_response_connect() {
    let request_id = 33333u32;
    let public_key = vec![30u8; 33];
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);

    let connect_response = ServerConnectResponse {
        request_id,
        public_key: public_key.clone(),
        socket_addr,
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.socket_addr, socket_addr);
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test serialization and deserialization of ServerResponse::ConnectError
#[test]
fn test_server_response_connect_error() {
    let request_id = 44444u32;
    let code = 500u16;

    let error_response = ServerErrorResponse { request_id, code };

    let response = ServerResponse::ConnectError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::ConnectError(e) => {
            assert_eq!(e.request_id, request_id);
            assert_eq!(e.code, code);
        }
        _ => panic!("Expected ConnectError response"),
    }
}

/// Test serialization and deserialization of ServerResponse::IncomingConnection
#[test]
fn test_server_response_incoming_connection() {
    let public_key = vec![50u8; 33];
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 9090);
    let source_id = 99;

    let incoming_response = ServerIncomingConnectionResponse {
        public_key: public_key.clone(),
        socket_addr,
        connection_id: source_id,
    };

    let response = ServerResponse::IncomingConnection(incoming_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::IncomingConnection(i) => {
            assert_eq!(i.public_key, public_key);
            assert_eq!(i.socket_addr, socket_addr);
            assert_eq!(i.connection_id, source_id);
        }
        _ => panic!("Expected IncomingConnection response"),
    }
}

/// Test serialization and deserialization with large public key
#[test]
fn test_server_request_large_public_key() {
    let request_id = 55555u32;
    let public_key = vec![0xFFu8; 1024]; // 1KB public key

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: public_key.clone(),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, request_id);
            assert_eq!(r.public_key, public_key);
            assert_eq!(r.public_key.len(), 1024);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test serialization and deserialization with IPv6 address
#[test]
fn test_server_response_with_ipv6() {
    let request_id = 66666u32;
    let public_key = vec![60u8; 33];
    let socket_addr = SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0, 0, 0x8a2e, 0x0370, 0x7334,
        )),
        443,
    );

    let connect_response = ServerConnectResponse {
        request_id,
        public_key: public_key.clone(),
        socket_addr,
    };

    let response = ServerResponse::Connect(connect_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, request_id);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.socket_addr, socket_addr);
            assert!(c.socket_addr.ip().is_ipv6());
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

/// Test deserialization with insufficient data for Register request
#[test]
fn test_server_request_insufficient_data() {
    // Type byte only, missing request_id and public_key
    let incomplete_bytes = vec![1];
    let result = ServerRequest::deserialize(&incomplete_bytes);
    assert!(result.is_err());
}

/// Test deserialization with insufficient data for Connect response
#[test]
fn test_server_response_insufficient_data() {
    // Type byte only, missing data
    let incomplete_bytes = vec![3];
    let result = ServerResponse::deserialize(&incomplete_bytes);
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
                public_key: vec![1, 2, 3],
            }),
            "Register",
        ),
        (
            ServerRequest::Connect(ServerConnectRequest {
                request_id: 2,
                public_key: vec![4, 5, 6],
                connection_id: 10,
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
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

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
                public_key: vec![7, 8, 9],
                socket_addr,
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
                public_key: vec![10, 11, 12],
                socket_addr,
                connection_id: 20,
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

/// Test maximum request_id values
#[test]
fn test_max_request_id_values() {
    // Test with u32::MAX
    let request_id = u32::MAX;
    let public_key = vec![1, 2, 3];

    let register_request = ServerRegisterRequest {
        request_id,
        public_key: public_key.clone(),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, u32::MAX);
        }
        _ => panic!("Expected Register request"),
    }

    // Also test with 0
    let register_request_zero = ServerRegisterRequest {
        request_id: 0,
        public_key,
    };

    let request_zero = ServerRequest::Register(register_request_zero);
    let serialized_zero = request_zero.serialize();
    let deserialized_zero = ServerRequest::deserialize(&serialized_zero).unwrap();

    match deserialized_zero {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, 0);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test maximum error code values
#[test]
fn test_max_error_code_values() {
    let error_response = ServerErrorResponse {
        request_id: 1,
        code: u16::MAX,
    };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.code, u16::MAX);
        }
        _ => panic!("Expected RegisterError response"),
    }
}

/// Test public key patterns in messages
#[test]
fn test_public_key_patterns_in_messages() {
    // Test with all zeros
    let public_key_zeros = vec![0u8; 33];
    let request1 = ServerRequest::Register(ServerRegisterRequest {
        request_id: 1,
        public_key: public_key_zeros.clone(),
    });
    let serialized1 = request1.serialize();
    let deserialized1 = ServerRequest::deserialize(&serialized1).unwrap();
    match deserialized1 {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key, public_key_zeros);
        }
        _ => panic!("Expected Register request"),
    }

    // Test with all 0xFF
    let public_key_max = vec![0xFFu8; 33];
    let request2 = ServerRequest::Register(ServerRegisterRequest {
        request_id: 2,
        public_key: public_key_max.clone(),
    });
    let serialized2 = request2.serialize();
    let deserialized2 = ServerRequest::deserialize(&serialized2).unwrap();
    match deserialized2 {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key, public_key_max);
        }
        _ => panic!("Expected Register request"),
    }

    // Test with alternating pattern
    let public_key_alt: Vec<u8> = (0..33)
        .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
        .collect();
    let request3 = ServerRequest::Register(ServerRegisterRequest {
        request_id: 3,
        public_key: public_key_alt.clone(),
    });
    let serialized3 = request3.serialize();
    let deserialized3 = ServerRequest::deserialize(&serialized3).unwrap();
    match deserialized3 {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key, public_key_alt);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test empty public key
#[test]
fn test_empty_public_key() {
    let public_key = vec![];

    let register_request = ServerRegisterRequest {
        request_id: 1,
        public_key: public_key.clone(),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key, public_key);
            assert!(r.public_key.is_empty());
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test various port numbers
#[test]
fn test_various_port_numbers() {
    let ports = vec![0, 1, 80, 443, 8080, 65535];

    for port in ports {
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
        let public_key = vec![1, 2, 3];

        let connect_response = ServerConnectResponse {
            request_id: port as u32,
            public_key: public_key.clone(),
            socket_addr,
        };

        let response = ServerResponse::Connect(connect_response);
        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::Connect(c) => {
                assert_eq!(c.socket_addr.port(), port);
            }
            _ => panic!("Expected Connect response"),
        }
    }
}

/// Test minimum values
#[test]
fn test_minimum_values() {
    // Test with minimum request_id (0)
    let register_request = ServerRegisterRequest {
        request_id: 0,
        public_key: vec![],
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.request_id, 0);
            assert!(r.public_key.is_empty());
        }
        _ => panic!("Expected Register request"),
    }

    // Test with minimum error code (0)
    let error_response = ServerErrorResponse {
        request_id: 0,
        code: 0,
    };

    let response = ServerResponse::RegisterError(error_response);
    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::RegisterError(e) => {
            assert_eq!(e.request_id, 0);
            assert_eq!(e.code, 0);
        }
        _ => panic!("Expected RegisterError response"),
    }
}

/// Test serialization consistency
#[test]
fn test_serialization_consistency() {
    let public_key = vec![1, 2, 3, 4, 5];
    let request = ServerRequest::Register(ServerRegisterRequest {
        request_id: 12345,
        public_key: public_key.clone(),
    });

    // Serialize multiple times and verify consistency
    let serialized1 = request.serialize();
    let serialized2 = request.serialize();
    let serialized3 = request.serialize();

    assert_eq!(serialized1, serialized2);
    assert_eq!(serialized2, serialized3);
}

/// Test mixed IP versions
#[test]
fn test_mixed_ip_versions() {
    // IPv4
    let addr_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 8080);
    let public_key_v4 = vec![1, 2, 3];
    let response_v4 = ServerResponse::Connect(ServerConnectResponse {
        request_id: 1,
        public_key: public_key_v4.clone(),
        socket_addr: addr_v4,
    });

    let serialized_v4 = response_v4.serialize();
    let deserialized_v4 = ServerResponse::deserialize(&serialized_v4).unwrap();

    match deserialized_v4 {
        ServerResponse::Connect(c) => {
            assert!(c.socket_addr.ip().is_ipv4());
            assert_eq!(c.public_key, public_key_v4);
        }
        _ => panic!("Expected Connect response"),
    }

    // IPv6
    let addr_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 9090);
    let public_key_v6 = vec![4, 5, 6];
    let response_v6 = ServerResponse::Connect(ServerConnectResponse {
        request_id: 2,
        public_key: public_key_v6.clone(),
        socket_addr: addr_v6,
    });

    let serialized_v6 = response_v6.serialize();
    let deserialized_v6 = ServerResponse::deserialize(&serialized_v6).unwrap();

    match deserialized_v6 {
        ServerResponse::Connect(c) => {
            assert!(c.socket_addr.ip().is_ipv6());
            assert_eq!(c.public_key, public_key_v6);
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test maximum public key size
#[test]
fn test_maximum_public_key_size() {
    // Test with a large public key (max u16 length)
    let large_public_key = vec![0xABu8; u16::MAX as usize];

    let register_request = ServerRegisterRequest {
        request_id: 1,
        public_key: large_public_key.clone(),
    };

    let request = ServerRequest::Register(register_request);
    let serialized = request.serialize();
    let deserialized = ServerRequest::deserialize(&serialized).unwrap();

    match deserialized {
        ServerRequest::Register(r) => {
            assert_eq!(r.public_key.len(), u16::MAX as usize);
            assert_eq!(r.public_key, large_public_key);
        }
        _ => panic!("Expected Register request"),
    }
}

/// Test error code range
#[test]
fn test_error_code_range() {
    let error_codes = vec![0, 1, 100, 404, 500, 1000, 10000, 65535];

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

/// Test localhost addresses
#[test]
fn test_localhost_addresses() {
    // IPv4 localhost
    let localhost_v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);
    let public_key = vec![1, 2, 3];
    let response_v4 = ServerResponse::Connect(ServerConnectResponse {
        request_id: 1,
        public_key: public_key.clone(),
        socket_addr: localhost_v4,
    });

    let serialized = response_v4.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.socket_addr, localhost_v4);
            assert!(c.socket_addr.ip().is_loopback());
        }
        _ => panic!("Expected Connect response"),
    }

    // IPv6 localhost
    let localhost_v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 3001);
    let response_v6 = ServerResponse::Connect(ServerConnectResponse {
        request_id: 2,
        public_key: public_key.clone(),
        socket_addr: localhost_v6,
    });

    let serialized = response_v6.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.socket_addr, localhost_v6);
            assert!(c.socket_addr.ip().is_loopback());
        }
        _ => panic!("Expected Connect response"),
    }
}

/// Test special IPv6 addresses
#[test]
fn test_special_ipv6_addresses() {
    let special_addrs = vec![
        Ipv6Addr::UNSPECIFIED,
        Ipv6Addr::LOCALHOST,
        Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1), // Link-local
        Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), // Documentation
    ];

    for ipv6 in special_addrs {
        let socket_addr = SocketAddr::new(IpAddr::V6(ipv6), 8080);
        let public_key = vec![1, 2, 3];

        let response = ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
            public_key: public_key.clone(),
            socket_addr,
            connection_id: 1,
        });

        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::IncomingConnection(i) => {
                assert_eq!(i.socket_addr.ip(), IpAddr::V6(ipv6));
            }
            _ => panic!("Expected IncomingConnection response"),
        }
    }
}

/// Test sequential request IDs
#[test]
fn test_sequential_request_ids() {
    for request_id in 0..100u32 {
        let public_key = vec![request_id as u8];
        let register_request = ServerRegisterRequest {
            request_id,
            public_key: public_key.clone(),
        };

        let request = ServerRequest::Register(register_request);
        let serialized = request.serialize();
        let deserialized = ServerRequest::deserialize(&serialized).unwrap();

        match deserialized {
            ServerRequest::Register(r) => {
                assert_eq!(r.request_id, request_id);
                assert_eq!(r.public_key, vec![request_id as u8]);
            }
            _ => panic!("Expected Register request"),
        }
    }
}

/// Test boundary port numbers
#[test]
fn test_boundary_port_numbers() {
    let boundary_ports = vec![0, 1, 1023, 1024, 49151, 49152, 65534, 65535];

    for port in boundary_ports {
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port);
        let public_key = vec![1, 2, 3];

        let response = ServerResponse::IncomingConnection(ServerIncomingConnectionResponse {
            public_key: public_key.clone(),
            socket_addr,
            connection_id: port as u32,
        });

        let serialized = response.serialize();
        let deserialized = ServerResponse::deserialize(&serialized).unwrap();

        match deserialized {
            ServerResponse::IncomingConnection(i) => {
                assert_eq!(i.socket_addr.port(), port);
                assert_eq!(i.connection_id, port as u32);
            }
            _ => panic!("Expected IncomingConnection response"),
        }
    }
}

/// Test corrupted length field
#[test]
fn test_corrupted_length_field() {
    // Create a valid serialized message
    let public_key = vec![1, 2, 3];
    let request = ServerRequest::Register(ServerRegisterRequest {
        request_id: 1,
        public_key,
    });
    let mut serialized = request.serialize();

    // Corrupt the length by truncating
    serialized.truncate(serialized.len() / 2);
    let result = ServerRequest::deserialize(&serialized);
    assert!(result.is_err());
}

/// Test all maximum values
#[test]
fn test_all_maximum_values() {
    let public_key = vec![0xFFu8; 33];
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)), 65535);

    let response = ServerResponse::Connect(ServerConnectResponse {
        request_id: u32::MAX,
        public_key: public_key.clone(),
        socket_addr,
    });

    let serialized = response.serialize();
    let deserialized = ServerResponse::deserialize(&serialized).unwrap();

    match deserialized {
        ServerResponse::Connect(c) => {
            assert_eq!(c.request_id, u32::MAX);
            assert_eq!(c.public_key, public_key);
            assert_eq!(c.socket_addr, socket_addr);
        }
        _ => panic!("Expected Connect response"),
    }
}
