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
/// different sequence. Reuse here would repeat the keystream and expose the
/// Poly1305 key, which is forgery under the group key -- which is why the
/// journal never rewinds its counter across a restart.
///
/// The author index is part of the nonce, and it has to be. The group key is
/// shared, but each member counts its own sequence from zero, so without the
/// index two members' first frames would seal under an identical nonce. That is
/// not a theoretical concern: `examples/nonce_proof` shows the XOR of two such
/// ciphertexts reproducing the XOR of their plaintexts exactly.
fn nonce_for(author_index: u16, sequence: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[..2].copy_from_slice(&author_index.to_be_bytes());
    bytes[2..10].copy_from_slice(&sequence.to_be_bytes());
    *Nonce::from_slice(&bytes)
}

/// The cleartext header every sealed frame carries: author index and sequence.
///
/// A receiver cannot open a frame without the nonce, and the nonce cannot be
/// inside the thing it decrypts. Both fields are also fed in as associated
/// data, so altering the header in flight makes the frame fail to open rather
/// than redirect it; and both are repeated inside the signed envelope, so a
/// lying header cannot change who a quorum credits.
pub const FRAME_HEADER_BYTES: usize = 10;

fn aad_for(author_index: u16, sequence: u64) -> [u8; FRAME_HEADER_BYTES] {
    let mut aad = [0u8; FRAME_HEADER_BYTES];
    aad[..2].copy_from_slice(&author_index.to_be_bytes());
    aad[2..].copy_from_slice(&sequence.to_be_bytes());
    aad
}

/// Encrypt `plaintext` for the mission group.
///
/// # Errors
///
/// [`WireError::DecryptionFailed`] if the primitive refuses the input.
pub fn seal(
    key: &GroupKey,
    author_index: u16,
    sequence: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>, WireError> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| WireError::DecryptionFailed)?;
    let aad = aad_for(author_index, sequence);
    cipher
        .encrypt(
            &nonce_for(author_index, sequence),
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
pub fn open(
    key: &GroupKey,
    author_index: u16,
    sequence: u64,
    sealed: &[u8],
) -> Result<Vec<u8>, WireError> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| WireError::DecryptionFailed)?;
    let aad = aad_for(author_index, sequence);
    cipher
        .decrypt(
            &nonce_for(author_index, sequence),
            Payload {
                msg: sealed,
                aad: &aad,
            },
        )
        .map_err(|_| WireError::DecryptionFailed)
}

/// Seal a frame for the air: cleartext header, then ciphertext.
///
/// This is what a radio actually sends. [`seal`] alone produces something no
/// receiver can open, because the nonce it needs would be inside the ciphertext.
///
/// # Errors
///
/// [`WireError::DecryptionFailed`] if the primitive refuses the input.
pub fn seal_frame(
    key: &GroupKey,
    author_index: u16,
    sequence: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>, WireError> {
    let ciphertext = seal(key, author_index, sequence, plaintext)?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + ciphertext.len());
    frame.extend_from_slice(&aad_for(author_index, sequence));
    frame.extend_from_slice(&ciphertext);
    Ok(frame)
}

/// Open a frame taken off the air, recovering who claims to have sent it.
///
/// The returned index is a *claim*: the header is authenticated, so it was not
/// altered in flight, but anyone holding the group key could have written it.
/// Only the signature inside decides authorship.
///
/// # Errors
///
/// [`WireError::MalformedFrame`] if the bytes are too short to hold a header,
/// and [`WireError::DecryptionFailed`] on a wrong key or any alteration.
pub fn open_frame(key: &GroupKey, frame: &[u8]) -> Result<(u16, u64, Vec<u8>), WireError> {
    if frame.len() <= FRAME_HEADER_BYTES {
        return Err(WireError::MalformedFrame);
    }
    let (header, ciphertext) = frame.split_at(FRAME_HEADER_BYTES);
    let author_index = u16::from_be_bytes([header[0], header[1]]);
    let mut sequence_bytes = [0u8; 8];
    sequence_bytes.copy_from_slice(&header[2..]);
    let sequence = u64::from_be_bytes(sequence_bytes);
    let plaintext = open(key, author_index, sequence, ciphertext)?;
    Ok((author_index, sequence, plaintext))
}
