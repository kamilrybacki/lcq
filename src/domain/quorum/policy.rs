//! The fleet manifest and the two thresholds derived from it.

use alloc::collections::BTreeMap;
use core::fmt;

extern crate alloc;

/// Why a proposed manifest is not a usable policy.
///
/// Rejection is loud and typed rather than a silently corrected default: a
/// manifest that does not say what its author meant is a safety problem, not a
/// formatting problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// No members. There is no denominator to reason about.
    EmptyFleet,
    /// A member ID that is empty.
    BlankMemberId,
    /// The same member ID appears twice, leaving the denominator ambiguous.
    DuplicateMemberId,
    /// A competence outside `1..=100`: zero is not a member, and the scale has
    /// a ceiling so that no manifest can invent an unbounded member.
    CompetenceOutOfRange,
    /// Most competent member exceeds `max_competence_ratio` times the least.
    CompetenceRatioExceeded,
    /// Fault budget outside `[0, 10000)` basis points.
    FaultBudgetOutOfRange,
    /// A competence ratio cap below one, which no manifest can satisfy.
    NonPositiveCompetenceRatio,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyFleet => "fleet must not be empty",
            Self::BlankMemberId => "member IDs must be non-empty",
            Self::DuplicateMemberId => "member IDs must be unique",
            Self::CompetenceOutOfRange => "competence must be in 1..=100",
            Self::CompetenceRatioExceeded => "competence ratio exceeds policy cap",
            Self::FaultBudgetOutOfRange => "fault budget must be in [0, 10000) basis points",
            Self::NonPositiveCompetenceRatio => "competence ratio cap must be at least 1",
        };
        f.write_str(message)
    }
}

impl core::error::Error for PolicyError {}

/// What a manifest says one member's judgment is worth, on a fixed scale of
/// 1 to 100 (D24).
///
/// # The protocol does not know why
///
/// This is deliberately a bare number. Whoever assembles the manifest decides
/// what competence means for its fleet and normalises it onto this scale
/// before it ever reaches the protocol: the square root of a language model's
/// parameter count adjusted for quantisation, a benchmark score, an
/// instrument's calibration record, the agreement history of a deterministic
/// calculator, or a figure a human wrote down. All of them arrive here the
/// same way, and nothing below this type can tell them apart -- which is what
/// lets one fleet mix members that decide by wholly different means.
///
/// # Not self-declared
///
/// A member never states its own competence on the air. The manifest is the
/// only source, so competence is exactly as trustworthy as the manifest is
/// (`THREAT-MODEL.md` F4, still open): a member that could declare its own
/// would simply declare 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Competence(u8);

impl Competence {
    /// The top of the scale, and the value a fleet uses when every member
    /// counts the same.
    pub const FULL: Self = Self(100);
    /// The top of the scale as a number.
    pub const SCALE: u8 = 100;

    /// A competence from a normalised score.
    ///
    /// # Errors
    ///
    /// [`PolicyError::CompetenceOutOfRange`] outside `1..=100`.
    pub const fn try_new(value: u8) -> Result<Self, PolicyError> {
        if value == 0 || value > Self::SCALE {
            return Err(PolicyError::CompetenceOutOfRange);
        }
        Ok(Self(value))
    }

    /// The score, `1..=100`.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    /// The band a service can normalise into and be sure the manifest will
    /// satisfy `max_ratio` whatever the other members score.
    ///
    /// The ratio cap is a property of the whole fleet, so a service that
    /// scores members one at a time cannot otherwise know whether its
    /// manifest is legal until it assembles it. Mapping every member into
    /// this band makes it legal by construction: at a cap of 3 the band is
    /// 34 to 100, and 100 is less than three times 34.
    #[must_use]
    pub const fn band(max_ratio: u32) -> (u8, u8) {
        if max_ratio == 0 {
            return (Self::SCALE, Self::SCALE);
        }
        let floor = (Self::SCALE as u32).div_ceil(max_ratio);
        let floor = if floor == 0 { 1 } else { floor };
        #[allow(clippy::cast_possible_truncation)]
        (floor as u8, Self::SCALE)
    }
}

impl fmt::Display for Competence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Default share of members the protocol tolerates being compromised, in basis
/// points. 4000 bp = 40 %, the figure the fleet design was agreed against.
pub const DEFAULT_BYZANTINE_BPS: u32 = 4_000;

/// Default cap on how much more competent the highest member may be than the
/// lowest.
pub const DEFAULT_MAX_COMPETENCE_RATIO: u32 = 3;

/// An immutable fleet manifest: who may endorse, and how much each one counts.
///
/// Membership and competences are fixed for the whole evaluation regardless of
/// who is currently reachable. A member that cannot be heard still counts in
/// the denominator.
///
/// # What competence is here
///
/// A fleet may be made of members that reach a verdict by entirely different
/// means -- a language model on one vessel, a deterministic calculation on
/// another, an instrument reading on a third. The manifest says only how much
/// each one's judgment is worth, on the fixed scale of [`Competence`]. The
/// normalisation happens wherever the manifest is assembled; the protocol
/// never learns what was normalised.
///
/// # Not a security boundary
///
/// A `Policy` says who *may* endorse. It cannot tell whether a given endorsement
/// really came from that member. Authentication belongs to a layer that must run
/// before any signer ID reaches this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    competences: BTreeMap<alloc::string::String, Competence>,
    byzantine_bps: u32,
    max_competence_ratio: u32,
    total_competence: u64,
}

impl Policy {
    /// Build a policy with the agreed defaults: 40 % fault budget, 3:1
    /// competence cap.
    ///
    /// Competences arrive as plain scores and are validated here, which is the
    /// boundary the rest of the domain relies on: past this point a competence
    /// is known to be on the scale.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] if the manifest is empty, has blank or duplicate
    /// IDs, has a competence off the scale, or breaches the ratio cap.
    pub fn new(
        members: impl IntoIterator<Item = (alloc::string::String, u8)>,
    ) -> Result<Self, PolicyError> {
        Self::with_settings(members, DEFAULT_BYZANTINE_BPS, DEFAULT_MAX_COMPETENCE_RATIO)
    }

    /// Build a policy with an explicit fault budget and competence cap.
    ///
    /// # Errors
    ///
    /// As [`Policy::new`], plus [`PolicyError::FaultBudgetOutOfRange`] and
    /// [`PolicyError::NonPositiveCompetenceRatio`].
    pub fn with_settings(
        members: impl IntoIterator<Item = (alloc::string::String, u8)>,
        byzantine_bps: u32,
        max_competence_ratio: u32,
    ) -> Result<Self, PolicyError> {
        if byzantine_bps >= 10_000 {
            return Err(PolicyError::FaultBudgetOutOfRange);
        }
        if max_competence_ratio < 1 {
            return Err(PolicyError::NonPositiveCompetenceRatio);
        }

        let mut competences = BTreeMap::new();
        for (id, score) in members {
            if id.is_empty() {
                return Err(PolicyError::BlankMemberId);
            }
            let competence = Competence::try_new(score)?;
            if competences.insert(id, competence).is_some() {
                return Err(PolicyError::DuplicateMemberId);
            }
        }
        if competences.is_empty() {
            return Err(PolicyError::EmptyFleet);
        }

        // Folded rather than unwrapped: emptiness was already rejected above, so
        // an `expect` here would be a panic guarding a condition that cannot
        // occur -- and a panic in manifest validation is not an acceptable
        // failure mode for a node at sea.
        let (highest, lowest) = competences.values().fold((0u8, u8::MAX), |(hi, lo), c| {
            (hi.max(c.value()), lo.min(c.value()))
        });
        if u64::from(highest) > u64::from(max_competence_ratio) * u64::from(lowest) {
            return Err(PolicyError::CompetenceRatioExceeded);
        }

        let total_competence = competences.values().map(|c| u64::from(c.value())).sum();
        Ok(Self {
            competences,
            byzantine_bps,
            max_competence_ratio,
            total_competence,
        })
    }

    /// Number of members in the manifest.
    #[must_use]
    pub fn size(&self) -> usize {
        self.competences.len()
    }

    /// Members the fault budget allows to be compromised, rounded down.
    #[must_use]
    pub fn max_faulty(&self) -> usize {
        // Widened before multiplying: a large fleet times 9999 bp overflows u32.
        // The quotient is at most the fleet size, so the narrowing cannot lose
        // information -- but it is saturated rather than unwrapped, because a
        // panic inside a safety threshold is the worst possible failure mode.
        let budget = u64::from(self.byzantine_bps) * self.size() as u64 / 10_000;
        usize::try_from(budget).unwrap_or(self.size())
    }

    /// Smallest signer count whose quorums all intersect in more than
    /// [`Policy::max_faulty`] members.
    #[must_use]
    pub fn min_signers(&self) -> usize {
        // `midpoint` rather than `(a + b) / 2`: the sum of a large fleet and its
        // fault budget can overflow, and an overflow here would silently produce
        // a threshold far below the one the fleet agreed to.
        usize::midpoint(self.size(), self.max_faulty()) + 1
    }

    /// Sum of all member competences: the denominator of the competence
    /// threshold.
    #[must_use]
    pub fn total_competence(&self) -> u64 {
        self.total_competence
    }

    /// Competence of one member, or `None` if the manifest does not name it.
    #[must_use]
    pub fn competence_of(&self, member: &str) -> Option<Competence> {
        self.competences.get(member).copied()
    }

    /// Member IDs in a stable order.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.competences.keys().map(alloc::string::String::as_str)
    }

    /// Configured fault budget in basis points.
    #[must_use]
    pub fn byzantine_bps(&self) -> u32 {
        self.byzantine_bps
    }

    /// Configured cap on the highest-to-lowest competence ratio.
    #[must_use]
    pub fn max_competence_ratio(&self) -> u32 {
        self.max_competence_ratio
    }
}
