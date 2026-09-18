//! Signing, verification and group encryption, on vetted primitives only.

use alloc::vec::Vec;
use core::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

extern crate alloc;

/// Why a cryptographic or wire operation failed.
///
/// Deliberately coarse. A caller learns that a frame is unusable, not which
/// check rejected it, because a detailed error is an oracle for anyone probing
/// the node with malformed traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The signature does not match this author over these bytes.
    BadSignature,
    /// The frame did not decrypt, or was altered after sealing.
    DecryptionFailed,
    /// The bytes are not a well-formed frame.
    MalformedFrame,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::BadSignature => "signature does not verify",
            Self::DecryptionFailed => "frame did not decrypt",
            Self::MalformedFrame => "frame is malformed",
        };
        f.write_str(message)
    }
}

impl core::error::Error for WireError {}

/// A member's signing key.
#[derive(Debug, Clone)]
pub struct SigningKey(ed25519_dalek::SigningKey);

impl SigningKey {
    /// Build from a 32-byte seed.
    ///
    /// Seeds come from the mission manifest provisioning, not from the radio.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// The matching public key.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(self.0.verifying_key())
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.0.sign(message).to_bytes()
    }
}

/// A member's public key, as carried in the mission manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyingKey(ed25519_dalek::VerifyingKey);

impl VerifyingKey {
    /// Check a signature over `message`.
    ///
    /// # Errors
    ///
    /// [`WireError::BadSignature`] if it does not verify.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> Result<(), WireError> {
        let signature = ed25519_dalek::Signature::from_bytes(signature);
        self.0
            .verify_strict(message, &signature)
            .map_err(|_| WireError::BadSignature)
    }
}

/// The mission-wide encryption key.
///
/// Shared by every member, so it says nothing about who sent a frame.
#[derive(Debug, Clone)]
pub struct GroupKey([u8; 32]);

impl GroupKey {
    /// Build from provisioned bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Derive the nonce from a sequence number.
///
/// The sequence is unique per sender per key by the journal's construction, and
/// is also bound as associated data, so a frame cannot be replayed under a
/// different sequence. Reuse here would be a decryption oracle, which is why
/// the journal never rewinds its counter across a restart.
fn nonce_for(sequence: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[..8].copy_from_slice(&sequence.to_be_bytes());
    *Nonce::from_slice(&bytes)
}

/// Encrypt `plaintext` for the mission group.
///
/// # Errors
///
/// [`WireError::DecryptionFailed`] if the primitive refuses the input.
pub fn seal(key: &GroupKey, sequence: u64, plaintext: &[u8]) -> Result<Vec<u8>, WireError> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| WireError::DecryptionFailed)?;
    let aad = sequence.to_be_bytes();
    cipher
        .encrypt(
            &nonce_for(sequence),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| WireError::DecryptionFailed)
}

/// Decrypt a frame sealed for the mission group.
///
/// # Errors
///
/// [`WireError::DecryptionFailed`] on a wrong key, a wrong sequence, or any
/// alteration after sealing.
pub fn open(key: &GroupKey, sequence: u64, sealed: &[u8]) -> Result<Vec<u8>, WireError> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| WireError::DecryptionFailed)?;
    let aad = sequence.to_be_bytes();
    cipher
        .decrypt(
            &nonce_for(sequence),
            Payload {
                msg: sealed,
                aad: &aad,
            },
        )
        .map_err(|_| WireError::DecryptionFailed)
}
