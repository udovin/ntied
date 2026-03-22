use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use sha3::{Digest, Sha3_256};

use super::{EphemeralPublicKey, KemCiphertext, SharedSecret};

pub const AEAD_KEY_SIZE: usize = 32;
pub const AEAD_NONCE_SIZE: usize = 12;
pub const AEAD_TAG_SIZE: usize = 16;

const I2R_LABEL: &[u8] = b"i2r";
const R2I_LABEL: &[u8] = b"r2i";
const DIRECTION_INITIATOR: u8 = 0x01;
const DIRECTION_RESPONDER: u8 = 0x02;

pub struct EncryptionKeys {
    initiator: EncryptionKey,
    responder: EncryptionKey,
}

pub struct EncryptionKey {
    raw: [u8; AEAD_KEY_SIZE],
    direction_tag: u8,
}

impl EncryptionKeys {
    pub fn new(
        shared_secret: &SharedSecret,
        ephemeral_pk: &EphemeralPublicKey,
        kem_ciphertext: &KemCiphertext,
    ) -> Self {
        let transcript_hash = compute_transcript_hash(ephemeral_pk, kem_ciphertext);
        let hkdf = Hkdf::<Sha3_256>::new(Some(&transcript_hash), shared_secret.as_bytes());
        Self {
            initiator: EncryptionKey {
                raw: hkdf_expand(&hkdf, I2R_LABEL),
                direction_tag: DIRECTION_INITIATOR,
            },
            responder: EncryptionKey {
                raw: hkdf_expand(&hkdf, R2I_LABEL),
                direction_tag: DIRECTION_RESPONDER,
            },
        }
    }

    pub fn initiator_key(&self) -> &EncryptionKey {
        &self.initiator
    }

    pub fn responder_key(&self) -> &EncryptionKey {
        &self.responder
    }
}

impl EncryptionKey {
    pub fn encrypt(&self, counter: u64, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.raw));
        let nonce = self.build_nonce(counter);
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .expect("ChaCha20-Poly1305 encryption failed")
    }

    pub fn decrypt(&self, counter: u64, aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.raw));
        let nonce = self.build_nonce(counter);
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .ok()
    }

    fn build_nonce(&self, counter: u64) -> [u8; AEAD_NONCE_SIZE] {
        let mut nonce = [0u8; AEAD_NONCE_SIZE];
        nonce[..8].copy_from_slice(&counter.to_le_bytes());
        nonce[AEAD_NONCE_SIZE - 1] = self.direction_tag;
        nonce
    }
}

pub fn compute_transcript_hash(
    ephemeral_pk: &EphemeralPublicKey,
    kem_ciphertext: &KemCiphertext,
) -> [u8; 32] {
    let mut hasher = Sha3_256::new();
    hasher.update(&ephemeral_pk.to_bytes());
    hasher.update(&kem_ciphertext.to_bytes());
    hasher.finalize().into()
}

fn hkdf_expand(hkdf: &Hkdf<Sha3_256>, label: &[u8]) -> [u8; AEAD_KEY_SIZE] {
    let mut key = [0u8; AEAD_KEY_SIZE];
    hkdf.expand(label, &mut key)
        .expect("HKDF-Expand failed: invalid output length");
    key
}
