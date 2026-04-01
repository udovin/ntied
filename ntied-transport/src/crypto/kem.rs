use kem::{Decapsulate, Encapsulate};
use ml_kem::{EncodedSizeUser, KemCore, MlKem768, MlKem768Params};
use rand::rngs::OsRng;

const X25519_PUBLIC_KEY_SIZE: usize = 32;
const ML_KEM_PUBLIC_KEY_SIZE: usize = 1184;
const ML_KEM_CIPHERTEXT_SIZE: usize = 1088;

pub const KEM_PUBLIC_KEY_SIZE: usize = X25519_PUBLIC_KEY_SIZE + ML_KEM_PUBLIC_KEY_SIZE;
pub const KEM_CIPHERTEXT_SIZE: usize = X25519_PUBLIC_KEY_SIZE + ML_KEM_CIPHERTEXT_SIZE;
pub const SHARED_SECRET_SIZE: usize = 64;

pub struct KemPrivateKey {
    x25519_ss: x25519_dalek::StaticSecret,
    ml_kem_dk: ml_kem::kem::DecapsulationKey<MlKem768Params>,
}

#[derive(Clone)]
pub struct KemPublicKey {
    x25519_pk: x25519_dalek::PublicKey,
    ml_kem_ek: ml_kem::kem::EncapsulationKey<MlKem768Params>,
}

pub struct KemCiphertext {
    x25519_pk: x25519_dalek::PublicKey,
    ml_kem_ct: ml_kem::Ciphertext<MlKem768>,
}

// pub struct SharedSecret([u8; SHARED_SECRET_SIZE]);
pub struct SharedSecret {
    x25519_ss: x25519_dalek::SharedSecret,
    ml_kem_ss: ml_kem::SharedKey<MlKem768>,
}

impl KemPrivateKey {
    pub fn generate() -> Self {
        let x25519_ss = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        let (ml_kem_dk, _) = MlKem768::generate(&mut OsRng);
        Self {
            x25519_ss,
            ml_kem_dk,
        }
    }

    pub fn public_key(&self) -> KemPublicKey {
        KemPublicKey {
            x25519_pk: x25519_dalek::PublicKey::from(&self.x25519_ss),
            ml_kem_ek: self.ml_kem_dk.encapsulation_key().clone(),
        }
    }

    pub fn encapsulate(&self, peer_pk: &KemPublicKey) -> Option<(KemCiphertext, SharedSecret)> {
        let x25519_ss = self.x25519_ss.diffie_hellman(&peer_pk.x25519_pk);
        if !x25519_ss.was_contributory() {
            return None;
        }
        let (ml_kem_ct, ml_kem_ss) = peer_pk.ml_kem_ek.encapsulate(&mut OsRng).ok()?;
        let ct = KemCiphertext {
            x25519_pk: x25519_dalek::PublicKey::from(&self.x25519_ss),
            ml_kem_ct,
        };
        let ss = SharedSecret {
            x25519_ss,
            ml_kem_ss,
        };
        Some((ct, ss))
        // Some((ct, build_shared_secret(x25519_ss.as_bytes(), &*ml_kem_ss)))
    }

    pub fn decapsulate(&self, ct: &KemCiphertext) -> Option<SharedSecret> {
        let x25519_ss = self.x25519_ss.diffie_hellman(&ct.x25519_pk);
        if !x25519_ss.was_contributory() {
            return None;
        }
        let ml_kem_ss = self.ml_kem_dk.decapsulate(&ct.ml_kem_ct).ok()?;
        let ss = SharedSecret {
            x25519_ss,
            ml_kem_ss,
        };
        Some(ss)
        // Some(build_shared_secret(x25519_ss.as_bytes(), &*ml_kem_ss))
    }
}

// fn build_shared_secret(x25519_ss: &[u8; 32], ml_kem_ss: &[u8]) -> SharedSecret {
//     let mut raw = [0u8; SHARED_SECRET_SIZE];
//     raw[..32].copy_from_slice(x25519_ss);
//     raw[32..].copy_from_slice(ml_kem_ss);
//     SharedSecret(raw)
// }

impl KemPublicKey {
    pub fn to_bytes(&self) -> [u8; KEM_PUBLIC_KEY_SIZE] {
        let mut buf = [0u8; KEM_PUBLIC_KEY_SIZE];
        buf[..X25519_PUBLIC_KEY_SIZE].copy_from_slice(self.x25519_pk.as_bytes());
        let ml_kem_bytes = self.ml_kem_ek.as_bytes();
        buf[X25519_PUBLIC_KEY_SIZE..].copy_from_slice(&ml_kem_bytes);
        buf
    }

    pub fn from_bytes(bytes: &[u8; KEM_PUBLIC_KEY_SIZE]) -> Self {
        let x25519_bytes: [u8; X25519_PUBLIC_KEY_SIZE] =
            bytes[..X25519_PUBLIC_KEY_SIZE].try_into().unwrap();
        let x25519 = x25519_dalek::PublicKey::from(x25519_bytes);

        let ml_kem_encoded = ml_kem::array::Array::from_fn(|i| bytes[X25519_PUBLIC_KEY_SIZE + i]);
        let ml_kem = ml_kem::kem::EncapsulationKey::<MlKem768Params>::from_bytes(&ml_kem_encoded);

        Self {
            x25519_pk: x25519,
            ml_kem_ek: ml_kem,
        }
    }
}

impl KemCiphertext {
    pub fn to_bytes(&self) -> [u8; KEM_CIPHERTEXT_SIZE] {
        let mut buf = [0u8; KEM_CIPHERTEXT_SIZE];
        buf[..X25519_PUBLIC_KEY_SIZE].copy_from_slice(self.x25519_pk.as_bytes());
        buf[X25519_PUBLIC_KEY_SIZE..].copy_from_slice(&self.ml_kem_ct);
        buf
    }

    pub fn from_bytes(bytes: &[u8; KEM_CIPHERTEXT_SIZE]) -> Self {
        let x25519_bytes: [u8; X25519_PUBLIC_KEY_SIZE] =
            bytes[..X25519_PUBLIC_KEY_SIZE].try_into().unwrap();
        let x25519_public = x25519_dalek::PublicKey::from(x25519_bytes);

        let ml_kem_ct = ml_kem::array::Array::from_fn(|i| bytes[X25519_PUBLIC_KEY_SIZE + i]);

        Self {
            x25519_pk: x25519_public,
            ml_kem_ct,
        }
    }
}

impl SharedSecret {
    pub fn to_bytes(&self) -> [u8; SHARED_SECRET_SIZE] {
        let mut raw = [0u8; SHARED_SECRET_SIZE];
        raw[..32].copy_from_slice(self.x25519_ss.as_bytes());
        raw[32..].copy_from_slice(&self.ml_kem_ss);
        raw
    }
}

impl PartialEq for SharedSecret {
    fn eq(&self, other: &Self) -> bool {
        self.x25519_ss.as_bytes() == other.x25519_ss.as_bytes() && self.ml_kem_ss == other.ml_kem_ss
    }
}

impl Eq for SharedSecret {}

impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedSecret").finish_non_exhaustive()
    }
}
