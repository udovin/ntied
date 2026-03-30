use super::*;
use crate::crypto::{EncryptionKeys, EphemeralPrivateKey, PrivateKey, compute_transcript_hash};

fn make_key_pair() -> (EncryptionKeys, EncryptionKeys, [u8; 32]) {
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

    assert!(resp.decrypt(2, 0, b"", &ct).is_none());
}

#[test]
fn crypto_state_decrypt_next_when_prev_exists() {
    let (_, keys_b1, _) = make_key_pair();
    let (_, keys_b2, _) = make_key_pair();
    let (keys_a3, keys_b3, _) = make_key_pair();

    let mut resp = CryptoState::new(Role::Responder, 1, keys_b1);

    resp.install_keys(2, keys_b2);
    resp.prepare_next_keys(3, keys_b3);

    let init = CryptoState::new(Role::Initiator, 3, keys_a3);
    let ciphertext = init.encrypt(0, b"", b"hello epoch 3");

    let decrypted = resp.decrypt(3, 0, b"", &ciphertext);
    assert!(decrypted.is_some());
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
        receiver_connection_id: 12345,
        payload: b"hello session".to_vec(),
    };

    let encrypted = init.encrypt(data);

    assert_eq!(encrypted.receiver_connection_id, 12345);
    assert_eq!(encrypted.counter, 0);
    assert_eq!(encrypted.epoch, 1);

    let decrypted = resp.decrypt(encrypted).expect("decrypt failed");
    assert_eq!(decrypted.receiver_connection_id, 12345);
    assert_eq!(decrypted.payload, b"hello session");
}

#[test]
fn on_auth_data_success() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Responder, 1, keys, th);

    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let sig = private_key.sign(&th);

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());

    let result = session.on_auth_data(&payload);
    assert_eq!(result, Some(SessionEvent::AuthCompleted(public_key)));
    assert_eq!(session.state(), SessionState::Established);
}

#[test]
fn on_auth_data_bad_signature() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Responder, 1, keys, th);

    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let sig = private_key.sign(b"wrong message");

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key.to_bytes());
    payload.extend_from_slice(&sig.to_bytes());

    assert!(session.on_auth_data(&payload).is_none());
    assert_eq!(session.state(), SessionState::Handshake);
}

#[test]
fn on_auth_data_too_short() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Responder, 1, keys, th);
    assert!(session.on_auth_data(&[0u8; 100]).is_none());
}

#[test]
fn rekey_flow_via_session() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let result_resp = resp.on_rekey_data(&epk_bytes);
    let ct_bytes = match result_resp {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    assert_eq!(resp.current_epoch(), 1);
    assert_eq!(resp.state(), SessionState::Established);

    let result_init = init.on_rekey_ack_data(&ct_bytes);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));
    assert_eq!(init.current_epoch(), 2);
    assert_eq!(init.state(), SessionState::Established);

    // Initiator sends a data packet in epoch 2
    let data = DecryptedData {
        receiver_connection_id: 1,
        payload: b"rekey test".to_vec(),
    };
    let encrypted = init.encrypt(data);
    assert_eq!(encrypted.epoch, 2);

    // Responder receives it and rotates keys
    let decrypted = resp.decrypt(encrypted).expect("decrypt failed after rekey");
    assert_eq!(decrypted.payload, b"rekey test");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn rekey_chicken_and_egg() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let result_resp = resp.on_rekey_data(&epk_bytes);
    assert!(matches!(result_resp, Some(SessionEvent::SendRekeyAck(_))));

    // Responder encrypts RekeyAck - must use old epoch
    let resp_data = DecryptedData {
        receiver_connection_id: 1,
        payload: b"mock RekeyAck payload".to_vec(),
    };
    let resp_packet = resp.encrypt(resp_data);
    assert_eq!(resp_packet.epoch, 1);

    let decrypted = init.decrypt(resp_packet).expect("Initiator failed to decrypt");
    assert_eq!(decrypted.payload, b"mock RekeyAck payload");
}

#[test]
fn rekey_duplicate() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let ct1 = match resp.on_rekey_data(&epk_bytes) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };

    let ct2 = match resp.on_rekey_data(&epk_bytes) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };

    assert_eq!(ct1, ct2, "Duplicate Rekey should return identical RekeyAck");

    let result_init = init.on_rekey_ack_data(&ct1);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));

    let data = DecryptedData {
        receiver_connection_id: 1,
        payload: b"rekey test".to_vec(),
    };
    let encrypted = init.encrypt(data);
    let decrypted = resp.decrypt(encrypted).expect("decrypt failed");
    assert_eq!(decrypted.payload, b"rekey test");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn rekey_simultaneous() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_init = init.start_rekey().unwrap();
    let epk_resp = resp.start_rekey().unwrap();

    // Responder receives Initiator's Rekey - loses tie-breaker
    let ct_bytes = match resp.on_rekey_data(&epk_init) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck from Responder"),
    };

    // Initiator receives Responder's Rekey - wins tie-breaker, ignores
    let result_init_ignore = init.on_rekey_data(&epk_resp);
    assert_eq!(result_init_ignore, None);

    // Initiator processes RekeyAck
    let result_init = init.on_rekey_ack_data(&ct_bytes);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));

    let data = DecryptedData {
        receiver_connection_id: 1,
        payload: b"simultaneous rekey win".to_vec(),
    };
    let encrypted = init.encrypt(data);
    assert_eq!(encrypted.epoch, 2);

    let decrypted = resp.decrypt(encrypted).expect("Responder failed to decrypt");
    assert_eq!(decrypted.payload, b"simultaneous rekey win");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn session_edge_cases() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);

    session.drop_previous_keys();

    // RekeyState error branches
    let mut rekey = RekeyState::new();
    assert!(rekey.handle_rekey(2, b"too short", true).is_none());
    assert!(rekey.handle_rekey_ack(2, b"too short").is_none());
    assert!(!rekey.handle_switch(99));
    rekey.start_rekey(2);
    assert!(rekey.start_rekey(3).is_none());
    assert!(!rekey.handle_switch(99));
}
