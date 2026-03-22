use crate::v2::crypto::EncryptionKeys;

/// The role of the local peer in the connection, used to determine
/// which direction key to use for encryption and decryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

/// Maintains the cryptographic state for an active session.
///
/// Handles AEAD encryption/decryption, tracks the send counter, and manages
/// the transition between key epochs (including a grace period for delayed packets).
pub struct CryptoState {
    role: Role,
    send_counter: u64,
    current_epoch: u8,
    current_keys: EncryptionKeys,
    previous: Option<(u8, EncryptionKeys)>,
    next: Option<(u8, EncryptionKeys)>,
}

impl CryptoState {
    /// Creates a new `CryptoState` with the initial keys from the handshake.
    pub fn new(role: Role, initial_epoch: u8, keys: EncryptionKeys) -> Self {
        Self {
            role,
            send_counter: 0,
            current_epoch: initial_epoch,
            current_keys: keys,
            previous: None,
            next: None,
        }
    }

    /// Returns the local peer's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// Returns the current active key epoch.
    pub fn current_epoch(&self) -> u8 {
        self.current_epoch
    }

    /// Returns the next available send counter and increments the internal state.
    /// This ensures that a unique nonce is generated for every encrypted packet.
    pub fn next_send_counter(&mut self) -> u64 {
        let c = self.send_counter;
        self.send_counter += 1;
        c
    }

    /// Prepares new keys for a future epoch (e.g. during Rekey).
    /// These keys will be automatically promoted to current when a valid packet
    /// using this epoch is successfully decrypted.
    pub fn prepare_next_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        self.next = Some((epoch, keys));
    }

    /// Explicitly promotes the next keys to current, moving the current keys to previous.
    pub fn promote_next_keys(&mut self) {
        if let Some((next_epoch, next_keys)) = self.next.take() {
            self.install_keys(next_epoch, next_keys);
        }
    }

    /// Installs new keys for a specified epoch.
    ///
    /// The current keys are moved into a "previous" state, providing a grace
    /// period where delayed packets encrypted with the old keys can still be
    /// successfully decrypted.
    pub fn install_keys(&mut self, epoch: u8, keys: EncryptionKeys) {
        let prev_epoch = self.current_epoch;
        let prev_keys = std::mem::replace(&mut self.current_keys, keys);
        self.previous = Some((prev_epoch, prev_keys));
        self.current_epoch = epoch;
    }

    /// Discards the keys from the previous epoch.
    ///
    /// This should be called once it is confirmed that the peer has successfully
    /// received packets encrypted with the current keys.
    pub fn drop_previous_keys(&mut self) {
        self.previous = None;
    }

    /// Encrypts a plaintext payload.
    ///
    /// Uses the local peer's role to select the correct direction key from
    /// the current key set.
    pub fn encrypt(&self, counter: u64, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let key = match self.role {
            Role::Initiator => self.current_keys.initiator_key(),
            Role::Responder => self.current_keys.responder_key(),
        };
        key.encrypt(counter, aad, plaintext)
    }

    /// Attempts to decrypt a ciphertext payload from a specific epoch.
    ///
    /// Searches for the appropriate key matching the provided `epoch` (either
    /// the current key or the previous key during a grace period). Returns
    /// `None` if the epoch is unknown or if the AEAD tag verification fails.
    pub fn decrypt(
        &self,
        epoch: u8,
        counter: u64,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Option<Vec<u8>> {
        let keys = if epoch == self.current_epoch {
            Some(&self.current_keys)
        } else {
            self.previous
                .as_ref()
                .filter(|(e, _)| *e == epoch)
                .or_else(|| self.next.as_ref().filter(|(e, _)| *e == epoch))
                .map(|(_, k)| k)
        };

        let keys = keys?;
        let key = match self.role {
            Role::Initiator => keys.responder_key(),
            Role::Responder => keys.initiator_key(),
        };

        key.decrypt(counter, aad, ciphertext)
    }

    /// Checks if the provided epoch matches the next epoch. If so, promotes it to current
    /// and returns true. Otherwise, returns false.
    pub fn handle_epoch_switch(&mut self, epoch: u8) -> bool {
        if self.next.as_ref().map_or(false, |(e, _)| *e == epoch) {
            self.promote_next_keys();
            true
        } else {
            false
        }
    }
}
