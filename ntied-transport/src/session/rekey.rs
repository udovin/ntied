use crate::crypto::{
    EPHEMERAL_PUBLIC_KEY_SIZE, EncryptionKeys, EphemeralPrivateKey, EphemeralPublicKey,
    KEM_CIPHERTEXT_SIZE, KemCiphertext,
};

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
    transition: Option<RekeyTransition>,
}

impl RekeyState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_rekey(&mut self, next_epoch: u8) -> Option<EphemeralPublicKey> {
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
                return None;
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
                        return Some((ciphertext.to_bytes().to_vec(), None));
                    }
                }
                RekeyTransition::Initiator { epoch, .. } => {
                    if *epoch == next_epoch {
                        if we_win_tie_breaker {
                            return None;
                        } else {
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
