use super::*;
use crate::v2::crypto::{EncryptionKeys, EphemeralPrivateKey, PrivateKey};
use crate::v2::wire::{Auth, Frame, Rekey, RekeyAck};

fn make_key_pair() -> (EncryptionKeys, EncryptionKeys, [u8; 32]) {
    use crate::v2::crypto::compute_transcript_hash;
    let initiator = EphemeralPrivateKey::generate();
    let responder = EphemeralPrivateKey::generate();
    let initiator_pk = initiator.public_key();
    let (ct, responder_ss) = responder.encapsulate(&initiator_pk).unwrap();
    let initiator_ss = initiator.decapsulate(&ct).unwrap();
    let keys_a = EncryptionKeys::new(&initiator_ss, &initiator_pk, &ct);
    let keys_b = EncryptionKeys::new(&responder_ss, &initiator_pk, &ct);
    let th = compute_transcript_hash(&initiator_pk, &ct);
    (keys_a, keys_b, th)
}

#[test]
fn fragment_collector_basic() {
    let mut collector = FragmentCollector::new();

    let res = collector.add_fragment(0, 3, b"hello ");
    assert!(res.is_none());

    let res = collector.add_fragment(1, 3, b"world");
    assert!(res.is_none());

    let res = collector.add_fragment(2, 3, b"!");
    assert_eq!(res.unwrap(), b"hello world!");
}

#[test]
fn fragment_collector_out_of_order() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(2, 3, b"!");
    collector.add_fragment(0, 3, b"hello ");
    let res = collector.add_fragment(1, 3, b"world");

    assert_eq!(res.unwrap(), b"hello world!");
}

#[test]
fn fragment_collector_duplicate_fragment() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(0, 2, b"a");
    collector.add_fragment(0, 2, b"a");
    let res = collector.add_fragment(1, 2, b"b");

    assert_eq!(res.unwrap(), b"ab");
}

#[test]
fn fragment_collector_total_changed() {
    let mut collector = FragmentCollector::new();

    collector.add_fragment(0, 3, b"a");
    // Network drops some packets, new message starts with different total
    let res = collector.add_fragment(0, 2, b"hello ");
    assert!(res.is_none());
    let res = collector.add_fragment(1, 2, b"world");

    assert_eq!(res.unwrap(), b"hello world");
}

#[test]
fn fragment_collector_invalid_index() {
    let mut collector = FragmentCollector::new();
    let res = collector.add_fragment(2, 2, b"out of bounds");
    assert!(res.is_none());
}

#[test]
fn crypto_state_send_counter() {
    let (keys, _, _) = make_key_pair();
    let mut state = CryptoState::new(Role::Initiator, 1, keys);

    assert_eq!(state.next_send_counter(), 0);
    assert_eq!(state.next_send_counter(), 1);
    assert_eq!(state.next_send_counter(), 2);
}

#[test]
fn crypto_state_encrypt_decrypt() {
    let (keys_a, keys_b, _) = make_key_pair();
    let mut init = CryptoState::new(Role::Initiator, 1, keys_a);
    let mut resp = CryptoState::new(Role::Responder, 1, keys_b);

    let counter = init.next_send_counter();
    let aad = b"header";
    let plaintext = b"hello";

    let ciphertext = init.encrypt(counter, aad, plaintext);
    let decrypted = resp
        .decrypt(1, counter, aad, &ciphertext)
        .expect("decrypt failed");

    assert_eq!(decrypted, plaintext);

    let counter2 = resp.next_send_counter();
    let ciphertext2 = resp.encrypt(counter2, aad, b"world");
    let decrypted2 = init
        .decrypt(1, counter2, aad, &ciphertext2)
        .expect("decrypt failed");

    assert_eq!(decrypted2, b"world");
}

#[test]
fn crypto_state_rekey_grace_period() {
    let (keys_a1, keys_b1, _) = make_key_pair();
    let (keys_a2, keys_b2, _) = make_key_pair();

    let mut init = CryptoState::new(Role::Initiator, 1, keys_a1);
    let mut resp = CryptoState::new(Role::Responder, 1, keys_b1);

    let counter1 = init.next_send_counter();
    let ct_epoch1 = init.encrypt(counter1, b"", b"msg1");
    assert!(resp.decrypt(1, counter1, b"", &ct_epoch1).is_some());

    init.install_keys(2, keys_a2);
    let counter2 = init.next_send_counter();
    let ct_epoch2 = init.encrypt(counter2, b"", b"msg2");

    resp.install_keys(2, keys_b2);
    assert!(resp.decrypt(2, counter2, b"", &ct_epoch2).is_some());

    // Delayed packet from epoch 1
    assert!(resp.decrypt(1, counter1, b"", &ct_epoch1).is_some());

    resp.drop_previous_keys();
    assert!(resp.decrypt(1, counter1, b"", &ct_epoch1).is_none());
}

#[test]
fn crypto_state_getters() {
    let (keys, _, _) = make_key_pair();
    let state = CryptoState::new(Role::Initiator, 5, keys);

    assert_eq!(state.role(), Role::Initiator);
    assert_eq!(state.current_epoch(), 5);
}

#[test]
fn crypto_state_decrypt_wrong_epoch() {
    let (keys_a, keys_b, _) = make_key_pair();
    let init = CryptoState::new(Role::Initiator, 1, keys_a);
    let resp = CryptoState::new(Role::Responder, 1, keys_b);

    let ct = init.encrypt(0, b"", b"hello");

    // Attempt to decrypt with incorrect epoch 2
    assert!(resp.decrypt(2, 0, b"", &ct).is_none());
}

#[test]
fn crypto_state_decrypt_next_when_prev_exists() {
    let (_, keys_b1, _) = make_key_pair();
    let (_, keys_b2, _) = make_key_pair();
    let (keys_a3, keys_b3, _) = make_key_pair();

    let mut resp = CryptoState::new(Role::Responder, 1, keys_b1);

    // Switch to epoch 2. Now current=2, prev=1
    resp.install_keys(2, keys_b2);

    // Prepare next epoch 3
    resp.prepare_next_keys(3, keys_b3);

    // Mock initiator sending data encrypted with epoch 3
    let init = CryptoState::new(Role::Initiator, 3, keys_a3);
    let ciphertext = init.encrypt(0, b"", b"hello epoch 3");

    let decrypted = resp.decrypt(3, 0, b"", &ciphertext);
    assert!(
        decrypted.is_some(),
        "Failed to decrypt next epoch when previous epoch exists"
    );
    assert_eq!(decrypted.unwrap(), b"hello epoch 3");
}

#[test]
fn session_initial_state() {
    let (keys, _, th) = make_key_pair();
    let session = Session::new(Role::Initiator, 1, keys, th);
    assert_eq!(session.state(), SessionState::Handshake);
}

#[test]
fn session_set_state() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);
    session.set_state(SessionState::Established);
    assert_eq!(session.state(), SessionState::Established);
}

#[test]
fn session_encrypt_decrypt() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let data = DecryptedData {
        receiver_session_id: 12345,
        payload: b"hello session".to_vec(),
    };

    let encrypted = init.encrypt(data);

    assert_eq!(encrypted.receiver_session_id, 12345);
    assert_eq!(encrypted.counter, 0);
    assert_eq!(encrypted.epoch, 1);

    let decrypted = resp.decrypt(encrypted).expect("decrypt failed");
    assert_eq!(decrypted.receiver_session_id, 12345);
    assert_eq!(decrypted.payload, b"hello session");
}

#[test]
fn auth_state_verify_success() {
    let mut auth = AuthState::new();
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let message = b"handshake transcript";

    let sig = private_key.sign(message);

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());

    assert!(auth.process_fragment(0, 2, &payload[0..1000]).is_none());
    let assembled = auth.process_fragment(1, 2, &payload[1000..]).unwrap();
    assert_eq!(assembled, payload);

    let verified_pk = auth.verify_payload(&assembled, message).unwrap();
    assert_eq!(verified_pk, public_key);
}

#[test]
fn auth_state_verify_fails() {
    let auth = AuthState::new();
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let message = b"handshake transcript";

    let sig = private_key.sign(b"wrong message");

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());

    assert!(auth.verify_payload(&payload, message).is_none());

    let too_short = vec![0u8; 100];
    assert!(auth.verify_payload(&too_short, message).is_none());
}

#[test]
fn rekey_state_flow() {
    let mut initiator_rekey = RekeyState::new();
    let mut responder_rekey = RekeyState::new();

    let epk = initiator_rekey.start_rekey(2).unwrap();
    let epk_bytes = epk.to_bytes();

    assert!(
        responder_rekey
            .process_fragment(0, 2, &epk_bytes[0..600])
            .is_none()
    );
    let assembled_epk = responder_rekey
        .process_fragment(1, 2, &epk_bytes[600..])
        .unwrap();

    let (ct_bytes, resp_keys_opt) = responder_rekey
        .handle_rekey(2, &assembled_epk, false)
        .unwrap();
    let resp_keys = resp_keys_opt.unwrap();

    let assembled_ct = initiator_rekey.process_fragment(0, 1, &ct_bytes).unwrap();
    let init_keys = initiator_rekey.handle_rekey_ack(2, &assembled_ct).unwrap();

    let plaintext = b"test message";
    let ciphertext = init_keys.initiator_key().encrypt(0, b"", plaintext);
    let decrypted = resp_keys
        .initiator_key()
        .decrypt(0, b"", &ciphertext)
        .unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn process_incoming_frame_auth() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Responder, 1, keys, th);

    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let sig = private_key.sign(&th);

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());

    let frame1 = Frame::Auth(Auth {
        fragment_index: 0,
        fragment_total: 2,
        data: payload[0..1000].to_vec(),
    });

    let frame2 = Frame::Auth(Auth {
        fragment_index: 1,
        fragment_total: 2,
        data: payload[1000..].to_vec(),
    });

    assert_eq!(session.process_incoming_frame(&frame1), None);
    assert_eq!(session.state(), SessionState::Handshake);

    let result = session.process_incoming_frame(&frame2);
    assert_eq!(result, Some(SessionEvent::AuthCompleted(public_key)));
    assert_eq!(session.state(), SessionState::Established);
}

#[test]
fn process_incoming_frame_rekey_flow() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let frame_rekey = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_bytes,
    });

    let result_resp = resp.process_incoming_frame(&frame_rekey);
    let ct_bytes = match result_resp {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    assert_eq!(resp.current_epoch(), 1);
    assert_eq!(resp.state(), SessionState::Established);

    let frame_rekey_ack = Frame::RekeyAck(RekeyAck {
        fragment_index: 0,
        fragment_total: 1,
        data: ct_bytes,
    });

    let result_init = init.process_incoming_frame(&frame_rekey_ack);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));
    assert_eq!(init.current_epoch(), 2);
    assert_eq!(init.state(), SessionState::Established);

    // Initiator sends a data packet in epoch 2
    let data = DecryptedData {
        receiver_session_id: 1,
        payload: b"rekey test".to_vec(),
    };
    let encrypted = init.encrypt(data);
    assert_eq!(encrypted.epoch, 2);

    // Responder receives it and rotates keys
    let decrypted = resp.decrypt(encrypted).expect("decrypt failed after rekey");
    assert_eq!(decrypted.payload, b"rekey test");
    assert_eq!(resp.current_epoch(), 2); // Rotated!
}

#[test]
fn process_incoming_frame_rekey_chicken_and_egg() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let frame_rekey = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_bytes,
    });

    // Responder receives Rekey
    let result_resp = resp.process_incoming_frame(&frame_rekey);
    assert!(matches!(result_resp, Some(SessionEvent::SendRekeyAck(_))));

    // Now Responder wants to send RekeyAck.
    // The coordinator will put the RekeyAck frame into a payload and encrypt it.
    // This MUST be encrypted with the old keys (epoch 1), because the Initiator
    // hasn't received the RekeyAck yet and therefore doesn't have the epoch 2 keys!
    let resp_data = DecryptedData {
        receiver_session_id: 1,
        payload: b"mock RekeyAck payload".to_vec(),
    };

    let resp_packet = resp.encrypt(resp_data);

    // Check that the packet is sent in epoch 1
    assert_eq!(
        resp_packet.epoch, 1,
        "BUG: Responder switched to new epoch too early! Initiator will not be able to decrypt."
    );

    // Check that initiator can actually decrypt it
    let decrypted = init
        .decrypt(resp_packet)
        .expect("Initiator failed to decrypt RekeyAck");
    assert_eq!(decrypted.payload, b"mock RekeyAck payload");
}

#[test]
fn process_incoming_frame_rekey_duplicate() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let frame_rekey = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_bytes,
    });

    // Responder receives Rekey for the first time
    let result_resp1 = resp.process_incoming_frame(&frame_rekey);
    let ct_bytes1 = match result_resp1 {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };

    // Initiator misses RekeyAck, resends Rekey
    let result_resp2 = resp.process_incoming_frame(&frame_rekey);
    let ct_bytes2 = match result_resp2 {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };

    assert_eq!(
        ct_bytes1, ct_bytes2,
        "Duplicate Rekey should return identical RekeyAck"
    );

    let frame_rekey_ack = Frame::RekeyAck(RekeyAck {
        fragment_index: 0,
        fragment_total: 1,
        data: ct_bytes1,
    });

    // Initiator processes RekeyAck successfully
    let result_init = init.process_incoming_frame(&frame_rekey_ack);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));

    // Initiator sends data in epoch 2
    let data = DecryptedData {
        receiver_session_id: 1,
        payload: b"rekey test".to_vec(),
    };
    let encrypted = init.encrypt(data);

    // Responder successfully decrypts it and rotates its keys
    let decrypted = resp
        .decrypt(encrypted)
        .expect("decrypt failed after duplicate rekey");
    assert_eq!(decrypted.payload, b"rekey test");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn process_incoming_frame_rekey_simultaneous() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    // Both start rekey
    let epk_init = init.start_rekey().unwrap();
    let epk_resp = resp.start_rekey().unwrap();

    let frame_from_init = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_init,
    });

    let frame_from_resp = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_resp,
    });

    // Responder receives Initiator's Rekey.
    // Responder loses tie-breaker (Role::Responder). So it acts as responder.
    let result_resp = resp.process_incoming_frame(&frame_from_init);
    let ct_bytes = match result_resp {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck from Responder"),
    };

    // Initiator receives Responder's Rekey.
    // Initiator wins tie-breaker (Role::Initiator). So it ignores it.
    let result_init_ignore = init.process_incoming_frame(&frame_from_resp);
    assert_eq!(
        result_init_ignore, None,
        "Initiator should ignore losing Rekey"
    );

    // Responder's RekeyAck arrives at Initiator.
    let frame_rekey_ack = Frame::RekeyAck(RekeyAck {
        fragment_index: 0,
        fragment_total: 1,
        data: ct_bytes,
    });

    let result_init = init.process_incoming_frame(&frame_rekey_ack);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));

    // Send packet from Initiator to Responder to verify
    let data = DecryptedData {
        receiver_session_id: 1,
        payload: b"simultaneous rekey win".to_vec(),
    };
    let encrypted = init.encrypt(data);
    assert_eq!(encrypted.epoch, 2);

    let decrypted = resp
        .decrypt(encrypted)
        .expect("Responder failed to decrypt after simultaneous rekey");
    assert_eq!(decrypted.payload, b"simultaneous rekey win");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn session_coverage_edge_cases() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);

    // Test facade getters
    session.auth_state_mut().reset();
    session.rekey_state_mut().reset();
    session.drop_previous_keys();

    // Test process_incoming_frame with irrelevant frame
    use crate::v2::wire::Ping;
    let ping_frame = Frame::Ping(Ping { ping_id: 123 });
    assert_eq!(session.process_incoming_frame(&ping_frame), None);

    // Test RekeyState error branches
    let mut rekey = RekeyState::new();
    assert!(rekey.handle_rekey(2, b"too short", true).is_none());
    assert!(rekey.handle_rekey_ack(2, b"too short").is_none());
    assert_eq!(rekey.handle_switch(99), false);
    rekey.start_rekey(2);
    assert!(rekey.start_rekey(3).is_none()); // Trying to start different epoch
    assert_eq!(rekey.handle_switch(99), false);
    rekey.reset();
    assert!(rekey.handle_rekey_ack(2, &[0u8; 1120]).is_none()); // No transition state

    // Test AuthState error branches
    let mut auth = AuthState::new();
    assert!(auth.verify_payload(b"too short", b"msg").is_none());
    auth.process_fragment(0, 1, b"fragment");
    auth.reset(); // coverage for reset
}
