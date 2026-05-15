use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce, Tag,
    aead::{Aead, AeadInPlace, KeyInit, Payload},
};
use hkdf::Hkdf;
use sha3::{Digest, Sha3_256};

use super::{KemCiphertext, KemPublicKey, SharedSecret};

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

impl EncryptionKeys {
    pub fn new(
        shared_secret: &SharedSecret,
        ephemeral_pk: &KemPublicKey,
        kem_ciphertext: &KemCiphertext,
    ) -> Self {
        let transcript_hash = compute_transcript_hash(ephemeral_pk, kem_ciphertext);
        let hkdf = Hkdf::<Sha3_256>::new(Some(&transcript_hash), &shared_secret.to_bytes());
        Self {
            initiator: EncryptionKey::new(hkdf_expand(&hkdf, I2R_LABEL), DIRECTION_INITIATOR),
            responder: EncryptionKey::new(hkdf_expand(&hkdf, R2I_LABEL), DIRECTION_RESPONDER),
        }
    }

    pub fn initiator_key(&self) -> &EncryptionKey {
        &self.initiator
    }

    pub fn responder_key(&self) -> &EncryptionKey {
        &self.responder
    }

    /// Consume self and return (initiator_key, responder_key).
    pub fn into_keys(self) -> (EncryptionKey, EncryptionKey) {
        (self.initiator, self.responder)
    }
}

/// AEAD encryption key with cached cipher state.
///
/// The `ChaCha20Poly1305` cipher is initialized once at construction and
/// reused across `encrypt`/`decrypt` calls — avoids per-call re-keying.
pub struct EncryptionKey {
    cipher: ChaCha20Poly1305,
    direction_tag: u8,
}

impl EncryptionKey {
    fn new(raw: [u8; AEAD_KEY_SIZE], direction_tag: u8) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&raw));
        Self { cipher, direction_tag }
    }

    pub fn encrypt(&self, counter: u64, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let nonce = self.build_nonce(counter);
        self.cipher
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
        let nonce = self.build_nonce(counter);
        self.cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .ok()
    }

    /// Encrypt `plaintext_len` bytes at the start of `data` in place, then
    /// write the 16-byte AEAD tag immediately after.
    ///
    /// `data` must have at least `plaintext_len + AEAD_TAG_SIZE` bytes.
    /// Returns the total written length (`plaintext_len + AEAD_TAG_SIZE`).
    pub fn encrypt_in_place(
        &self,
        counter: u64,
        aad: &[u8],
        data: &mut [u8],
        plaintext_len: usize,
    ) -> usize {
        debug_assert!(data.len() >= plaintext_len + AEAD_TAG_SIZE);
        let nonce = self.build_nonce(counter);
        let (msg, tag_dst) = data.split_at_mut(plaintext_len);
        let tag = self
            .cipher
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), aad, msg)
            .expect("ChaCha20-Poly1305 encryption failed");
        tag_dst[..AEAD_TAG_SIZE].copy_from_slice(&tag);
        plaintext_len + AEAD_TAG_SIZE
    }

    /// Decrypt `data` in place.  `data` layout is `[ciphertext | tag (16 bytes)]`.
    /// On success, returns `Some(plaintext_len)` — the plaintext is in
    /// `data[..plaintext_len]`.  On authentication failure, returns `None` and
    /// the contents of `data` are undefined (partially decrypted).
    pub fn decrypt_in_place(
        &self,
        counter: u64,
        aad: &[u8],
        data: &mut [u8],
    ) -> Option<usize> {
        if data.len() < AEAD_TAG_SIZE {
            return None;
        }
        let plaintext_len = data.len() - AEAD_TAG_SIZE;
        let nonce = self.build_nonce(counter);
        let (msg, tag_src) = data.split_at_mut(plaintext_len);
        let tag = Tag::clone_from_slice(tag_src);
        self.cipher
            .decrypt_in_place_detached(Nonce::from_slice(&nonce), aad, msg, &tag)
            .ok()?;
        Some(plaintext_len)
    }

    fn build_nonce(&self, counter: u64) -> [u8; AEAD_NONCE_SIZE] {
        let mut nonce = [0u8; AEAD_NONCE_SIZE];
        nonce[..8].copy_from_slice(&counter.to_le_bytes());
        nonce[AEAD_NONCE_SIZE - 1] = self.direction_tag;
        nonce
    }
}

pub(crate) fn compute_transcript_hash(
    ephemeral_pk: &KemPublicKey,
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
