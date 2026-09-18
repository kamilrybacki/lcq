//! The frame that actually goes on the air.
//!
//! [`super::Envelope`] carries human-readable identifiers, which is right for a
//! journal and wrong for a radio: strings cost roughly thirty bytes that the
//! mission manifest already knows. This form carries indices into that manifest
//! instead.
//!
//! Measured, not assumed: the readable form seals to 156 bytes, this one to
//! 105. Both fit a raw `LoRa` payload at every spreading factor — the 51-byte
//! figure that once appeared here is `LoRaWAN`'s DR0 application cap, not a PHY
//! limit, and this protocol is peer to peer. What a long-range frame cannot
//! afford is the airtime, which `DECISIONS.md` D2 measures and settles.
//!
//! Of these 105 bytes, 64 are the signature and 32 the content hash, so 91 % of
//! every frame is those two fields. The hash is the addressable part: a short
//! case reference would do, because a receiver already knows the case and can
//! verify against the full hash it holds — see `DECISIONS.md` D4.

use alloc::vec::Vec;

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

use crate::wire::crypto::{SigningKey, VerifyingKey, WireError};

extern crate alloc;

const COMPACT_DOMAIN: &[u8] = b"lorai-v1-compact";

/// A core utterance addressed by manifest indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactEnvelope {
    mission_epoch: u16,
    event: u32,
    revision: u16,
    content_hash: [u8; 32],
    started_at: u64,
    author_index: u16,
    stage: u8,
    verdict: u8,
    sequence: u64,
}

impl CompactEnvelope {
    /// Build a compact envelope from manifest indices.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        mission_epoch: u16,
        event: u32,
        revision: u16,
        content_hash: [u8; 32],
        started_at: u64,
        author_index: u16,
        stage: u8,
        verdict: u8,
        sequence: u64,
    ) -> Self {
        Self {
            mission_epoch,
            event,
            revision,
            content_hash,
            started_at,
            author_index,
            stage,
            verdict,
            sequence,
        }
    }

    /// Index of the claimed author within the mission manifest.
    #[must_use]
    pub const fn author_index(&self) -> u16 {
        self.author_index
    }

    /// Sequence number, which is also the nonce input.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The bytes a signature covers, domain-separated from the readable form so
    /// a signature over one can never verify as the other.
    #[must_use]
    pub fn transcript(&self) -> [u8; 32] {
        let mut hasher = Blake2s256::new();
        hasher.update(COMPACT_DOMAIN);
        hasher.update([self.stage]);
        hasher.update(self.mission_epoch.to_be_bytes());
        hasher.update(self.event.to_be_bytes());
        hasher.update(self.revision.to_be_bytes());
        hasher.update(self.content_hash);
        hasher.update(self.started_at.to_be_bytes());
        hasher.update(self.author_index.to_be_bytes());
        hasher.update([self.verdict]);
        hasher.update(self.sequence.to_be_bytes());
        hasher.finalize().into()
    }

    /// Sign it.
    #[must_use]
    pub fn sign(self, key: &SigningKey) -> SignedCompactEnvelope {
        let signature = key.sign(&self.transcript());
        SignedCompactEnvelope {
            envelope: self,
            signature,
        }
    }
}

/// A compact envelope with its author's signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCompactEnvelope {
    envelope: CompactEnvelope,
    #[serde(with = "super::codec::serde_signature")]
    signature: [u8; 64],
}

impl SignedCompactEnvelope {
    /// The envelope, whose author index is a claim until verified.
    #[must_use]
    pub const fn envelope(&self) -> &CompactEnvelope {
        &self.envelope
    }

    /// Check the signature against the manifest key for the claimed index.
    ///
    /// # Errors
    ///
    /// [`WireError::BadSignature`] if it does not verify.
    pub fn verify(&self, key: &VerifyingKey) -> Result<(), WireError> {
        key.verify(&self.envelope.transcript(), &self.signature)
    }
}

/// Serialise a compact frame.
///
/// # Errors
///
/// [`WireError::MalformedFrame`] if serialisation fails.
pub fn encode_compact(signed: &SignedCompactEnvelope) -> Result<Vec<u8>, WireError> {
    postcard::to_allocvec(signed).map_err(|_| WireError::MalformedFrame)
}

/// Parse a compact frame.
///
/// # Errors
///
/// [`WireError::MalformedFrame`] on truncated or unparseable bytes.
pub fn decode_compact(bytes: &[u8]) -> Result<SignedCompactEnvelope, WireError> {
    postcard::from_bytes(bytes).map_err(|_| WireError::MalformedFrame)
}
