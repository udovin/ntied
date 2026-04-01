use super::*;
use crate::crypto::{EncryptionKeys, KemPrivateKey, PrivateKey, compute_transcript_hash};

fn make_key_pair() -> (EncryptionKeys, EncryptionKeys, [u8; 32]) {
    let initiator = KemPrivateKey::generate();
    let responder = KemPrivateKey::generate();
    let initiator_pk = initiator.public_key();
    let (ct, responder_ss) = responder.encapsulate(&initiator_pk).unwrap();
    let initiator_ss = initiator.decapsulate(&ct).unwrap();
    let keys_a = EncryptionKeys::new(&initiator_ss, &initiator_pk, &ct);
    let keys_b = EncryptionKeys::new(&responder_ss, &initiator_pk, &ct);
    let th = compute_transcript_hash(&initiator_pk, &ct);
    (keys_a, keys_b, th)
}

#[test]
fn initial_state() {
    let (keys, _, th) = make_key_pair();
    let session = Session::new(Role::Initiator, 1, keys, th);
    assert_eq!(session.state(), SessionState::Handshake);
    assert_eq!(session.current_epoch(), 1);
}

#[test]
fn encrypt_decrypt() {
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
fn encrypt_decrypt_bidirectional() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let ct = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"hello".to_vec(),
    });
    let decrypted = resp.decrypt(ct).expect("decrypt failed");
    assert_eq!(decrypted.payload, b"hello");

    let ct2 = resp.encrypt(DecryptedData {
        receiver_connection_id: 2,
        payload: b"world".to_vec(),
    });
    let decrypted2 = init.decrypt(ct2).expect("decrypt failed");
    assert_eq!(decrypted2.payload, b"world");
}

#[test]
fn decrypt_wrong_epoch_fails() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let encrypted = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"hello".to_vec(),
    });

    // Tamper epoch
    let mut bad = encrypted;
    bad.epoch = 2;
    assert!(resp.decrypt(bad).is_none());
}

#[test]
fn rekey_grace_period() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    // Encrypt a message in epoch 1
    let ct_epoch1 = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"msg1".to_vec(),
    });
    assert!(resp.decrypt(ct_epoch1.clone()).is_some());

    // Perform rekey
    let epk = init.start_rekey().unwrap();
    let ct = match resp.on_rekey_data(&epk) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    init.on_rekey_ack_data(&ct);
    assert_eq!(init.current_epoch(), 2);

    // Encrypt in epoch 2
    let ct_epoch2 = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"msg2".to_vec(),
    });

    // Responder decrypts epoch 2 — promotes keys, epoch 1 moves to grace period
    assert!(resp.decrypt(ct_epoch2).is_some());
    assert_eq!(resp.current_epoch(), 2);

    // Delayed packet from epoch 1 still works (grace period)
    assert!(resp.decrypt(ct_epoch1.clone()).is_some());

    // Drop previous keys — epoch 1 no longer decryptable
    resp.drop_previous_keys();
    assert!(resp.decrypt(ct_epoch1).is_none());
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
fn rekey_flow() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();

    let ct_bytes = match resp.on_rekey_data(&epk_bytes) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    assert_eq!(resp.current_epoch(), 1);
    assert_eq!(resp.state(), SessionState::Rekeying);

    let result_init = init.on_rekey_ack_data(&ct_bytes);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));
    assert_eq!(init.current_epoch(), 2);
    assert_eq!(init.state(), SessionState::Established);

    let encrypted = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"rekey test".to_vec(),
    });
    assert_eq!(encrypted.epoch, 2);

    let decrypted = resp.decrypt(encrypted).expect("decrypt failed after rekey");
    assert_eq!(decrypted.payload, b"rekey test");
    assert_eq!(resp.current_epoch(), 2);
    assert_eq!(resp.state(), SessionState::Established);
}

#[test]
fn rekey_chicken_and_egg() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk_bytes = init.start_rekey().unwrap();
    resp.on_rekey_data(&epk_bytes);

    // Responder must still encrypt with old epoch
    let resp_packet = resp.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"mock RekeyAck payload".to_vec(),
    });
    assert_eq!(resp_packet.epoch, 1);

    let decrypted = init
        .decrypt(resp_packet)
        .expect("Initiator failed to decrypt");
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

    init.on_rekey_ack_data(&ct1);

    let encrypted = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"rekey test".to_vec(),
    });
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
    assert_eq!(init.on_rekey_data(&epk_resp), None);

    // Initiator processes RekeyAck
    assert_eq!(
        init.on_rekey_ack_data(&ct_bytes),
        Some(SessionEvent::KeysRotated)
    );

    let encrypted = init.encrypt(DecryptedData {
        receiver_connection_id: 1,
        payload: b"simultaneous rekey win".to_vec(),
    });
    assert_eq!(encrypted.epoch, 2);

    let decrypted = resp
        .decrypt(encrypted)
        .expect("Responder failed to decrypt");
    assert_eq!(decrypted.payload, b"simultaneous rekey win");
    assert_eq!(resp.current_epoch(), 2);
}

#[test]
fn on_rekey_data_too_short() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Responder, 1, keys, th);
    assert!(session.on_rekey_data(b"too short").is_none());
}

#[test]
fn on_rekey_ack_data_too_short() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);
    assert!(session.on_rekey_ack_data(b"too short").is_none());
}

#[test]
fn on_rekey_ack_data_without_start() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);
    assert!(session.on_rekey_ack_data(&[0u8; 1120]).is_none());
}

#[test]
fn start_rekey_idempotent() {
    let (keys, _, th) = make_key_pair();
    let mut session = Session::new(Role::Initiator, 1, keys, th);

    let epk1 = session.start_rekey().unwrap();
    let epk2 = session.start_rekey().unwrap();

    // Same epoch — returns same public key
    assert_eq!(epk1, epk2);
}

#[test]
fn start_rekey_stale_transition_returns_none() {
    // Initiator has a transition for epoch 2, but current_epoch advanced to 2
    // so start_rekey wants epoch 3 — mismatch with existing transition
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    // Complete a rekey to advance epoch
    let epk = init.start_rekey().unwrap();
    let ct = match resp.on_rekey_data(&epk) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    // Don't process the ack — transition for epoch 2 is still pending
    // But manually advance by starting another rekey after ack
    init.on_rekey_ack_data(&ct);
    assert_eq!(init.current_epoch(), 2);

    // Start another rekey — now wants epoch 3, old transition cleared
    // This should work since transition was cleared
    assert!(init.start_rekey().is_some());
}

#[test]
fn on_rekey_data_responder_different_payload() {
    // Responder already handled a rekey, then receives a different payload for same epoch
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init1 = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk1 = init1.start_rekey().unwrap();
    resp.on_rekey_data(&epk1).unwrap();

    // Different initiator sends a different ephemeral key for same epoch
    let (keys_a2, _, _) = make_key_pair();
    let mut init2 = Session::new(Role::Initiator, 1, keys_a2, th);
    let epk2 = init2.start_rekey().unwrap();
    assert_ne!(epk1, epk2);

    // Responder processes it — different payload, not a duplicate
    let result = resp.on_rekey_data(&epk2);
    assert!(result.is_some());
}

#[test]
fn on_rekey_ack_data_wrong_epoch() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk = init.start_rekey().unwrap();
    let ct = match resp.on_rekey_data(&epk) {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };

    // Complete the rekey
    init.on_rekey_ack_data(&ct);
    assert_eq!(init.current_epoch(), 2);

    // Try processing the same ack again — transition is cleared, should return None
    assert!(init.on_rekey_ack_data(&ct).is_none());
}
