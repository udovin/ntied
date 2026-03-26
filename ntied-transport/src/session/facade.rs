use super::{AuthState, CryptoState, RekeyState, Role};
use crate::crypto::{EncryptionKeys, PublicKey};
use crate::wire::Frame;
use crate::wire::packet::Data;

/// Events resulting from processing control frames.
#[derive(Debug, PartialEq)]
pub enum SessionEvent {
    /// Authentication phase successfully completed.
    AuthCompleted(PublicKey),
    /// A Rekey request was processed; the provided payload should be sent as `RekeyAck` frames.
    SendRekeyAck(Vec<u8>),
    /// Keys were successfully rotated following a RekeyAck.
    KeysRotated,
}

/// Represents the current phase of the session lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Initial phase where the connection is establishing and exchanging identities.
    Handshake,
    /// Secure channel is fully established and ready for application data.
    Established,
    /// Temporary phase where the session is negotiating new encryption keys.
    Rekeying,
}

/// A structure representing data that has been successfully decrypted.
pub struct DecryptedData {
    /// The local session ID that this data was addressed to.
    pub receiver_session_id: u64,
    /// The decrypted plaintext payload containing frames.
    pub payload: Vec<u8>,
}

/// The main facade for cryptographic and connection state management.
///
/// It encapsulates the `CryptoState` for encryption/decryption, as well as the
/// state machines for handshake authentication and key rotation (rekeying).
pub struct Session {
    crypto: CryptoState,
    state: SessionState,
    auth: AuthState,
    rekey: RekeyState,
    transcript_hash: [u8; 32],
}

impl Session {
    /// Creates a new `Session` starting in the `Handshake` state.
    pub fn new(
        role: Role,
        initial_epoch: u8,
        keys: EncryptionKeys,
        transcript_hash: [u8; 32],
    ) -> Self {
        Self {
            crypto: CryptoState::new(role, initial_epoch, keys),
            state: SessionState::Handshake,
            auth: AuthState::new(),
            rekey: RekeyState::new(),
            transcript_hash,
        }
    }

    /// Returns the current state of the session.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Updates the session state to a new phase.
    pub fn set_state(&mut self, state: SessionState) {
        self.state = state;
    }

    /// Returns the current active key epoch.
    pub fn current_epoch(&self) -> u8 {
        self.crypto.current_epoch()
    }

    /// Encrypts the provided payload and wraps it in a `Data` packet.
    /// Automatically increments the session send counter.
    pub fn encrypt(&mut self, data: DecryptedData) -> Data {
        let counter = self.crypto.next_send_counter();
        let epoch = self.crypto.current_epoch();

        let mut packet = Data {
            epoch,
            receiver_session_id: data.receiver_session_id,
            counter,
            encrypted_payload: Vec::new(),
        };

        let aad = packet.aad();
        packet.encrypted_payload = self.crypto.encrypt(counter, &aad, &data.payload);

        packet
    }

    /// Attempts to decrypt the given `Data` packet.
    /// Returns the decrypted payload if successful, or `None` if decryption fails
    /// (e.g. wrong key, wrong epoch, or tampered data).
    pub fn decrypt(&mut self, data: Data) -> Option<DecryptedData> {
        let aad = data.aad();

        let payload =
            self.crypto
                .decrypt(data.epoch, data.counter, &aad, &data.encrypted_payload)?;

        if self.crypto.handle_epoch_switch(data.epoch) {
            self.rekey.handle_switch(data.epoch);
        }

        Some(DecryptedData {
            receiver_session_id: data.receiver_session_id,
            payload,
        })
    }

    /// Installs new encryption keys for the specified epoch.
    /// Previous keys are retained for a grace period to handle delayed packets.
    pub fn install_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        self.crypto.install_keys(epoch, keys);
    }

    /// Drops the keys from the previous epoch.
    pub fn drop_previous_keys(&mut self) {
        self.crypto.drop_previous_keys();
    }

    /// Initiates a key rotation and returns the ephemeral public key to send as a Rekey frame.
    pub fn start_rekey(&mut self) -> Option<Vec<u8>> {
        let next_epoch = self.crypto.current_epoch().wrapping_add(1);
        self.rekey
            .start_rekey(next_epoch)
            .map(|pk| pk.to_bytes().to_vec())
    }

    /// Returns a mutable reference to the internal `AuthState`.
    pub fn auth_state_mut(&mut self) -> &mut AuthState {
        &mut self.auth
    }

    /// Returns a mutable reference to the internal `RekeyState`.
    pub fn rekey_state_mut(&mut self) -> &mut RekeyState {
        &mut self.rekey
    }

    /// Processes an incoming control frame (e.g. Auth, Rekey, RekeyAck).
    ///
    /// Uses the internal `transcript_hash` to verify signatures if an `Auth` frame
    /// completes the authentication payload.
    pub fn process_incoming_frame(&mut self, frame: &Frame) -> Option<SessionEvent> {
        match frame {
            Frame::Auth(f) => {
                if let Some(payload) =
                    self.auth
                        .process_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    if let Some(pk) = self.auth.verify_payload(&payload, &self.transcript_hash) {
                        self.state = SessionState::Established;
                        return Some(SessionEvent::AuthCompleted(pk));
                    }
                }
            }
            Frame::Rekey(f) => {
                if let Some(payload) =
                    self.rekey
                        .process_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    let next_epoch = self.crypto.current_epoch().wrapping_add(1);
                    let we_win = self.crypto.role() == Role::Initiator;
                    if let Some((ct_bytes, maybe_keys)) =
                        self.rekey.handle_rekey(next_epoch, &payload, we_win)
                    {
                        if let Some(keys) = maybe_keys {
                            self.crypto.prepare_next_keys(next_epoch, keys);
                        }
                        self.state = SessionState::Established;
                        return Some(SessionEvent::SendRekeyAck(ct_bytes));
                    }
                }
            }
            Frame::RekeyAck(f) => {
                if let Some(payload) =
                    self.rekey
                        .process_fragment(f.fragment_index, f.fragment_total, &f.data)
                {
                    let next_epoch = self.crypto.current_epoch().wrapping_add(1);
                    if let Some(keys) = self.rekey.handle_rekey_ack(next_epoch, &payload) {
                        self.crypto.prepare_next_keys(next_epoch, keys);
                        self.crypto.promote_next_keys();
                        self.rekey.handle_switch(next_epoch);
                        self.state = SessionState::Established;
                        return Some(SessionEvent::KeysRotated);
                    }
                }
            }
            _ => {}
        }
        None
    }
}
