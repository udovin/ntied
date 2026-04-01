use super::*;

#[test]
fn peer_id_is_stable() {
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();

    let id1 = public_key.peer_id();
    let id2 = public_key.peer_id();
    let id3 = private_key.public_key().peer_id();

    assert_eq!(id1, id2);
    assert_eq!(id2, id3);
}

#[test]
fn peer_id_type_byte() {
    let pk = PrivateKey::generate().public_key();
    let id = pk.peer_id();
    assert_eq!(id.to_bytes()[0], PEER_ID_TYPE_SHA3_256);
}

#[test]
fn peer_id_format_parse_roundtrip() {
    let id = PrivateKey::generate().public_key().peer_id();
    let formatted = id.format();
    let parsed = PeerId::parse(&formatted).expect("parse failed");
    assert_eq!(id, parsed);
}

#[test]
fn peer_id_parse_invalid() {
    assert!(PeerId::parse("not-valid-base64!!!").is_none());
    assert!(PeerId::parse("AAAA").is_none());
}

#[test]
fn peer_id_differs_between_keys() {
    let id1 = PrivateKey::generate().public_key().peer_id();
    let id2 = PrivateKey::generate().public_key().peer_id();
    assert_ne!(id1, id2);
}

#[test]
fn sign_and_verify() {
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let message = b"hello world";

    let signature = private_key.sign(message);
    assert!(public_key.verify(message, &signature));
}

#[test]
fn verify_wrong_message_fails() {
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();

    let signature = private_key.sign(b"correct message");
    assert!(!public_key.verify(b"wrong message", &signature));
}

#[test]
fn verify_wrong_key_fails() {
    let signer = PrivateKey::generate();
    let impostor = PrivateKey::generate().public_key();
    let message = b"signed by signer";

    let signature = signer.sign(message);
    assert!(!impostor.verify(message, &signature));
}

#[test]
fn public_key_roundtrip() {
    let pk = PrivateKey::generate().public_key();
    let bytes = pk.to_bytes();
    let restored = PublicKey::from_bytes(&bytes).expect("deserialization failed");

    assert_eq!(pk, restored);
    assert_eq!(pk.peer_id(), restored.peer_id());
}

#[test]
fn signature_roundtrip() {
    let private_key = PrivateKey::generate();
    let public_key = private_key.public_key();
    let message = b"roundtrip test";

    let sig = private_key.sign(message);
    let bytes = sig.to_bytes();
    let restored = Signature::from_bytes(&bytes).expect("deserialization failed");

    assert!(public_key.verify(message, &restored));
}

#[test]
fn kem_encapsulate_decapsulate() {
    let alice = KemPrivateKey::generate();
    let bob = KemPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct, bob_ss) = bob.encapsulate(&alice_pk).expect("encapsulate failed");
    let alice_ss = alice.decapsulate(&ct).expect("decapsulate failed");

    assert_eq!(alice_ss, bob_ss);
}

#[test]
fn kem_roles_are_interchangeable() {
    let alice = KemPrivateKey::generate();
    let bob = KemPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct1, bob_ss) = bob.encapsulate(&alice_pk).unwrap();
    let alice_ss = alice.decapsulate(&ct1).unwrap();
    assert_eq!(alice_ss, bob_ss);

    let bob_pk = bob.public_key();
    let (ct2, alice_ss2) = alice.encapsulate(&bob_pk).unwrap();
    let bob_ss2 = bob.decapsulate(&ct2).unwrap();
    assert_eq!(alice_ss2, bob_ss2);

    assert_ne!(alice_ss, alice_ss2);
}

#[test]
fn kem_wrong_key_gives_different_secret() {
    let alice = KemPrivateKey::generate();
    let bob = KemPrivateKey::generate();
    let eve = KemPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct, _bob_ss) = bob.encapsulate(&alice_pk).expect("encapsulate failed");
    let eve_ss = eve.decapsulate(&ct);

    match eve_ss {
        Some(ss) => assert_ne!(ss, _bob_ss),
        None => {}
    }
}

#[test]
fn ephemeral_public_key_roundtrip() {
    let epk = KemPrivateKey::generate().public_key();
    let bytes = epk.to_bytes();
    let restored = KemPublicKey::from_bytes(&bytes);

    let alice = KemPrivateKey::generate();
    let (ct1, ss1) = alice
        .encapsulate(&epk)
        .expect("encapsulate original failed");
    let (ct2, ss2) = alice
        .encapsulate(&restored)
        .expect("encapsulate restored failed");

    assert_ne!(ss1, ss2);

    let bytes1 = ct1.to_bytes();
    let bytes2 = ct2.to_bytes();
    assert_ne!(bytes1, bytes2);
}

#[test]
fn kem_ciphertext_roundtrip() {
    let alice = KemPrivateKey::generate();
    let bob = KemPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct, bob_ss) = bob.encapsulate(&alice_pk).expect("encapsulate failed");

    let ct_bytes = ct.to_bytes();
    let ct_restored = KemCiphertext::from_bytes(&ct_bytes);
    let alice_ss = alice.decapsulate(&ct_restored).expect("decapsulate failed");

    assert_eq!(alice_ss, bob_ss);
}

fn make_handshake() -> (EncryptionKeys, KemPublicKey, KemCiphertext, SharedSecret) {
    let initiator = KemPrivateKey::generate();
    let responder = KemPrivateKey::generate();
    let initiator_pk = initiator.public_key();
    let (ct, responder_ss) = responder.encapsulate(&initiator_pk).unwrap();
    let initiator_ss = initiator.decapsulate(&ct).unwrap();
    assert_eq!(initiator_ss, responder_ss);
    let keys = EncryptionKeys::new(&initiator_ss, &initiator_pk, &ct);
    (keys, initiator_pk, ct, initiator_ss)
}

#[test]
fn encrypt_decrypt_roundtrip() {
    let (keys, _, _, _) = make_handshake();
    let plaintext = b"hello world";
    let aad = b"packet header";

    let ciphertext = keys.initiator_key().encrypt(0, aad, plaintext);
    let decrypted = keys
        .initiator_key()
        .decrypt(0, aad, &ciphertext)
        .expect("decrypt failed");

    assert_eq!(decrypted, plaintext);
}

#[test]
fn both_sides_derive_same_keys() {
    let initiator = KemPrivateKey::generate();
    let responder = KemPrivateKey::generate();
    let initiator_pk = initiator.public_key();
    let (ct, responder_ss) = responder.encapsulate(&initiator_pk).unwrap();
    let initiator_ss = initiator.decapsulate(&ct).unwrap();

    let keys_a = EncryptionKeys::new(&initiator_ss, &initiator_pk, &ct);
    let keys_b = EncryptionKeys::new(&responder_ss, &initiator_pk, &ct);

    let plaintext = b"cross-side test";
    let aad = b"aad";

    let ciphertext = keys_a.initiator_key().encrypt(0, aad, plaintext);
    let decrypted = keys_b
        .initiator_key()
        .decrypt(0, aad, &ciphertext)
        .expect("decrypt failed");
    assert_eq!(decrypted, plaintext);

    let ciphertext = keys_b.responder_key().encrypt(5, aad, plaintext);
    let decrypted = keys_a
        .responder_key()
        .decrypt(5, aad, &ciphertext)
        .expect("decrypt failed");
    assert_eq!(decrypted, plaintext);
}

#[test]
fn decrypt_with_wrong_direction_key_fails() {
    let (keys, _, _, _) = make_handshake();
    let ciphertext = keys.initiator_key().encrypt(0, b"aad", b"secret");

    assert!(
        keys.responder_key()
            .decrypt(0, b"aad", &ciphertext)
            .is_none()
    );
}

#[test]
fn decrypt_with_wrong_counter_fails() {
    let (keys, _, _, _) = make_handshake();
    let ciphertext = keys.initiator_key().encrypt(0, b"aad", b"secret");

    assert!(
        keys.initiator_key()
            .decrypt(1, b"aad", &ciphertext)
            .is_none()
    );
}

#[test]
fn decrypt_with_wrong_aad_fails() {
    let (keys, _, _, _) = make_handshake();
    let ciphertext = keys.initiator_key().encrypt(0, b"correct aad", b"secret");

    assert!(
        keys.initiator_key()
            .decrypt(0, b"wrong aad", &ciphertext)
            .is_none()
    );
}

#[test]
fn decrypt_tampered_ciphertext_fails() {
    let (keys, _, _, _) = make_handshake();
    let mut ciphertext = keys.initiator_key().encrypt(0, b"aad", b"secret");

    ciphertext[0] ^= 0xFF;

    assert!(
        keys.initiator_key()
            .decrypt(0, b"aad", &ciphertext)
            .is_none()
    );
}

#[test]
fn different_handshake_produces_different_keys() {
    let (keys_a, _, _, _) = make_handshake();
    let (keys_b, _, _, _) = make_handshake();

    let ciphertext = keys_a.initiator_key().encrypt(0, b"aad", b"secret");

    assert!(
        keys_b
            .initiator_key()
            .decrypt(0, b"aad", &ciphertext)
            .is_none()
    );
}

#[test]
fn ciphertext_is_longer_than_plaintext_by_tag() {
    let (keys, _, _, _) = make_handshake();
    let plaintext = b"test payload";

    let ciphertext = keys.initiator_key().encrypt(0, b"", plaintext);

    assert_eq!(ciphertext.len(), plaintext.len() + AEAD_TAG_SIZE);
}
