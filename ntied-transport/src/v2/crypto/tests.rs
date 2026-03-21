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
    let alice = EphemeralPrivateKey::generate();
    let bob = EphemeralPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct, bob_ss) = bob.encapsulate(&alice_pk).expect("encapsulate failed");
    let alice_ss = alice.decapsulate(&ct).expect("decapsulate failed");

    assert_eq!(alice_ss, bob_ss);
}

#[test]
fn kem_wrong_key_gives_different_secret() {
    let alice = EphemeralPrivateKey::generate();
    let bob = EphemeralPrivateKey::generate();
    let eve = EphemeralPrivateKey::generate();

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
    let epk = EphemeralPrivateKey::generate().public_key();
    let bytes = epk.to_bytes();
    let restored = EphemeralPublicKey::from_bytes(&bytes);

    let alice = EphemeralPrivateKey::generate();
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
    let alice = EphemeralPrivateKey::generate();
    let bob = EphemeralPrivateKey::generate();

    let alice_pk = alice.public_key();
    let (ct, bob_ss) = bob.encapsulate(&alice_pk).expect("encapsulate failed");

    let ct_bytes = ct.to_bytes();
    let ct_restored = KemCiphertext::from_bytes(&ct_bytes);
    let alice_ss = alice.decapsulate(&ct_restored).expect("decapsulate failed");

    assert_eq!(alice_ss, bob_ss);
}
