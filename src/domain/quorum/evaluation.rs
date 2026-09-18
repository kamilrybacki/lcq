//! Deciding whether a set of endorsements clears both thresholds.

use alloc::collections::BTreeSet;
use alloc::string::String;
use core::fmt;

use crate::domain::quorum::policy::Policy;

extern crate alloc;

/// Why a set of endorsements could not be evaluated at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationError {
    /// A signer that the manifest does not name.
    ///
    /// Rejected rather than ignored. Silently dropping an unknown signer would
    /// let a caller mix a stale manifest with a fresh endorsement set and get a
    /// plausible-looking verdict computed against the wrong fleet.
    UnknownSigner,
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSigner => f.write_str("signer is not a member of this fleet"),
        }
    }
}

impl core::error::Error for EvaluationError {}

/// The outcome of one evaluation, with both thresholds visible separately.
///
/// The two are reported apart on purpose: "not enough members" and "not enough
/// weight" are different operational situations and call for different
/// responses, so collapsing them into one boolean would throw away the part an
/// operator needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuorumResult {
    signer_count: usize,
    support_weight: u64,
    count_met: bool,
    weight_met: bool,
}

impl QuorumResult {
    /// Distinct members that endorsed.
    #[must_use]
    pub fn signer_count(&self) -> usize {
        self.signer_count
    }

    /// Manifest weight behind the endorsement.
    #[must_use]
    pub fn support_weight(&self) -> u64 {
        self.support_weight
    }

    /// Whether the member-count threshold is met.
    #[must_use]
    pub fn count_met(&self) -> bool {
        self.count_met
    }

    /// Whether the weight threshold is met.
    #[must_use]
    pub fn weight_met(&self) -> bool {
        self.weight_met
    }

    /// Whether both thresholds are met.
    ///
    /// Approval means "enough of the fleet endorsed this". It does **not** mean
    /// the claim is true, and there is no opposite verdict: this protocol
    /// carries positive endorsements only, so a false here is "not endorsed",
    /// never "all clear".
    #[must_use]
    pub fn approved(&self) -> bool {
        self.count_met && self.weight_met
    }
}

/// Evaluate endorsements from `signers` against `policy`.
///
/// Duplicates collapse: a member counts once however many times its ID appears,
/// so a replayed frame cannot inflate support.
///
/// # Not authentication
///
/// Every ID passed here is taken on trust. This function cannot check a
/// signature, a message scope, a revision or a replay window; a caller that
/// feeds it unvalidated network input gets arithmetic with no security meaning.
///
/// # Errors
///
/// [`EvaluationError::UnknownSigner`] if any ID is absent from the manifest.
pub fn evaluate(
    policy: &Policy,
    signers: impl IntoIterator<Item = String>,
) -> Result<QuorumResult, EvaluationError> {
    let unique: BTreeSet<String> = signers.into_iter().collect();

    let mut support_weight: u64 = 0;
    for signer in &unique {
        let weight = policy
            .weight_of(signer)
            .ok_or(EvaluationError::UnknownSigner)?;
        support_weight += u64::from(weight);
    }

    Ok(QuorumResult {
        signer_count: unique.len(),
        support_weight,
        count_met: unique.len() >= policy.min_signers(),
        // Integer arithmetic throughout: 3w > 2T rather than w/T > 2/3, so no
        // rounding decides a safety threshold.
        weight_met: 3 * support_weight > 2 * policy.total_weight(),
    })
}
