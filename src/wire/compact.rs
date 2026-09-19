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

const COMPACT_DOMAIN: &[u8] = b"lcq-v1-compact";

/// Which members the sender has heard, as one bit each.
///
/// Rides on frames the protocol already sends, so acknowledgement costs eight
/// bytes rather than a frame. A member that sees its own bit set somewhere
/// knows it was heard and can stop retransmitting -- and retransmitting blind,
/// which is the alternative, costs about four and a half times the airtime
/// (`DECISIONS.md` D4).
///
/// **Advisory, never binding.** It is signed, so nobody can alter it in
/// flight, but a member can still lie about what it heard and silence somebody
/// who was not. That is a liveness attack of the same class as jamming a slot:
/// no schedule and no bitmap lets anyone forge a signature, so a fleet fed lies
/// blocks rather than approves. A receiver therefore treats this as a reason to
/// stop *early*, never as proof, and the count threshold is still decided by
/// signatures alone.
///
/// Sixty-four members, because one `u64` of bits is what a frame can spare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heard([u8; 8]);

impl Heard {
    /// Nobody heard yet.
    #[must_use]
    pub const fn none() -> Self {
        Self([0; 8])
    }

    /// The largest manifest index this can record.
    pub const CAPACITY: usize = 64;

    /// Record that `index` was heard. Indices past the capacity are dropped,
    /// which costs a retransmission and never a wrong answer.
    pub const fn heard_from(&mut self, index: usize) {
        if index < Self::CAPACITY {
            self.0[index / 8] |= 1 << (index % 8);
        }
    }

    /// Whether `index` is recorded.
    #[must_use]
    pub const fn contains(&self, index: usize) -> bool {
        index < Self::CAPACITY && self.0[index / 8] & (1 << (index % 8)) != 0
    }

    /// How many members are recorded.
    #[must_use]
    pub const fn count(&self) -> u32 {
        let mut total = 0;
        let mut at = 0;
        while at < 8 {
            total += self.0[at].count_ones();
            at += 1;
        }
        total
    }

    /// The raw bits, for a wire format that wants them.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 8] {
        self.0
    }
}

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
    heard: Heard,
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
            heard: Heard::none(),
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

    /// Which protocol stage this utterance claims to belong to.
    ///
    /// A receiver must read this from the frame rather than assume it from the
    /// phase it happens to be in. The transcript is domain-separated by stage,
    /// so a frame whose stage does not match what was signed fails to verify --
    /// but only if somebody actually looks.
    #[must_use]
    pub const fn stage(&self) -> u8 {
        self.stage
    }

    /// The verdict carried, as its wire code.
    #[must_use]
    pub const fn verdict(&self) -> u8 {
        self.verdict
    }

    /// The same envelope, reporting who the sender has heard.
    #[must_use]
    pub const fn acknowledging(mut self, heard: Heard) -> Self {
        self.heard = heard;
        self
    }

    /// Who the sender reports having heard.
    #[must_use]
    pub const fn heard(&self) -> Heard {
        self.heard
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
        // Signed, so the acknowledgement cannot be altered in flight or lifted
        // onto another frame. It can still be a lie by its author, which is why
        // a receiver treats it as advisory.
        hasher.update(self.heard.bytes());
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
