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
    let resp = Session::new(Role::Responder, 1, keys_b, th);

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

    let epk = initiator_rekey.start_rekey();
    let epk_bytes = epk.to_bytes();

    assert!(
        responder_rekey
            .process_fragment(0, 2, &epk_bytes[0..600])
            .is_none()
    );
    let assembled_epk = responder_rekey
        .process_fragment(1, 2, &epk_bytes[600..])
        .unwrap();

    let (ct, resp_keys) = responder_rekey.handle_rekey(&assembled_epk).unwrap();
    let ct_bytes = ct.to_bytes();

    let assembled_ct = initiator_rekey.process_fragment(0, 1, &ct_bytes).unwrap();
    let init_keys = initiator_rekey.handle_rekey_ack(&assembled_ct).unwrap();

    let plaintext = b"test message";
    let ciphertext = init_keys.initiator_key().encrypt(0, b"", plaintext);
    let decrypted = resp_keys
        .initiator_key()
        .decrypt(0, b"", &ciphertext)
        .unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn process_control_frame_auth() {
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

    assert_eq!(session.process_control_frame(&frame1), None);
    assert_eq!(session.state(), SessionState::Handshake);

    let result = session.process_control_frame(&frame2);
    assert_eq!(result, Some(SessionEvent::AuthCompleted(public_key)));
    assert_eq!(session.state(), SessionState::Established);
}

#[test]
fn process_control_frame_rekey_flow() {
    let (keys_a, keys_b, th) = make_key_pair();
    let mut init = Session::new(Role::Initiator, 1, keys_a, th);
    let mut resp = Session::new(Role::Responder, 1, keys_b, th);

    let epk = init.rekey_state_mut().start_rekey();
    let epk_bytes = epk.to_bytes().to_vec();

    let frame_rekey = Frame::Rekey(Rekey {
        fragment_index: 0,
        fragment_total: 1,
        data: epk_bytes,
    });

    let result_resp = resp.process_control_frame(&frame_rekey);
    let ct_bytes = match result_resp {
        Some(SessionEvent::SendRekeyAck(ct)) => ct,
        _ => panic!("Expected SendRekeyAck"),
    };
    assert_eq!(resp.current_epoch(), 2);
    assert_eq!(resp.state(), SessionState::Established);

    let frame_rekey_ack = Frame::RekeyAck(RekeyAck {
        fragment_index: 0,
        fragment_total: 1,
        data: ct_bytes,
    });

    let result_init = init.process_control_frame(&frame_rekey_ack);
    assert_eq!(result_init, Some(SessionEvent::KeysRotated));
    assert_eq!(init.current_epoch(), 2);
    assert_eq!(init.state(), SessionState::Established);

    // Verify they can communicate with new keys
    let data = DecryptedData {
        receiver_session_id: 1,
        payload: b"rekey test".to_vec(),
    };
    let encrypted = init.encrypt(data);
    let decrypted = resp.decrypt(encrypted).expect("decrypt failed after rekey");
    assert_eq!(decrypted.payload, b"rekey test");
}
