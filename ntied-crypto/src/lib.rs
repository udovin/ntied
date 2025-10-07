use aes_gcm::{Aes256Gcm, Nonce, aead::Aead};
use p256::ecdh::EphemeralSecret;
use p256::pkcs8::{DecodePrivateKey as _, EncodePrivateKey as _};
use p256::{PublicKey as P256PublicKey, SecretKey as P256SecretKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// A public key for cryptographic operations including ECDH key exchange and signature verification.
#[derive(Clone)]
pub struct PublicKey {
    public_key: P256PublicKey,
    verifying_key: p256::ecdsa::VerifyingKey,
}

impl PublicKey {
    /// Verify a digital signature using this public key.
    ///
    /// # Arguments
    ///
    /// * `message` - The original message that was signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the signature is valid, `Ok(false)` if invalid, or `Err` on error.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    ///
    /// let message = b"Message to sign";
    /// let signature = private_key.sign(message);
    ///
    /// // Verify with correct key
    /// assert!(public_key.verify(message, &signature).unwrap());
    ///
    /// // Verify with wrong message
    /// assert!(!public_key.verify(b"Wrong message", &signature).unwrap());
    /// ```
    pub fn verify(
        &self,
        message: impl AsRef<[u8]>,
        signature: impl AsRef<[u8]>,
    ) -> Result<bool, Error> {
        use p256::ecdsa::signature::Verifier;
        let signature = p256::ecdsa::Signature::from_slice(signature.as_ref())?;
        Ok(self
            .verifying_key
            .verify(message.as_ref(), &signature)
            .is_ok())
    }

    /// Serialize this public key to bytes in DER format.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    /// let bytes = public_key.to_bytes().unwrap();
    /// let restored = ntied_crypto::PublicKey::from_bytes(&bytes).unwrap();
    /// ```
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        use p256::pkcs8::EncodePublicKey;
        Ok(self.public_key.to_public_key_der()?.into_vec())
    }

    /// Deserialize a public key from bytes in DER format.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    /// let bytes = public_key.to_bytes().unwrap();
    /// let restored = ntied_crypto::PublicKey::from_bytes(&bytes).unwrap();
    /// ```
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        use p256::pkcs8::DecodePublicKey;
        let public_key = P256PublicKey::from_public_key_der(bytes)?;
        Ok(Self::new_from_public_key(public_key))
    }

    fn new_from_public_key(public_key: P256PublicKey) -> Self {
        let verifying_key = p256::ecdsa::VerifyingKey::from(public_key);
        Self {
            public_key,
            verifying_key,
        }
    }
}

/// A private key for cryptographic operations including key generation, signing, and ECDH.
///
/// # Examples
///
/// ```
/// use ntied_crypto::PrivateKey;
///
/// // Generate new private key
/// let private_key = PrivateKey::generate().unwrap();
///
/// // Get corresponding public key
/// let public_key = private_key.public_key();
///
/// // Sign a message
/// let message = b"Hello, world!";
/// let signature = private_key.sign(message);
///
/// // Serialize to PEM
/// let pem = private_key.to_pem().unwrap();
/// let restored = PrivateKey::from_pem(&pem).unwrap();
/// ```
#[derive(Clone)]
pub struct PrivateKey {
    secret_key: P256SecretKey,
    signing_key: p256::ecdsa::SigningKey,
}

impl PrivateKey {
    /// Generate a new random private key.
    ///
    /// Uses a cryptographically secure random number generator to create a new
    /// P-256 private key suitable for ECDH and ECDSA operations.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    /// ```
    pub fn generate() -> Result<Self, Error> {
        let secret_key = P256SecretKey::random(&mut OsRng);
        Ok(Self::from_secret_key(secret_key))
    }

    /// Get the public key corresponding to this private key.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    /// ```
    pub fn public_key(&self) -> PublicKey {
        PublicKey::new_from_public_key(self.secret_key.public_key())
    }

    /// Create a digital signature for the given message.
    ///
    /// Uses ECDSA (Elliptic Curve Digital Signature Algorithm) to create a signature
    /// that can be verified with the corresponding public key.
    ///
    /// # Arguments
    ///
    /// * `message` - The message to sign
    ///
    /// # Returns
    ///
    /// The signature as a byte vector.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let public_key = private_key.public_key();
    ///
    /// let message = b"Important message";
    /// let signature = private_key.sign(message);
    ///
    /// assert!(public_key.verify(message, &signature).unwrap());
    /// ```
    pub fn sign(&self, message: impl AsRef<[u8]>) -> Vec<u8> {
        use p256::ecdsa::signature::Signer;
        let signature: p256::ecdsa::Signature = self.signing_key.sign(message.as_ref());
        signature.to_vec()
    }

    /// Serialize the private key to PEM format.
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let pem = private_key.to_pem().unwrap();
    ///
    /// assert!(pem.contains("-----BEGIN PRIVATE KEY-----"));
    /// assert!(pem.contains("-----END PRIVATE KEY-----"));
    /// ```
    pub fn to_pem(&self) -> Result<String, Error> {
        Ok(self
            .secret_key
            .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)?
            .to_string())
    }

    /// Deserialize a private key from PEM format.
    ///
    /// # Arguments
    ///
    /// * `pem` - The PEM-encoded private key string
    ///
    /// # Examples
    ///
    /// ```
    /// use ntied_crypto::PrivateKey;
    ///
    /// let private_key = PrivateKey::generate().unwrap();
    /// let pem = private_key.to_pem().unwrap();
    /// let restored = PrivateKey::from_pem(&pem).unwrap();
    ///
    /// // Keys should be functionally equivalent
    /// let message = b"Test message";
    /// let signature1 = private_key.sign(message);
    /// let signature2 = restored.sign(message);
    ///
    /// let public1 = private_key.public_key();
    /// let public2 = restored.public_key();
    /// assert!(public1.verify(message, &signature2).unwrap());
    /// assert!(public2.verify(message, &signature1).unwrap());
    /// ```
    pub fn from_pem(pem: &str) -> Result<Self, Error> {
        let secret_key = P256SecretKey::from_pkcs8_pem(pem)?;
        Ok(Self::from_secret_key(secret_key))
    }

    fn from_secret_key(secret_key: P256SecretKey) -> Self {
        let signing_key = p256::ecdsa::SigningKey::from(&secret_key);
        Self {
            secret_key,
            signing_key,
        }
    }
}

/// Shared secret for symmetric encryption between two parties.
///
/// Created through ECDH key exchange and used for AES-GCM encryption/decryption.
#[derive(Clone)]
pub struct SharedSecret {
    cipher: Aes256Gcm,
}

impl SharedSecret {
    pub fn encrypt_nonce(&self, nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Error)
    }

    pub fn decrypt_nonce(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Error)
    }

    fn from_bytes(bytes: [u8; 32]) -> Result<Self, Error> {
        use aes_gcm::aead::KeyInit;
        let cipher = Aes256Gcm::new_from_slice(&bytes)
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Error)?;
        Ok(Self { cipher })
    }
}

/// Ephemeral key pair for Perfect Forward Secrecy (PFS).
///
/// This struct represents a temporary key pair that should be used for a single session
/// and then discarded. It provides proper PFS by ensuring that compromise of long-term
/// keys doesn't affect past sessions.
pub struct EphemeralKeyPair {
    secret: EphemeralSecret,
}

impl EphemeralKeyPair {
    /// Generate a new ephemeral key pair for PFS.
    ///
    /// Creates a fresh key pair that should be used for a single session.
    /// The private key will be automatically zeroized when dropped.
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random(&mut OsRng);
        Self { secret }
    }

    /// Get the public key bytes to send to the other party.
    ///
    /// Returns the public key in SEC1 format that can be transmitted
    /// to the other party for computing the shared secret.
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.secret.public_key().to_sec1_bytes().to_vec()
    }

    /// Compute shared secret with another party's ephemeral public key.
    ///
    /// Both parties compute the same shared secret using ECDH with their
    /// ephemeral private key and the other party's ephemeral public key.
    ///
    /// # Arguments
    ///
    /// * `other_public_key` - The other party's ephemeral public key in SEC1 format
    ///
    /// # Returns
    ///
    /// A `SharedSecret` that can be used for symmetric encryption
    ///
    /// # Security
    ///
    /// This provides Perfect Forward Secrecy as the ephemeral keys are not
    /// derived from or related to the long-term identity keys.
    pub fn compute_shared_secret(
        &self,
        other_public_key: impl AsRef<[u8]>,
    ) -> Result<SharedSecret, Error> {
        // Parse the other party's public key
        let other_public = P256PublicKey::from_sec1_bytes(other_public_key.as_ref())?;
        // Perform ECDH to get shared secret
        let shared_secret_point = self.secret.diffie_hellman(&other_public);
        let shared_secret_bytes = shared_secret_point.raw_secret_bytes();
        // Hash the shared secret with both public keys for additional security
        // This prevents certain attacks and ensures both parties contribute to the final key
        // Sort public keys to ensure consistent hashing regardless of who computes it
        let mut hasher = Sha256::new();
        hasher.update(shared_secret_bytes);
        // Hash public keys in deterministic order (lexicographic)
        let public_key_bytes = self.public_key_bytes();
        let other_bytes = other_public_key.as_ref();
        if public_key_bytes.as_slice() < other_bytes {
            hasher.update(&public_key_bytes);
            hasher.update(other_bytes);
        } else {
            hasher.update(other_bytes);
            hasher.update(&public_key_bytes);
        }
        let hashed_secret: [u8; 32] = hasher.finalize().into();
        SharedSecret::from_bytes(hashed_secret)
    }
}
