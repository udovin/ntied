use crate::crypto::{
    EncryptionKeys, KEM_CIPHERTEXT_SIZE, KEM_PUBLIC_KEY_SIZE, KemCiphertext, KemPrivateKey,
    KemPublicKey, PUBLIC_KEY_SIZE, PublicKey, SIGNATURE_SIZE, Signature,
};
use crate::wire::packet::Data;

pub const MAX_EPOCHS: u8 = 32;

fn next_epoch(current: u8) -> u8 {
    (current + 1) % MAX_EPOCHS
}

#[derive(Debug, PartialEq)]
pub enum SessionEvent {
    AuthCompleted(PublicKey),
    SendRekeyAck(Vec<u8>),
    KeysRotated,
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
        peer_payload: Vec<u8>,
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
        if let Some((next_epoch, next_keys)) = self.next_keys.take() {
            self.install_keys(next_epoch, next_keys);
        }
    }

    pub fn on_auth_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        let pk = verify_auth_payload(payload, &self.transcript_hash)?;
        self.state = SessionState::Established;
        Some(SessionEvent::AuthCompleted(pk))
    }

    pub fn start_rekey(&mut self) -> Option<Vec<u8>> {
        let next_epoch = next_epoch(self.current_epoch);

        if let Some(RekeyTransition::Initiator {
            epoch,
            ref public_key,
            ..
        }) = self.rekey_transition
        {
            if epoch == next_epoch {
                let mut pk_bytes = [0u8; KEM_PUBLIC_KEY_SIZE];
                pk_bytes.copy_from_slice(&public_key.to_bytes());
                return Some(KemPublicKey::from_bytes(&pk_bytes).to_bytes().to_vec());
            } else {
                return None;
            }
        }

        let private_key = KemPrivateKey::generate();
        let public_key = private_key.public_key();
        let pk_bytes_vec = public_key.to_bytes().to_vec();

        let mut pk_clone_bytes = [0u8; KEM_PUBLIC_KEY_SIZE];
        pk_clone_bytes.copy_from_slice(&public_key.to_bytes());
        let public_key_clone = KemPublicKey::from_bytes(&pk_clone_bytes);

        self.rekey_transition = Some(RekeyTransition::Initiator {
            epoch: next_epoch,
            private_key,
            public_key: public_key_clone,
        });

        self.state = SessionState::Rekeying;
        Some(pk_bytes_vec)
    }

    pub fn on_rekey_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        if payload.len() != KEM_PUBLIC_KEY_SIZE {
            return None;
        }

        let next_epoch = next_epoch(self.current_epoch);
        let we_win = self.role == Role::Initiator;

        if let Some(transition) = &self.rekey_transition {
            match transition {
                RekeyTransition::Responder {
                    epoch,
                    peer_payload,
                    ciphertext,
                } => {
                    if *epoch == next_epoch && peer_payload == payload {
                        let ct_bytes = ciphertext.to_bytes().to_vec();
                        return Some(SessionEvent::SendRekeyAck(ct_bytes));
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

        let mut pk_bytes = [0u8; KEM_PUBLIC_KEY_SIZE];
        pk_bytes.copy_from_slice(payload);
        let peer_pk = KemPublicKey::from_bytes(&pk_bytes);

        let local_private_key = KemPrivateKey::generate();
        let (ciphertext, shared_secret) = local_private_key.encapsulate(&peer_pk)?;

        let keys = EncryptionKeys::new(&shared_secret, &peer_pk, &ciphertext);
        let ct_bytes = ciphertext.to_bytes().to_vec();

        self.prepare_next_keys(next_epoch, keys);

        let mut ct_clone_bytes = [0u8; KEM_CIPHERTEXT_SIZE];
        ct_clone_bytes.copy_from_slice(&ciphertext.to_bytes());
        let ciphertext_clone = KemCiphertext::from_bytes(&ct_clone_bytes);

        self.rekey_transition = Some(RekeyTransition::Responder {
            epoch: next_epoch,
            peer_payload: payload.to_vec(),
            ciphertext: ciphertext_clone,
        });

        self.state = SessionState::Rekeying;
        Some(SessionEvent::SendRekeyAck(ct_bytes))
    }

    pub fn on_rekey_ack_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        if payload.len() != KEM_CIPHERTEXT_SIZE {
            return None;
        }

        let next_epoch = next_epoch(self.current_epoch);

        if let Some(RekeyTransition::Initiator {
            epoch, private_key, ..
        }) = &self.rekey_transition
        {
            if *epoch == next_epoch {
                let mut ct_bytes = [0u8; KEM_CIPHERTEXT_SIZE];
                ct_bytes.copy_from_slice(payload);
                let ciphertext = KemCiphertext::from_bytes(&ct_bytes);

                let shared_secret = private_key.decapsulate(&ciphertext)?;
                let public_key = private_key.public_key();

                let keys = EncryptionKeys::new(&shared_secret, &public_key, &ciphertext);
                self.prepare_next_keys(next_epoch, keys);
                self.promote_next_keys();
                self.clear_rekey_transition(next_epoch);
                self.state = SessionState::Established;
                return Some(SessionEvent::KeysRotated);
            }
        }

        None
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

fn verify_auth_payload(payload: &[u8], expected_message: &[u8]) -> Option<PublicKey> {
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
