use super::FragmentCollector;
use crate::crypto::{PUBLIC_KEY_SIZE, PublicKey, SIGNATURE_SIZE, Signature};

/// Manages the assembly and verification of authentication payloads
/// during Phase 2 of the connection handshake.
#[derive(Default)]
pub struct AuthState {
    collector: FragmentCollector,
}

impl AuthState {
    /// Creates a new, empty authentication state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes an incoming authentication fragment.
    ///
    /// If this fragment completes the full authentication payload, the assembled
    /// bytes are returned. Otherwise, returns `None`.
    pub fn process_fragment(&mut self, index: u8, total: u8, data: &[u8]) -> Option<Vec<u8>> {
        self.collector.add_fragment(index, total, data)
    }

    /// Resets the fragment collector, discarding any partially assembled data.
    pub fn reset(&mut self) {
        self.collector.reset();
    }

    /// Attempts to parse and verify a fully assembled authentication payload.
    ///
    /// The payload must contain exactly a serialized `PublicKey` followed by a
    /// `Signature`. If the signature correctly signs the `expected_message`
    /// (usually the session transcript hash), the `PublicKey` is returned.
    pub fn verify_payload(&self, payload: &[u8], expected_message: &[u8]) -> Option<PublicKey> {
        if payload.len() != PUBLIC_KEY_SIZE + SIGNATURE_SIZE {
            return None;
        }

        let mut pk_bytes = [0u8; PUBLIC_KEY_SIZE];
        pk_bytes.copy_from_slice(&payload[0..PUBLIC_KEY_SIZE]);

        let mut sig_bytes = [0u8; SIGNATURE_SIZE];
        sig_bytes.copy_from_slice(&payload[PUBLIC_KEY_SIZE..PUBLIC_KEY_SIZE + SIGNATURE_SIZE]);

        let pk = PublicKey::from_bytes(&pk_bytes)?;
        let sig = Signature::from_bytes(&sig_bytes)?;

        if pk.verify(expected_message, &sig) {
            Some(pk)
        } else {
            None
        }
    }
}
