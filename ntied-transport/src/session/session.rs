use super::{CryptoState, RekeyState, Role};
use crate::crypto::{EncryptionKeys, PublicKey, PUBLIC_KEY_SIZE, SIGNATURE_SIZE, Signature};
use crate::wire::packet::Data;

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

pub struct DecryptedData {
    pub receiver_connection_id: u64,
    pub payload: Vec<u8>,
}

pub struct Session {
    crypto: CryptoState,
    state: SessionState,
    rekey: RekeyState,
    transcript_hash: [u8; 32],
}

impl Session {
    pub fn new(
        role: Role,
        initial_epoch: u8,
        keys: EncryptionKeys,
        transcript_hash: [u8; 32],
    ) -> Self {
        Self {
            crypto: CryptoState::new(role, initial_epoch, keys),
            state: SessionState::Handshake,
            rekey: RekeyState::new(),
            transcript_hash,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn set_state(&mut self, state: SessionState) {
        self.state = state;
    }

    pub fn current_epoch(&self) -> u8 {
        self.crypto.current_epoch()
    }

    pub fn encrypt(&mut self, data: DecryptedData) -> Data {
        let counter = self.crypto.next_send_counter();
        let epoch = self.crypto.current_epoch();

        let mut packet = Data {
            epoch,
            receiver_connection_id: data.receiver_connection_id,
            counter,
            encrypted_payload: Vec::new(),
        };

        let aad = packet.aad();
        packet.encrypted_payload = self.crypto.encrypt(counter, &aad, &data.payload);

        packet
    }

    pub fn decrypt(&mut self, data: Data) -> Option<DecryptedData> {
        let aad = data.aad();

        let payload =
            self.crypto
                .decrypt(data.epoch, data.counter, &aad, &data.encrypted_payload)?;

        if self.crypto.handle_epoch_switch(data.epoch) {
            self.rekey.handle_switch(data.epoch);
        }

        Some(DecryptedData {
            receiver_connection_id: data.receiver_connection_id,
            payload,
        })
    }

    pub fn install_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        self.crypto.install_keys(epoch, keys);
    }

    pub fn drop_previous_keys(&mut self) {
        self.crypto.drop_previous_keys();
    }

    pub fn start_rekey(&mut self) -> Option<Vec<u8>> {
        let next_epoch = self.crypto.current_epoch().wrapping_add(1);
        self.rekey
            .start_rekey(next_epoch)
            .map(|pk| pk.to_bytes().to_vec())
    }

    pub fn on_auth_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        let pk = verify_auth_payload(payload, &self.transcript_hash)?;
        self.state = SessionState::Established;
        Some(SessionEvent::AuthCompleted(pk))
    }

    pub fn on_rekey_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        let next_epoch = self.crypto.current_epoch().wrapping_add(1);
        let we_win = self.crypto.role() == Role::Initiator;
        let (ct_bytes, maybe_keys) = self.rekey.handle_rekey(next_epoch, payload, we_win)?;
        if let Some(keys) = maybe_keys {
            self.crypto.prepare_next_keys(next_epoch, keys);
        }
        self.state = SessionState::Established;
        Some(SessionEvent::SendRekeyAck(ct_bytes))
    }

    pub fn on_rekey_ack_data(&mut self, payload: &[u8]) -> Option<SessionEvent> {
        let next_epoch = self.crypto.current_epoch().wrapping_add(1);
        let keys = self.rekey.handle_rekey_ack(next_epoch, payload)?;
        self.crypto.prepare_next_keys(next_epoch, keys);
        self.crypto.promote_next_keys();
        self.rekey.handle_switch(next_epoch);
        self.state = SessionState::Established;
        Some(SessionEvent::KeysRotated)
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
