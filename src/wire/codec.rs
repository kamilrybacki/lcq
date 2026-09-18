//! The compact frame and the transcript that gets signed.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

use crate::domain::contracts::{Stage, Subject, Verdict};
use crate::domain::time::Timestamp;
use crate::wire::crypto::{SigningKey, VerifyingKey, WireError};

extern crate alloc;

/// Domain separator. Prefixing the transcript keeps a lorai signature from ever
/// verifying as a signature over anything else the same key might sign.
const TRANSCRIPT_DOMAIN: &[u8] = b"lorai-v1-endorsement";

/// One node's utterance, in the form that travels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    mission: String,
    event: String,
    revision: u32,
    content_hash: [u8; 32],
    started_at: u64,
    author: String,
    stage: u8,
    verdict: u8,
    sequence: u64,
}

impl Envelope {
    /// Build an envelope for a subject.
    #[must_use]
    pub fn new(
        subject: &Subject,
        author: &str,
        stage: Stage,
        verdict: Verdict,
        sequence: u64,
    ) -> Self {
        Self {
            mission: subject.mission().to_string(),
            event: subject.event().to_string(),
            revision: subject.revision(),
            content_hash: *subject.content_hash(),
            started_at: subject.started_at().as_secs(),
            author: author.to_string(),
            stage: stage_code(stage),
            verdict: verdict_code(verdict),
            sequence,
        }
    }

    /// Rebuild the subject this envelope refers to.
    ///
    /// # Errors
    ///
    /// [`WireError::MalformedFrame`] if the identifiers are blank.
    pub fn subject(&self) -> Result<Subject, WireError> {
        Subject::new(
            &self.mission,
            &self.event,
            self.revision,
            self.content_hash,
            Timestamp::from_secs(self.started_at),
        )
        .map_err(|_| WireError::MalformedFrame)
    }

    /// The claimed author. A claim until a signature verifies it.
    #[must_use]
    pub fn author(&self) -> &str {
        &self.author
    }

    /// Sequence number, which is also the encryption nonce input.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The exact bytes a signature covers.
    ///
    /// Every field is included and the stage is separated explicitly, so an
    /// independent opinion cannot be lifted and replayed as a binding vote —
    /// the same content at a different stage hashes differently.
    #[must_use]
    pub fn transcript(&self) -> [u8; 32] {
        let mut hasher = Blake2s256::new();
        hasher.update(TRANSCRIPT_DOMAIN);
        hasher.update([self.stage]);
        // Length-prefixed so two fields cannot be shifted into one another.
        // Truncation is impossible in practice and would only ever make the
        // transcript differ, never collide with a shorter one.
        hasher.update(
            u32::try_from(self.mission.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        hasher.update(self.mission.as_bytes());
        // Length-prefixed so two fields cannot be shifted into one another.
        // Truncation is impossible in practice and would only ever make the
        // transcript differ, never collide with a shorter one.
        hasher.update(
            u32::try_from(self.event.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        hasher.update(self.event.as_bytes());
        hasher.update(self.revision.to_be_bytes());
        hasher.update(self.content_hash);
        hasher.update(self.started_at.to_be_bytes());
        // Length-prefixed so two fields cannot be shifted into one another.
        // Truncation is impossible in practice and would only ever make the
        // transcript differ, never collide with a shorter one.
        hasher.update(
            u32::try_from(self.author.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        hasher.update(self.author.as_bytes());
        hasher.update([self.verdict]);
        hasher.update(self.sequence.to_be_bytes());
        hasher.finalize().into()
    }

    /// Sign this envelope.
    #[must_use]
    pub fn sign(self, key: &SigningKey) -> SignedEnvelope {
        let signature = key.sign(&self.transcript());
        SignedEnvelope {
            envelope: self,
            signature,
        }
    }
}

/// An envelope with its author's signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    envelope: Envelope,
    #[serde(with = "serde_signature")]
    signature: [u8; 64],
}

impl SignedEnvelope {
    /// The envelope, whose author is still only a claim until verified.
    #[must_use]
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// The raw signature bytes.
    #[must_use]
    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }

    /// Check the signature against a key from the mission manifest.
    ///
    /// # Errors
    ///
    /// [`WireError::BadSignature`] if it does not verify.
    pub fn verify(&self, key: &VerifyingKey) -> Result<(), WireError> {
        key.verify(&self.envelope.transcript(), &self.signature)
    }

    /// Alter the verdict without re-signing. Tests only.
    #[doc(hidden)]
    pub fn tamper_verdict_for_test(&mut self, verdict: Verdict) {
        self.envelope.verdict = verdict_code(verdict);
    }

    /// Alter the revision without re-signing. Tests only.
    #[doc(hidden)]
    pub fn tamper_revision_for_test(&mut self, revision: u32) {
        self.envelope.revision = revision;
    }
}

const fn stage_code(stage: Stage) -> u8 {
    match stage {
        Stage::Independent => 1,
        Stage::Consultation => 2,
        Stage::BindingSupport => 3,
    }
}

const fn verdict_code(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Support => 1,
        Verdict::Dispute => 2,
        Verdict::InsufficientData => 3,
    }
}

/// Serialise a signed envelope compactly.
///
/// # Errors
///
/// [`WireError::MalformedFrame`] if serialisation fails.
pub fn encode(signed: &SignedEnvelope) -> Result<Vec<u8>, WireError> {
    postcard::to_allocvec(signed).map_err(|_| WireError::MalformedFrame)
}

/// Parse a signed envelope.
///
/// # Errors
///
/// [`WireError::MalformedFrame`] on truncated, oversized or unparseable bytes.
pub fn decode(bytes: &[u8]) -> Result<SignedEnvelope, WireError> {
    postcard::from_bytes(bytes).map_err(|_| WireError::MalformedFrame)
}

/// `[u8; 64]` has no derived serde impl; postcard writes it as a fixed run.
pub(super) mod serde_signature {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&value.as_slice(), s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let bytes = <&[u8]>::deserialize(d)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))
    }
}
