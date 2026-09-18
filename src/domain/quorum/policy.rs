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
    /// A weight of zero or less: a member that cannot contribute is not a member.
    NonPositiveWeight,
    /// Heaviest member exceeds `max_weight_ratio` times the lightest.
    WeightRatioExceeded,
    /// Fault budget outside `[0, 10000)` basis points.
    FaultBudgetOutOfRange,
    /// A weight ratio cap below one, which no manifest can satisfy.
    NonPositiveWeightRatio,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyFleet => "fleet must not be empty",
            Self::BlankMemberId => "member IDs must be non-empty",
            Self::DuplicateMemberId => "member IDs must be unique",
            Self::NonPositiveWeight => "weights must be positive",
            Self::WeightRatioExceeded => "weight ratio exceeds policy cap",
            Self::FaultBudgetOutOfRange => "fault budget must be in [0, 10000) basis points",
            Self::NonPositiveWeightRatio => "weight ratio cap must be at least 1",
        };
        f.write_str(message)
    }
}

impl core::error::Error for PolicyError {}

/// Default share of members the protocol tolerates being compromised, in basis
/// points. 4000 bp = 40 %, the figure the fleet design was agreed against.
pub const DEFAULT_BYZANTINE_BPS: u32 = 4_000;

/// Default cap on how much heavier the largest member may be than the smallest.
pub const DEFAULT_MAX_WEIGHT_RATIO: u32 = 3;

/// An immutable fleet manifest: who may endorse, and how much each one counts.
///
/// Membership and weights are fixed for the whole evaluation regardless of who
/// is currently reachable. A member that cannot be heard still counts in the
/// denominator.
///
/// # Not a security boundary
///
/// A `Policy` says who *may* endorse. It cannot tell whether a given endorsement
/// really came from that member. Authentication belongs to a layer that must run
/// before any signer ID reaches this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    weights: BTreeMap<alloc::string::String, u32>,
    byzantine_bps: u32,
    max_weight_ratio: u32,
    total_weight: u64,
}

impl Policy {
    /// Build a policy with the agreed defaults: 40 % fault budget, 3:1 weight cap.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] if the manifest is empty, has blank or duplicate
    /// IDs, has a non-positive weight, or breaches the weight ratio cap.
    pub fn new(
        members: impl IntoIterator<Item = (alloc::string::String, u32)>,
    ) -> Result<Self, PolicyError> {
        Self::with_settings(members, DEFAULT_BYZANTINE_BPS, DEFAULT_MAX_WEIGHT_RATIO)
    }

    /// Build a policy with an explicit fault budget and weight cap.
    ///
    /// # Errors
    ///
    /// As [`Policy::new`], plus [`PolicyError::FaultBudgetOutOfRange`] and
    /// [`PolicyError::NonPositiveWeightRatio`].
    pub fn with_settings(
        members: impl IntoIterator<Item = (alloc::string::String, u32)>,
        byzantine_bps: u32,
        max_weight_ratio: u32,
    ) -> Result<Self, PolicyError> {
        if byzantine_bps >= 10_000 {
            return Err(PolicyError::FaultBudgetOutOfRange);
        }
        if max_weight_ratio < 1 {
            return Err(PolicyError::NonPositiveWeightRatio);
        }

        let mut weights = BTreeMap::new();
        for (id, weight) in members {
            if id.is_empty() {
                return Err(PolicyError::BlankMemberId);
            }
            if weight == 0 {
                return Err(PolicyError::NonPositiveWeight);
            }
            if weights.insert(id, weight).is_some() {
                return Err(PolicyError::DuplicateMemberId);
            }
        }
        if weights.is_empty() {
            return Err(PolicyError::EmptyFleet);
        }

        // Folded rather than unwrapped: emptiness was already rejected above, so
        // an `expect` here would be a panic guarding a condition that cannot
        // occur -- and a panic in manifest validation is not an acceptable
        // failure mode for a node at sea.
        let (heaviest, lightest) = weights
            .values()
            .fold((0u32, u32::MAX), |(hi, lo), w| (hi.max(*w), lo.min(*w)));
        if u64::from(heaviest) > u64::from(max_weight_ratio) * u64::from(lightest) {
            return Err(PolicyError::WeightRatioExceeded);
        }

        let total_weight = weights.values().map(|w| u64::from(*w)).sum();
        Ok(Self {
            weights,
            byzantine_bps,
            max_weight_ratio,
            total_weight,
        })
    }

    /// Number of members in the manifest.
    #[must_use]
    pub fn size(&self) -> usize {
        self.weights.len()
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

    /// Sum of all member weights: the denominator of the weight threshold.
    #[must_use]
    pub fn total_weight(&self) -> u64 {
        self.total_weight
    }

    /// Weight of one member, or `None` if the manifest does not name it.
    #[must_use]
    pub fn weight_of(&self, member: &str) -> Option<u32> {
        self.weights.get(member).copied()
    }

    /// Member IDs in a stable order.
    pub fn members(&self) -> impl Iterator<Item = &str> {
        self.weights.keys().map(alloc::string::String::as_str)
    }

    /// Configured fault budget in basis points.
    #[must_use]
    pub fn byzantine_bps(&self) -> u32 {
        self.byzantine_bps
    }

    /// Configured cap on the heaviest-to-lightest weight ratio.
    #[must_use]
    pub fn max_weight_ratio(&self) -> u32 {
        self.max_weight_ratio
    }
}
