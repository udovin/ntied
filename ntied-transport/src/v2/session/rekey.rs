use super::fragment::FragmentCollector;
use crate::v2::crypto::{
    EPHEMERAL_PUBLIC_KEY_SIZE, EncryptionKeys, EphemeralPrivateKey, EphemeralPublicKey,
    KEM_CIPHERTEXT_SIZE, KemCiphertext,
};

/// Manages the state machine for session key rotation (rekeying).
///
/// Handles the generation, fragmentation, assembly, and processing of ephemeral
/// keys during the rekey KEM exchange.
#[derive(Default)]
pub struct RekeyState {
    collector: FragmentCollector,
    initiator_key: Option<EphemeralPrivateKey>,
}

impl RekeyState {
    /// Creates a new, empty rekey state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes an incoming rekey fragment.
    ///
    /// If the fragment completes the payload, returns the fully assembled data.
    pub fn process_fragment(&mut self, index: u8, total: u8, data: &[u8]) -> Option<Vec<u8>> {
        self.collector.add_fragment(index, total, data)
    }

    /// Resets the internal state, discarding any partially assembled data and keys.
    pub fn reset(&mut self) {
        self.collector.reset();
        self.initiator_key = None;
    }

    /// Initiates the rekey process.
    ///
    /// Generates a new ephemeral keypair and returns the public key to be sent
    /// to the peer. The private key is retained for processing the acknowledgment.
    pub fn start_rekey(&mut self) -> EphemeralPublicKey {
        self.reset();
        let private_key = EphemeralPrivateKey::generate();
        let public_key = private_key.public_key();
        self.initiator_key = Some(private_key);
        public_key
    }

    /// Handles a fully assembled rekey payload from the peer (as the responder).
    ///
    /// Encapsulates a shared secret against the peer's ephemeral public key
    /// and derives new encryption keys. Returns the KEM ciphertext to send back
    /// alongside the newly derived keys.
    pub fn handle_rekey(&mut self, payload: &[u8]) -> Option<(KemCiphertext, EncryptionKeys)> {
        if payload.len() != EPHEMERAL_PUBLIC_KEY_SIZE {
            return None;
        }

        let mut pk_bytes = [0u8; EPHEMERAL_PUBLIC_KEY_SIZE];
        pk_bytes.copy_from_slice(payload);
        let peer_pk = EphemeralPublicKey::from_bytes(&pk_bytes);

        let local_private_key = EphemeralPrivateKey::generate();
        let (ciphertext, shared_secret) = local_private_key.encapsulate(&peer_pk)?;

        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ciphertext);
        Some((ciphertext, keys))
    }

    /// Handles a fully assembled rekey acknowledgment payload (as the initiator).
    ///
    /// Decapsulates the ciphertext using the previously retained ephemeral private
    /// key and derives the final set of new encryption keys.
    pub fn handle_rekey_ack(&mut self, payload: &[u8]) -> Option<EncryptionKeys> {
        if payload.len() != KEM_CIPHERTEXT_SIZE {
            return None;
        }

        let private_key = self.initiator_key.take()?;

        let mut ct_bytes = [0u8; KEM_CIPHERTEXT_SIZE];
        ct_bytes.copy_from_slice(payload);
        let ciphertext = KemCiphertext::from_bytes(&ct_bytes);

        let shared_secret = private_key.decapsulate(&ciphertext)?;
        let public_key = private_key.public_key();

        let keys = EncryptionKeys::new(&shared_secret, &public_key, &ciphertext);
        Some(keys)
    }
}
