use crate::crypto::{
    EncryptionKeys, KemCiphertext, KemPrivateKey, KemPublicKey, PublicKey, Signature,
};
use crate::wire::packet::Data;

pub const MAX_EPOCHS: u8 = 32;

fn next_epoch(current: u8) -> u8 {
    (current + 1) % MAX_EPOCHS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Handshake,
    Established,
    Rekeying,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

pub struct DecryptedData {
    pub receiver_connection_id: u64,
    pub payload: Vec<u8>,
}

enum RekeyTransition {
    Initiator {
        epoch: u8,
        private_key: KemPrivateKey,
        public_key: KemPublicKey,
    },
    Responder {
        epoch: u8,
        peer_public_key: KemPublicKey,
        ciphertext: KemCiphertext,
    },
}

pub struct Session {
    role: Role,
    state: SessionState,
    transcript_hash: [u8; 32],

    // crypto state
    send_counter: u64,
    current_epoch: u8,
    current_keys: EncryptionKeys,
    previous_keys: Option<(u8, EncryptionKeys)>,
    next_keys: Option<(u8, EncryptionKeys)>,

    // rekey state
    rekey_transition: Option<RekeyTransition>,
}

impl Session {
    pub fn new(
        role: Role,
        initial_epoch: u8,
        keys: EncryptionKeys,
        transcript_hash: [u8; 32],
    ) -> Self {
        Self {
            role,
            state: SessionState::Handshake,
            transcript_hash,
            send_counter: 0,
            current_epoch: initial_epoch,
            current_keys: keys,
            previous_keys: None,
            next_keys: None,
            rekey_transition: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn current_epoch(&self) -> u8 {
        self.current_epoch
    }

    pub fn encrypt(&mut self, data: DecryptedData) -> Data {
        let counter = self.send_counter;
        self.send_counter += 1;

        let mut packet = Data {
            epoch: self.current_epoch,
            receiver_connection_id: data.receiver_connection_id,
            counter,
            encrypted_payload: Vec::new(),
        };

        let aad = packet.aad();
        let key = match self.role {
            Role::Initiator => self.current_keys.initiator_key(),
            Role::Responder => self.current_keys.responder_key(),
        };
        packet.encrypted_payload = key.encrypt(counter, &aad, &data.payload);

        packet
    }

    pub fn decrypt(&mut self, data: Data) -> Option<DecryptedData> {
        let aad = data.aad();

        let keys = if data.epoch == self.current_epoch {
            Some(&self.current_keys)
        } else {
            self.previous_keys
                .as_ref()
                .filter(|(e, _)| *e == data.epoch)
                .or_else(|| self.next_keys.as_ref().filter(|(e, _)| *e == data.epoch))
                .map(|(_, k)| k)
        };

        let keys = keys?;
        let key = match self.role {
            Role::Initiator => keys.responder_key(),
            Role::Responder => keys.initiator_key(),
        };

        let payload = key.decrypt(data.counter, &aad, &data.encrypted_payload)?;

        // Auto-promote next keys if epoch matches
        if self
            .next_keys
            .as_ref()
            .map_or(false, |(e, _)| *e == data.epoch)
        {
            self.promote_next_keys();
            self.clear_rekey_transition(data.epoch);
            self.state = SessionState::Established;
        }

        Some(DecryptedData {
            receiver_connection_id: data.receiver_connection_id,
            payload,
        })
    }

    fn install_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        let prev_epoch = self.current_epoch;
        let prev_keys = std::mem::replace(&mut self.current_keys, keys);
        self.previous_keys = Some((prev_epoch, prev_keys));
        self.current_epoch = epoch;
    }

    pub fn drop_previous_keys(&mut self) {
        self.previous_keys = None;
    }

    fn prepare_next_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        self.next_keys = Some((epoch, keys));
    }

    fn promote_next_keys(&mut self) {
        if let Some((epoch, keys)) = self.next_keys.take() {
            self.install_keys(epoch, keys);
        }
    }

    pub fn on_auth_data(&mut self, pk: &PublicKey, sig: &Signature) -> bool {
        if self.state != SessionState::Handshake {
            return false;
        }
        if !pk.verify(&self.transcript_hash, sig) {
            return false;
        }
        self.state = SessionState::Established;
        true
    }

    pub fn start_rekey(&mut self) -> Option<KemPublicKey> {
        let next_epoch = next_epoch(self.current_epoch);

        if let Some(RekeyTransition::Initiator {
            epoch,
            ref public_key,
            ..
        }) = self.rekey_transition
        {
            if epoch == next_epoch {
                return Some(public_key.clone());
            } else {
                return None;
            }
        }

        let private_key = KemPrivateKey::generate();
        let public_key = private_key.public_key();

        let result = public_key.clone();
        self.rekey_transition = Some(RekeyTransition::Initiator {
            epoch: next_epoch,
            private_key,
            public_key,
        });

        self.state = SessionState::Rekeying;
        Some(result)
    }

    pub fn on_rekey_data(&mut self, peer_pk: &KemPublicKey) -> Option<KemCiphertext> {
        let next_epoch = next_epoch(self.current_epoch);
        let we_win = self.role == Role::Initiator;

        if let Some(transition) = &self.rekey_transition {
            match transition {
                RekeyTransition::Responder {
                    epoch,
                    peer_public_key,
                    ciphertext,
                } => {
                    if *epoch == next_epoch && peer_public_key.to_bytes() == peer_pk.to_bytes() {
                        return Some(ciphertext.clone());
                    }
                }
                RekeyTransition::Initiator { epoch, .. } => {
                    if *epoch == next_epoch {
                        if we_win {
                            return None;
                        } else {
                            self.rekey_transition = None;
                        }
                    }
                }
            }
        }

        let local_private_key = KemPrivateKey::generate();
        let (ciphertext, shared_secret) = local_private_key.encapsulate(peer_pk)?;

        let keys = EncryptionKeys::new(&shared_secret, peer_pk, &ciphertext);
        self.prepare_next_keys(next_epoch, keys);

        self.rekey_transition = Some(RekeyTransition::Responder {
            epoch: next_epoch,
            peer_public_key: peer_pk.clone(),
            ciphertext: ciphertext.clone(),
        });

        self.state = SessionState::Rekeying;
        Some(ciphertext)
    }

    pub fn on_rekey_ack_data(&mut self, ciphertext: &KemCiphertext) -> bool {
        let next_epoch = next_epoch(self.current_epoch);

        if let Some(RekeyTransition::Initiator {
            epoch, private_key, ..
        }) = &self.rekey_transition
        {
            if *epoch == next_epoch {
                let Some(shared_secret) = private_key.decapsulate(ciphertext) else {
                    return false;
                };
                let public_key = private_key.public_key();

                let keys = EncryptionKeys::new(&shared_secret, &public_key, ciphertext);
                self.prepare_next_keys(next_epoch, keys);
                self.promote_next_keys();
                self.clear_rekey_transition(next_epoch);
                self.state = SessionState::Established;
                return true;
            }
        }

        false
    }

    fn clear_rekey_transition(&mut self, epoch: u8) {
        match &self.rekey_transition {
            Some(RekeyTransition::Initiator { epoch: e, .. })
            | Some(RekeyTransition::Responder { epoch: e, .. }) => {
                if *e == epoch {
                    self.rekey_transition = None;
                }
            }
            None => {}
        }
    }
}
