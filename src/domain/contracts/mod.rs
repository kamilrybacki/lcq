//! What a node says, and about exactly which claim.
//!
//! The contracts here exist to make two errors unrepresentable: counting an
//! opinion as a vote, and aggregating endorsements that refer to different
//! claims. Both would produce a quorum that looks valid and means nothing.

use alloc::string::{String, ToString};
use core::fmt;

use crate::domain::time::Timestamp;

extern crate alloc;

/// Seconds from the shared start until opinions stop being collected.
pub const CONSULTATION_CUTOFF_SECONDS: u64 = 300;
/// Seconds from the shared start at which endorsement is expected.
pub const ENDORSEMENT_TARGET_SECONDS: u64 = 600;
/// Fallback validity when the input system supplies none.
pub const DEFAULT_VALIDITY_SECONDS: u64 = 1_800;

/// Why a proposed subject is not a usable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectError {
    /// A mission or event identifier that is empty.
    BlankIdentifier,
}

impl fmt::Display for SubjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankIdentifier => f.write_str("mission and event identifiers must be non-empty"),
        }
    }
}

impl core::error::Error for SubjectError {}

/// The thing being endorsed, identified fleet-wide.
///
/// Identity deliberately spans four parts. The upstream application's event ID
/// is a *local* identity — two boats can mint the same one for unrelated events —
/// so it cannot stand alone. Mission scopes it, revision separates successive
/// versions of the same warning, and the content hash stops two nodes signing
/// different text from being counted as agreeing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Subject {
    mission: String,
    event: String,
    revision: u32,
    content_hash: [u8; 32],
    started_at: Timestamp,
}

impl Subject {
    /// Build a subject.
    ///
    /// # Errors
    ///
    /// [`SubjectError::BlankIdentifier`] if the mission or event ID is empty.
    pub fn new(
        mission: &str,
        event: &str,
        revision: u32,
        content_hash: [u8; 32],
        started_at: Timestamp,
    ) -> Result<Self, SubjectError> {
        if mission.is_empty() || event.is_empty() {
            return Err(SubjectError::BlankIdentifier);
        }
        Ok(Self {
            mission: mission.to_string(),
            event: event.to_string(),
            revision,
            content_hash,
            started_at,
        })
    }

    /// Mission this subject belongs to.
    #[must_use]
    pub fn mission(&self) -> &str {
        &self.mission
    }

    /// Upstream event identifier, meaningful only within the mission.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// Which revision of the warning this is.
    #[must_use]
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Hash of the endorsed content.
    #[must_use]
    pub fn content_hash(&self) -> &[u8; 32] {
        &self.content_hash
    }

    /// The shared instant every deadline is measured from.
    ///
    /// Timing authority is this value, carried with the subject — not each
    /// node's local arrival time, which would give every node a different
    /// window and let a late arrival reopen a closed one.
    #[must_use]
    pub fn started_at(&self) -> Timestamp {
        self.started_at
    }

    /// When opinion collection closes.
    #[must_use]
    pub fn consultation_cutoff(&self) -> Timestamp {
        self.started_at.plus_secs(CONSULTATION_CUTOFF_SECONDS)
    }

    /// When endorsement is expected, after which collection continues but no
    /// second consultation is opened.
    #[must_use]
    pub fn endorsement_target(&self) -> Timestamp {
        self.started_at.plus_secs(ENDORSEMENT_TARGET_SECONDS)
    }

    /// Fallback expiry when the input system gives no validity.
    #[must_use]
    pub fn default_expiry(&self) -> Timestamp {
        self.started_at.plus_secs(DEFAULT_VALIDITY_SECONDS)
    }

    /// Whether two subjects are the same claim at the same revision.
    ///
    /// Equality of the whole value, spelled out so call sites read as the rule
    /// they are enforcing rather than as a struct comparison.
    #[must_use]
    pub fn same_logical_case_and_revision(&self, other: &Self) -> bool {
        self == other
    }
}

/// What a node concluded about a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The node supports the warning.
    Support,
    /// The node disputes it.
    Dispute,
    /// The node cannot tell.
    InsufficientData,
}

impl Verdict {
    /// Whether this verdict may contribute to an approval threshold.
    ///
    /// Only support can. There is no fleet verdict meaning "no danger": a
    /// dispute is recorded and shown, never counted, and the absence of
    /// approval never means safety.
    #[must_use]
    pub fn can_support_approval(self) -> bool {
        matches!(self, Self::Support)
    }
}

/// Which protocol stage an utterance belongs to.
///
/// Stages never aggregate. A first-round opinion is not a vote, and counting
/// the two together is precisely the error that would let a quorum form without
/// anyone having committed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The node's own assessment, before seeing its neighbours'.
    Independent,
    /// The single re-assessment after seeing the collected opinions.
    Consultation,
    /// A committing vote that may count toward a quorum.
    BindingSupport,
}

impl Stage {
    /// Whether utterances at this stage may count toward a quorum.
    #[must_use]
    pub fn is_binding(self) -> bool {
        matches!(self, Self::BindingSupport)
    }
}

/// One node's utterance about one subject at one stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opinion {
    author: String,
    subject: Subject,
    stage: Stage,
    verdict: Verdict,
}

impl Opinion {
    /// Record an utterance.
    #[must_use]
    pub fn new(author: &str, subject: Subject, stage: Stage, verdict: Verdict) -> Self {
        Self {
            author: author.to_string(),
            subject,
            stage,
            verdict,
        }
    }

    /// Who said it. Authorship here is a claim, not a verified signature.
    #[must_use]
    pub fn author(&self) -> &str {
        &self.author
    }

    /// What it is about.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// Which stage it belongs to.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// What was concluded.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// Whether this utterance is a committing vote.
    #[must_use]
    pub fn is_binding(&self) -> bool {
        self.stage.is_binding()
    }

    /// Whether two utterances may be counted in the same tally.
    ///
    /// Same subject, same revision, same content, same stage. Anything else is
    /// two different questions and must not be added together.
    #[must_use]
    pub fn counts_with(&self, other: &Self) -> bool {
        self.stage == other.stage && self.subject.same_logical_case_and_revision(&other.subject)
    }
}
