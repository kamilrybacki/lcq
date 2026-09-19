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
/// Domain of the case reference: what makes eight bytes of a hash a name.
const CASE_DOMAIN: &[u8] = b"lcq-v1-case";

/// Bytes of the case reference a frame carries instead of the content hash.
pub const CASE_REFERENCE_BYTES: usize = 8;

/// The short reference a frame carries for a case: the first eight bytes of
/// a domain-separated hash of the content hash. A lookup key, not a
/// commitment -- the signature covers the full hash, which the receiver
/// reconstructs from the case it already holds, and a frame that names the
/// wrong case simply fails to verify (D20).
#[must_use]
pub fn case_reference(content_hash: &[u8; 32]) -> [u8; CASE_REFERENCE_BYTES] {
    let mut hasher = Blake2s256::new();
    hasher.update(CASE_DOMAIN);
    hasher.update(content_hash);
    let digest: [u8; 32] = hasher.finalize().into();
    let mut reference = [0u8; CASE_REFERENCE_BYTES];
    reference.copy_from_slice(&digest[..CASE_REFERENCE_BYTES]);
    reference
}

/// The widest frame this protocol puts on the air, in bytes, sealed and with
/// its cleartext header.
///
/// A slot has to be wide enough for the widest frame that can ever occupy it,
/// or a slot sized to today's frame silently overlaps its neighbour the day a
/// field is added. This is a protocol constant, not an implementation figure:
/// a sender whose frame exceeds it refuses to transmit rather than trusting the
/// slot to stretch. Measured with every field at its maximum encoding and
/// every acknowledgement bit set; see `tests/wire.rs`. Twenty-four bytes
/// narrower since D20 replaced the content hash with a reference.
pub const MAX_FRAME_BYTES: usize = 152;

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

/// Which round an utterance belongs to.
///
/// A round is opened by one frame, and that frame is named by who sent it and
/// under which sequence number. The journal guarantees a member never reuses a
/// sequence, so two triggers from the same member are necessarily two different
/// rounds -- no hashing, no truncation, and nothing an attacker can grind a
/// collision for.
///
/// This exists because signatures do not stop a member saying two things. A
/// compromised member can send different, validly signed triggers to different
/// halves of a fleet and leave them counting slots from different instants,
/// which makes them collide with each other indefinitely. It cannot fabricate a
/// quorum -- votes bind to the subject and the journal allows one each -- so the
/// damage is liveness. Carrying the round in every frame turns that from an
/// invisible pile-up into something the first frame from the other half
/// reveals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundId {
    opener: u16,
    sequence: u32,
}

impl RoundId {
    /// The round opened by `opener` under `sequence`.
    #[must_use]
    pub const fn new(opener: u16, sequence: u32) -> Self {
        Self { opener, sequence }
    }

    /// No round yet.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            opener: u16::MAX,
            sequence: 0,
        }
    }

    /// Whether this names a round at all.
    #[must_use]
    pub const fn is_set(&self) -> bool {
        self.opener != u16::MAX
    }

    /// Who opened it.
    #[must_use]
    pub const fn opener(&self) -> u16 {
        self.opener
    }

    /// The opener's sequence number for the frame that opened it.
    #[must_use]
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// The bytes a signature covers.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 6] {
        let opener = self.opener.to_be_bytes();
        let sequence = self.sequence.to_be_bytes();
        [
            opener[0],
            opener[1],
            sequence[0],
            sequence[1],
            sequence[2],
            sequence[3],
        ]
    }
}

/// A core utterance addressed by manifest indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactEnvelope {
    mission_epoch: u16,
    event: u32,
    revision: u16,
    case: [u8; CASE_REFERENCE_BYTES],
    started_at: u64,
    author_index: u16,
    stage: u8,
    verdict: u8,
    sequence: u64,
    heard: Heard,
    round: RoundId,
}

impl CompactEnvelope {
    /// Build a compact envelope from manifest indices, naming the case by the
    /// reference of its content hash.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
            case: case_reference(&content_hash),
            started_at,
            author_index,
            stage,
            verdict,
            sequence,
            heard: Heard::none(),
            round: RoundId::none(),
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

    /// Mission epoch the utterance was made under.
    #[must_use]
    pub const fn mission_epoch(&self) -> u16 {
        self.mission_epoch
    }

    /// The event this utterance is about, as a manifest index.
    #[must_use]
    pub const fn event(&self) -> u32 {
        self.event
    }

    /// Revision of the claim.
    #[must_use]
    pub const fn revision(&self) -> u16 {
        self.revision
    }

    /// The reference to the claim's content: [`case_reference`] of the hash.
    #[must_use]
    pub const fn case(&self) -> &[u8; CASE_REFERENCE_BYTES] {
        &self.case
    }

    /// When the claim started, in seconds since the mission epoch.
    #[must_use]
    pub const fn started_at(&self) -> u64 {
        self.started_at
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

    /// The same envelope, declaring which round it belongs to.
    #[must_use]
    pub const fn in_round(mut self, round: RoundId) -> Self {
        self.round = round;
        self
    }

    /// The round this utterance belongs to.
    #[must_use]
    pub const fn round(&self) -> RoundId {
        self.round
    }

    /// The bytes a signature covers, domain-separated from the readable form so
    /// a signature over one can never verify as the other.
    #[must_use]
    pub fn transcript(&self, content_hash: &[u8; 32]) -> [u8; 32] {
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
        // Signed, so a frame cannot be moved from the round it was cast in into
        // another one.
        hasher.update(self.round.bytes());
        // The full hash of the case, which only the wire does not carry: the
        // receiver supplies the one it holds, and a frame about another case
        // fails right here.
        hasher.update(content_hash);
        hasher.update(self.case);
        hasher.update(self.started_at.to_be_bytes());
        hasher.update(self.author_index.to_be_bytes());
        hasher.update([self.verdict]);
        hasher.update(self.sequence.to_be_bytes());
        hasher.finalize().into()
    }

    /// Sign it, over the full content hash of the case it is about.
    #[must_use]
    pub fn sign(self, key: &SigningKey, content_hash: &[u8; 32]) -> SignedCompactEnvelope {
        let signature = key.sign(&self.transcript(content_hash));
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

    /// Check the signature against the manifest key for the claimed index,
    /// over the content hash of the case the receiver holds.
    ///
    /// # Errors
    ///
    /// [`WireError::BadSignature`] if it does not verify -- including when the
    /// frame was signed over another case.
    pub fn verify(&self, key: &VerifyingKey, content_hash: &[u8; 32]) -> Result<(), WireError> {
        key.verify(&self.envelope.transcript(content_hash), &self.signature)
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
