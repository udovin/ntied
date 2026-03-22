use super::fragment::FragmentCollector;
use crate::v2::crypto::{
    EPHEMERAL_PUBLIC_KEY_SIZE, EncryptionKeys, EphemeralPrivateKey, EphemeralPublicKey,
    KEM_CIPHERTEXT_SIZE, KemCiphertext,
};

/// Manages the state machine for session key rotation (rekeying).
///
/// Handles the generation, fragmentation, assembly, and processing of ephemeral
/// keys during the rekey KEM exchange.
pub enum RekeyTransition {
    Initiator {
        epoch: u8,
        private_key: EphemeralPrivateKey,
        public_key: EphemeralPublicKey,
    },
    Responder {
        epoch: u8,
        peer_payload: Vec<u8>,
        ciphertext: KemCiphertext,
    },
}

#[derive(Default)]
pub struct RekeyState {
    collector: FragmentCollector,
    transition: Option<RekeyTransition>,
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
        self.transition = None;
    }

    /// Initiates the rekey process.
    ///
    /// Generates a new ephemeral keypair and returns the public key to be sent
    /// to the peer. The private key is retained for processing the acknowledgment.
    pub fn start_rekey(&mut self, next_epoch: u8) -> Option<EphemeralPublicKey> {
        self.collector.reset();

        if let Some(RekeyTransition::Initiator {
            epoch,
            ref public_key,
            ..
        }) = self.transition
        {
            if epoch == next_epoch {
                let mut pk_bytes = [0u8; EPHEMERAL_PUBLIC_KEY_SIZE];
                pk_bytes.copy_from_slice(&public_key.to_bytes());
                return Some(EphemeralPublicKey::from_bytes(&pk_bytes));
            } else {
                return None; // Already transitioning to a different epoch
            }
        }

        let private_key = EphemeralPrivateKey::generate();
        let public_key = private_key.public_key();

        let mut pk_bytes = [0u8; EPHEMERAL_PUBLIC_KEY_SIZE];
        pk_bytes.copy_from_slice(&public_key.to_bytes());
        let public_key_clone = EphemeralPublicKey::from_bytes(&pk_bytes);

        self.transition = Some(RekeyTransition::Initiator {
            epoch: next_epoch,
            private_key,
            public_key: public_key_clone,
        });

        Some(public_key)
    }

    /// Handles a fully assembled rekey payload from the peer (as the responder).
    ///
    /// Uses a tie-breaker (`we_win_tie_breaker`) to handle simultaneous rekey attempts.
    /// Returns the KEM ciphertext to send back alongside the newly derived keys.
    pub fn handle_rekey(
        &mut self,
        next_epoch: u8,
        payload: &[u8],
        we_win_tie_breaker: bool,
    ) -> Option<(Vec<u8>, Option<EncryptionKeys>)> {
        if payload.len() != EPHEMERAL_PUBLIC_KEY_SIZE {
            return None;
        }

        if let Some(transition) = &self.transition {
            match transition {
                RekeyTransition::Responder {
                    epoch,
                    peer_payload,
                    ciphertext,
                } => {
                    if *epoch == next_epoch && peer_payload == payload {
                        return Some((ciphertext.to_bytes().to_vec(), None)); // Duplicate, return cached
                    }
                }
                RekeyTransition::Initiator { epoch, .. } => {
                    if *epoch == next_epoch {
                        if we_win_tie_breaker {
                            return None; // We win the tie-breaker, ignore their request
                        } else {
                            // We lose, surrender our initiator state and act as responder
                            self.transition = None;
                        }
                    }
                }
            }
        }

        let mut pk_bytes = [0u8; EPHEMERAL_PUBLIC_KEY_SIZE];
        pk_bytes.copy_from_slice(payload);
        let peer_pk = EphemeralPublicKey::from_bytes(&pk_bytes);

        let local_private_key = EphemeralPrivateKey::generate();
        let (ciphertext, shared_secret) = local_private_key.encapsulate(&peer_pk)?;

        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ciphertext);
        let ct_bytes = ciphertext.to_bytes().to_vec();

        let mut ct_clone_bytes = [0u8; KEM_CIPHERTEXT_SIZE];
        ct_clone_bytes.copy_from_slice(&ciphertext.to_bytes());
        let ciphertext_clone = KemCiphertext::from_bytes(&ct_clone_bytes);

        self.transition = Some(RekeyTransition::Responder {
            epoch: next_epoch,
            peer_payload: payload.to_vec(),
            ciphertext: ciphertext_clone,
        });

        Some((ct_bytes, Some(keys)))
    }

    /// Handles a fully assembled rekey acknowledgment payload (as the initiator).
    ///
    /// Decapsulates the ciphertext using the previously retained ephemeral private
    /// key and derives the final set of new encryption keys.
    pub fn handle_rekey_ack(&mut self, next_epoch: u8, payload: &[u8]) -> Option<EncryptionKeys> {
        if payload.len() != KEM_CIPHERTEXT_SIZE {
            return None;
        }

        if let Some(RekeyTransition::Initiator {
            epoch, private_key, ..
        }) = &self.transition
        {
            if *epoch == next_epoch {
                let mut ct_bytes = [0u8; KEM_CIPHERTEXT_SIZE];
                ct_bytes.copy_from_slice(payload);
                let ciphertext = KemCiphertext::from_bytes(&ct_bytes);

                let shared_secret = private_key.decapsulate(&ciphertext)?;
                let public_key = private_key.public_key();

                let keys = EncryptionKeys::new(&shared_secret, &public_key, &ciphertext);
                return Some(keys);
            }
        }

        None
    }

    /// Clears the heavy transition state if the provided epoch matches the one we were transitioning to.
    /// This is called when we have successfully decrypted a packet from the new epoch, proving the peer switched.
    pub fn handle_switch(&mut self, epoch: u8) -> bool {
        match &self.transition {
            Some(RekeyTransition::Initiator { epoch: e, .. })
            | Some(RekeyTransition::Responder { epoch: e, .. }) => {
                if *e == epoch {
                    self.transition = None;
                    return true;
                }
            }
            None => {}
        }
        false
    }
}
