use base64::Engine;
use ed25519_dalek;
use hybrid_array;
use ml_dsa::signature::{Signer, Verifier};
use ml_dsa::{KeyGen, MlDsa65};
use rand::rngs::OsRng;
use sha3::{Digest, Sha3_256};

pub const ED25519_PUBLIC_KEY_SIZE: usize = 32;
pub const ED25519_SIGNATURE_SIZE: usize = 64;
pub const ML_DSA_PUBLIC_KEY_SIZE: usize = 1952;
pub const ML_DSA_SIGNATURE_SIZE: usize = 3309;
pub const PUBLIC_KEY_SIZE: usize = ED25519_PUBLIC_KEY_SIZE + ML_DSA_PUBLIC_KEY_SIZE;
pub const SIGNATURE_SIZE: usize = ED25519_SIGNATURE_SIZE + ML_DSA_SIGNATURE_SIZE;
pub const PEER_ID_SIZE: usize = 33;
pub const PEER_ID_TYPE_SHA3_256: u8 = 0x01;

pub struct PrivateKey {
    ed25519: ed25519_dalek::SigningKey,
    ml_dsa: ml_dsa::KeyPair<MlDsa65>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublicKey {
    ed25519: ed25519_dalek::VerifyingKey,
    ml_dsa: ml_dsa::VerifyingKey<MlDsa65>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    ed25519: ed25519_dalek::Signature,
    ml_dsa: ml_dsa::Signature<MlDsa65>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId([u8; PEER_ID_SIZE]);

impl PrivateKey {
    pub fn generate() -> Self {
        let mut rng = OsRng;
        let ed25519 = ed25519_dalek::SigningKey::generate(&mut rng);
        let ml_dsa = MlDsa65::key_gen(&mut rng);
        Self { ed25519, ml_dsa }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            ed25519: self.ed25519.verifying_key(),
            ml_dsa: self.ml_dsa.verifying_key().clone(),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        let ed25519_sig: ed25519_dalek::Signature = self.ed25519.sign(message);
        let ml_dsa_sig: ml_dsa::Signature<MlDsa65> = self.ml_dsa.sign(message);
        Signature {
            ed25519: ed25519_sig,
            ml_dsa: ml_dsa_sig,
        }
    }
}

impl PublicKey {
    pub fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        let ed25519_ok = self.ed25519.verify(message, &signature.ed25519).is_ok();
        let ml_dsa_ok = self.ml_dsa.verify(message, &signature.ml_dsa).is_ok();
        ed25519_ok && ml_dsa_ok
    }

    pub fn peer_id(&self) -> PeerId {
        let mut hasher = Sha3_256::new();
        hasher.update(self.ed25519.as_bytes());
        let ml_dsa_encoded = self.ml_dsa.encode();
        hasher.update(&*ml_dsa_encoded);
        let hash = hasher.finalize();
        let mut id = [0u8; PEER_ID_SIZE];
        id[0] = PEER_ID_TYPE_SHA3_256;
        id[1..].copy_from_slice(&hash);
        PeerId(id)
    }

    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_SIZE] {
        let mut buf = [0u8; PUBLIC_KEY_SIZE];
        buf[..ED25519_PUBLIC_KEY_SIZE].copy_from_slice(self.ed25519.as_bytes());
        let ml_dsa_encoded = self.ml_dsa.encode();
        buf[ED25519_PUBLIC_KEY_SIZE..].copy_from_slice(&*ml_dsa_encoded);
        buf
    }

    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_SIZE]) -> Option<Self> {
        let ed25519_bytes: &[u8; ED25519_PUBLIC_KEY_SIZE] =
            bytes[..ED25519_PUBLIC_KEY_SIZE].try_into().ok()?;
        let ed25519 = ed25519_dalek::VerifyingKey::from_bytes(ed25519_bytes).ok()?;

        let ml_dsa_slice = &bytes[ED25519_PUBLIC_KEY_SIZE..];
        let ml_dsa_encoded = hybrid_array::Array::from_fn(|i| ml_dsa_slice[i]);
        let ml_dsa = ml_dsa::VerifyingKey::<MlDsa65>::decode(&ml_dsa_encoded);

        Some(Self { ed25519, ml_dsa })
    }
}

impl Signature {
    pub fn to_bytes(&self) -> [u8; SIGNATURE_SIZE] {
        let mut buf = [0u8; SIGNATURE_SIZE];
        buf[..ED25519_SIGNATURE_SIZE].copy_from_slice(&self.ed25519.to_bytes());
        let ml_dsa_encoded = self.ml_dsa.encode();
        buf[ED25519_SIGNATURE_SIZE..].copy_from_slice(&*ml_dsa_encoded);
        buf
    }

    pub fn from_bytes(bytes: &[u8; SIGNATURE_SIZE]) -> Option<Self> {
        let ed25519 =
            ed25519_dalek::Signature::from_slice(&bytes[..ED25519_SIGNATURE_SIZE]).ok()?;

        let ml_dsa =
            ml_dsa::Signature::<MlDsa65>::try_from(&bytes[ED25519_SIGNATURE_SIZE..]).ok()?;

        Some(Self { ed25519, ml_dsa })
    }
}

impl PeerId {
    pub fn to_bytes(&self) -> [u8; PEER_ID_SIZE] {
        self.0
    }

    pub fn from_bytes(bytes: [u8; PEER_ID_SIZE]) -> Self {
        Self(bytes)
    }

    pub fn format(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .ok()?;
        let arr: [u8; PEER_ID_SIZE] = bytes.try_into().ok()?;
        Some(Self(arr))
    }
}

impl std::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PeerId({})", self.format())
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.format())
    }
}
