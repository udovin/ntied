use ntied_crypto::EphemeralKeyPair;
use ntied_transport::{
    Address, DataPacket, DecryptedPacket, EncryptedPacket, EncryptionEpoch, HandshakeAckPacket,
    HandshakePacket, HeartbeatPacket, Packet, RotatePacket,
};

/// Test serialization and deserialization of Handshake message
#[test]
fn test_handshake_message_serialization() {
    let source_id = 5;
    let public_key = vec![1, 2, 3, 4, 5];
    let address = Address::from_bytes([0u8; 33]);
    let peer_address = Address::from_bytes([1u8; 33]);
    let ephemeral_public_key = vec![6, 7, 8, 9, 10];
    let signature = vec![11, 12, 13, 14, 15];

    let handshake = HandshakePacket {
        source_id,
        public_key: public_key.clone(),
        address,
        peer_address,
        ephemeral_public_key: ephemeral_public_key.clone(),
        signature: signature.clone(),
    };

    let message = Packet::Handshake(handshake);
    let serialized = message.serialize();
    let deserialized = Packet::deserialize(&serialized).unwrap();

    match deserialized {
        Packet::Handshake(h) => {
            assert_eq!(h.source_id, source_id);
            assert_eq!(h.public_key, public_key);
            assert_eq!(h.address, address);
            assert_eq!(h.peer_address, peer_address);
            assert_eq!(h.ephemeral_public_key, ephemeral_public_key);
            assert_eq!(h.signature, signature);
        }
        _ => panic!("Expected Handshake message"),
    }
}

/// Test serialization and deserialization of HandshakeAck message
#[test]
fn test_handshake_ack_message_serialization() {
    let target_id = 9;
    let source_id = 10;
    let public_key = vec![16, 17, 18, 19, 20];
    let address = Address::from_bytes([2u8; 33]);
    let peer_address = Address::from_bytes([3u8; 33]);
    let ephemeral_public_key = vec![21, 22, 23, 24, 25];
    let signature = vec![26, 27, 28, 29, 30];

    let handshake_ack = HandshakeAckPacket {
        target_id,
        source_id,
        public_key: public_key.clone(),
        address,
        peer_address,
        ephemeral_public_key: ephemeral_public_key.clone(),
        signature: signature.clone(),
    };

    let message = Packet::HandshakeAck(handshake_ack);
    let serialized = message.serialize();
    let deserialized = Packet::deserialize(&serialized).unwrap();

    match deserialized {
        Packet::HandshakeAck(h) => {
            assert_eq!(h.target_id, target_id);
            assert_eq!(h.source_id, source_id);
            assert_eq!(h.public_key, public_key);
            assert_eq!(h.address, address);
            assert_eq!(h.peer_address, peer_address);
            assert_eq!(h.ephemeral_public_key, ephemeral_public_key);
            assert_eq!(h.signature, signature);
        }
        _ => panic!("Expected HandshakeAck message"),
    }
}

/// Test serialization and deserialization of Encrypted message with minimum epoch
#[test]
fn test_encrypted_message_min_epoch() {
    let target_id = 30;
    let epoch = EncryptionEpoch::new(0u8);
    let payload = vec![31, 32, 33, 34, 35];
    let nonce = [1u8; 12];

    let encrypted = EncryptedPacket {
        target_id,
        epoch,
        payload: payload.clone(),
        nonce,
    };

    let message = Packet::Encrypted(encrypted);
    let serialized = message.serialize();
    let deserialized = Packet::deserialize(&serialized).unwrap();

    match deserialized {
        Packet::Encrypted(e) => {
            assert_eq!(e.target_id, target_id);
            assert_eq!(e.epoch, epoch);
            assert_eq!(e.payload, payload);
            assert_eq!(e.nonce, nonce);
        }
        _ => panic!("Expected Encrypted message"),
    }
}

/// Test serialization and deserialization of Encrypted message with maximum epoch
#[test]
fn test_encrypted_message_max_epoch() {
    let target_id = 35;
    let epoch = EncryptionEpoch::new(255 - 128);
    let payload = vec![36, 37, 38, 39, 40];
    let nonce = [2u8; 12];

    let encrypted = EncryptedPacket {
        target_id,
        epoch,
        payload: payload.clone(),
        nonce,
    };

    let message = Packet::Encrypted(encrypted);
    let serialized = message.serialize();
    let deserialized = Packet::deserialize(&serialized).unwrap();

    match deserialized {
        Packet::Encrypted(e) => {
            assert_eq!(e.target_id, target_id);
            assert_eq!(e.epoch, epoch);
            assert_eq!(e.payload, payload);
            assert_eq!(e.nonce, nonce);
        }
        _ => panic!("Expected Encrypted message"),
    }
}

/// Test that epoch exceeding maximum causes panic
#[test]
#[should_panic(expected = "Epoch value exceeds maximum allowed")]
fn test_encrypted_message_epoch_overflow() {
    let target_id = 40;
    let epoch = EncryptionEpoch::new(255 - 127);
    let payload = vec![41, 42, 43];
    let nonce = [3u8; 12];

    let encrypted = EncryptedPacket {
        target_id,
        epoch,
        payload,
        nonce,
    };

    let message = Packet::Encrypted(encrypted);
    message.serialize(); // Should panic here
}

/// Test serialization and deserialization of Rotate decrypted message
#[test]
fn test_rotate_message_serialization() {
    let ephemeral_public_key = vec![44, 45, 46, 47, 48];
    let signature = vec![49, 50, 51, 52, 53];

    let rotate = RotatePacket {
        ephemeral_public_key: ephemeral_public_key.clone(),
        signature: signature.clone(),
    };

    let message = DecryptedPacket::Rotate(rotate);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::Rotate(r) => {
            assert_eq!(r.ephemeral_public_key, ephemeral_public_key);
            assert_eq!(r.signature, signature);
        }
        _ => panic!("Expected Rotate message"),
    }
}

/// Test serialization and deserialization of RotateAck decrypted message
#[test]
fn test_rotate_ack_message_serialization() {
    let ephemeral_public_key = vec![54, 55, 56, 57, 58];
    let signature = vec![59, 60, 61, 62, 63];

    let rotate_ack = RotatePacket {
        ephemeral_public_key: ephemeral_public_key.clone(),
        signature: signature.clone(),
    };

    let message = DecryptedPacket::RotateAck(rotate_ack);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::RotateAck(r) => {
            assert_eq!(r.ephemeral_public_key, ephemeral_public_key);
            assert_eq!(r.signature, signature);
        }
        _ => panic!("Expected RotateAck message"),
    }
}

/// Test serialization and deserialization of Heartbeat decrypted message
#[test]
fn test_heartbeat_message_serialization() {
    let heartbeat = HeartbeatPacket {};
    let message = DecryptedPacket::Heartbeat(heartbeat);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::Heartbeat(_) => {
            // Heartbeat has no fields to check
        }
        _ => panic!("Expected Heartbeat message"),
    }
}

/// Test serialization and deserialization of HeartbeatAck decrypted message
#[test]
fn test_heartbeat_ack_message_serialization() {
    let heartbeat_ack = HeartbeatPacket {};
    let message = DecryptedPacket::HeartbeatAck(heartbeat_ack);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::HeartbeatAck(_) => {
            // HeartbeatAck has no fields to check
        }
        _ => panic!("Expected HeartbeatAck message"),
    }
}

/// Test serialization and deserialization of Data decrypted message
#[test]
fn test_data_message_serialization() {
    let data = vec![64, 65, 66, 67, 68, 69, 70];
    let data_message = DataPacket { data: data.clone() };

    let message = DecryptedPacket::Data(data_message);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::Data(d) => {
            assert_eq!(d.data, data);
        }
        _ => panic!("Expected Data message"),
    }
}

/// Test deserialization with empty data
#[test]
fn test_empty_data_deserialization() {
    let empty_data = vec![];
    let result = Packet::deserialize(&empty_data);
    assert!(result.is_err());
}

/// Test deserialization with invalid message type for DecryptedMessage
#[test]
fn test_invalid_decrypted_message_type() {
    let invalid_data = vec![99]; // Invalid message type
    let result = DecryptedPacket::deserialize(&invalid_data);
    assert!(result.is_err());
}

/// Test full encryption and decryption flow
#[test]
fn test_encrypted_message_encrypt_decrypt() {
    // Generate ephemeral key pairs for both parties
    let ephemeral1 = EphemeralKeyPair::generate();
    let ephemeral2 = EphemeralKeyPair::generate();

    // Exchange public keys and compute shared secrets
    let shared_secret = ephemeral1
        .compute_shared_secret(&ephemeral2.public_key_bytes())
        .unwrap();
    let shared_secret2 = ephemeral2
        .compute_shared_secret(&ephemeral1.public_key_bytes())
        .unwrap();

    // Create a data message
    let stream = 6;
    let data = vec![100, 101, 102, 103, 104];
    let data_message = DataPacket { data: data.clone() };
    let decrypted = DecryptedPacket::Data(data_message);

    // Encrypt the message
    let epoch = EncryptionEpoch::new(5u8);
    let nonce = [4u8; 12];
    let encrypted =
        EncryptedPacket::encrypt(stream, decrypted, epoch, &shared_secret, nonce).unwrap();

    // Decrypt the message
    let decrypted_result = encrypted.decrypt(&shared_secret2).unwrap();

    match decrypted_result {
        DecryptedPacket::Data(d) => {
            assert_eq!(d.data, data);
        }
        _ => panic!("Expected Data message after decryption"),
    }
}

/// Test Address serialization in message context
#[test]
fn test_address_in_message() {
    let mut address_bytes = [0u8; 33];
    for i in 0..33 {
        address_bytes[i] = (i * 3) as u8;
    }
    let address = Address::from_bytes(address_bytes);

    let mut peer_address_bytes = [0u8; 33];
    for i in 0..33 {
        peer_address_bytes[i] = (i * 5) as u8;
    }
    let peer_address = Address::from_bytes(peer_address_bytes);

    let handshake = HandshakePacket {
        source_id: 80,
        public_key: vec![71, 72, 73],
        address,
        peer_address,
        ephemeral_public_key: vec![74, 75, 76],
        signature: vec![77, 78, 79],
    };

    let message = Packet::Handshake(handshake);
    let serialized = message.serialize();
    let deserialized = Packet::deserialize(&serialized).unwrap();

    match deserialized {
        Packet::Handshake(h) => {
            assert_eq!(h.address, address);
            assert_eq!(h.peer_address, peer_address);
            assert_eq!(h.address.as_bytes(), &address_bytes);
            assert_eq!(h.peer_address.as_bytes(), &peer_address_bytes);
        }
        _ => panic!("Expected Handshake message"),
    }
}

/// Test large payload serialization
#[test]
fn test_large_payload_serialization() {
    let large_data = vec![42u8; 10000]; // 10KB of data
    let data_message = DataPacket {
        data: large_data.clone(),
    };

    let message = DecryptedPacket::Data(data_message);
    let serialized = message.serialize();
    let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

    match deserialized {
        DecryptedPacket::Data(d) => {
            assert_eq!(d.data, large_data);
            assert_eq!(d.data.len(), 10000);
        }
        _ => panic!("Expected Data message"),
    }
}

/// Test message type discrimination
#[test]
fn test_message_type_discrimination() {
    // Create different message types and verify they deserialize to correct type
    let messages = vec![
        (
            Packet::Handshake(HandshakePacket {
                source_id: 42,
                public_key: vec![80],
                address: Address::from_bytes([4u8; 33]),
                peer_address: Address::from_bytes([5u8; 33]),
                ephemeral_public_key: vec![81],
                signature: vec![82],
            }),
            "Handshake",
        ),
        (
            Packet::HandshakeAck(HandshakeAckPacket {
                target_id: 42,
                source_id: 43,
                public_key: vec![83],
                address: Address::from_bytes([6u8; 33]),
                peer_address: Address::from_bytes([7u8; 33]),
                ephemeral_public_key: vec![84],
                signature: vec![85],
            }),
            "HandshakeAck",
        ),
        (
            Packet::Encrypted(EncryptedPacket {
                target_id: 44,
                epoch: EncryptionEpoch::new(10),
                payload: vec![86, 87],
                nonce: [5u8; 12],
            }),
            "Encrypted",
        ),
    ];

    for (message, expected_type) in messages {
        let serialized = message.serialize();
        let deserialized = Packet::deserialize(&serialized).unwrap();

        let actual_type = match deserialized {
            Packet::Handshake(_) => "Handshake",
            Packet::HandshakeAck(_) => "HandshakeAck",
            Packet::Encrypted(_) => "Encrypted",
        };

        assert_eq!(actual_type, expected_type);
    }
}

/// Test decrypted message type discrimination
#[test]
fn test_decrypted_message_type_discrimination() {
    let messages = vec![
        (
            DecryptedPacket::Rotate(RotatePacket {
                ephemeral_public_key: vec![88],
                signature: vec![89],
            }),
            "Rotate",
        ),
        (
            DecryptedPacket::RotateAck(RotatePacket {
                ephemeral_public_key: vec![90],
                signature: vec![91],
            }),
            "RotateAck",
        ),
        (DecryptedPacket::Heartbeat(HeartbeatPacket {}), "Heartbeat"),
        (
            DecryptedPacket::HeartbeatAck(HeartbeatPacket {}),
            "HeartbeatAck",
        ),
        (
            DecryptedPacket::Data(DataPacket {
                data: vec![92, 93, 94],
            }),
            "Data",
        ),
    ];

    for (message, expected_type) in messages {
        let serialized = message.serialize();
        let deserialized = DecryptedPacket::deserialize(&serialized).unwrap();

        let actual_type = match deserialized {
            DecryptedPacket::Rotate(_) => "Rotate",
            DecryptedPacket::RotateAck(_) => "RotateAck",
            DecryptedPacket::Heartbeat(_) => "Heartbeat",
            DecryptedPacket::HeartbeatAck(_) => "HeartbeatAck",
            DecryptedPacket::Data(_) => "Data",
        };

        assert_eq!(actual_type, expected_type);
    }
}
