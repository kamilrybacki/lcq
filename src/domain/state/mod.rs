//! Pure stage transitions for one subject.
//!
//! A `Case` is the protocol's memory of a single claim: which opinions arrived
//! before the cutoff, whether the cutoff has been frozen, and who has committed
//! a binding vote. Every transition takes an injected clock, so the machine can
//! be driven deterministically and no rule reaches for a real one.
//!
//! What this does **not** do: verify a signature, check a sender's membership,
//! or decide whether a quorum is met. Authorship is taken on trust here exactly
//! as it is in [`crate::domain::quorum`]; the layers that establish it do not
//! exist yet.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::domain::contracts::{Opinion, Stage, Subject, Verdict};
use crate::domain::time::{Clock, Timestamp};

extern crate alloc;

/// Why an utterance could not be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    /// The utterance is about a different subject, revision or content.
    DifferentSubject,
    /// The stage does not belong to the phase the case is in.
    WrongStageForPhase,
    /// Consultation has been frozen; its set cannot be reopened.
    ConsultationClosed,
    /// The cutoff has not certainly passed yet.
    CutoffNotReached,
    /// The subject has certainly expired.
    Expired,
    /// The clock skew budget leaves validity genuinely unknown.
    ///
    /// The node abstains from a binding vote rather than guessing. It may still
    /// raise a local warning; refusing to vote is not deciding there is no
    /// danger.
    TimeUncertain,
    /// This author already cast a binding vote on this case.
    AlreadyVoted,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DifferentSubject => "utterance refers to a different subject",
            Self::WrongStageForPhase => "stage does not belong to the current phase",
            Self::ConsultationClosed => "consultation is closed",
            Self::CutoffNotReached => "consultation cutoff has not certainly passed",
            Self::Expired => "subject has expired",
            Self::TimeUncertain => "clock uncertainty prevents establishing validity",
            Self::AlreadyVoted => "author already cast a binding vote",
        };
        f.write_str(message)
    }
}

impl core::error::Error for TransitionError {}

/// How many times the state machine said no, and why.
///
/// A simulator that drops refusals on the floor cannot tell a fleet that agreed
/// from one whose utterances were all rejected for the same silly reason. This
/// makes every refusal show up in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransitionErrorCount {
    /// Utterances about a different subject, revision or content.
    pub different_subject: usize,
    /// Stage did not belong to the phase the case was in.
    pub wrong_stage_for_phase: usize,
    /// Arrived after the consultation set was frozen.
    pub consultation_closed: usize,
    /// Consultation could not close because the cutoff had not certainly passed.
    pub cutoff_not_reached: usize,
    /// The subject had certainly expired.
    pub expired: usize,
    /// Clock skew left validity genuinely unknown.
    pub time_uncertain: usize,
    /// The author had already cast a binding vote.
    pub already_voted: usize,
}

impl TransitionErrorCount {
    /// Tally one refusal.
    pub const fn record(&mut self, error: TransitionError) {
        let slot = match error {
            TransitionError::DifferentSubject => &mut self.different_subject,
            TransitionError::WrongStageForPhase => &mut self.wrong_stage_for_phase,
            TransitionError::ConsultationClosed => &mut self.consultation_closed,
            TransitionError::CutoffNotReached => &mut self.cutoff_not_reached,
            TransitionError::Expired => &mut self.expired,
            TransitionError::TimeUncertain => &mut self.time_uncertain,
            TransitionError::AlreadyVoted => &mut self.already_voted,
        };
        *slot += 1;
    }

    /// Every refusal, whatever the reason.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.different_subject
            + self.wrong_stage_for_phase
            + self.consultation_closed
            + self.cutoff_not_reached
            + self.expired
            + self.time_uncertain
            + self.already_voted
    }
}

/// Which part of its life a case is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Independent opinions are being collected.
    Consulting,
    /// Consultation is frozen; binding votes are being collected.
    CollectingVotes,
}

/// The protocol's memory of one claim.
#[derive(Debug, Clone)]
pub struct Case {
    subject: Subject,
    expires_at: Timestamp,
    phase: Phase,
    independent: Vec<Opinion>,
    binding: Vec<Opinion>,
    voted: BTreeSet<String>,
}

impl Case {
    /// Open a case with the subject's default validity.
    #[must_use]
    pub fn open(subject: Subject) -> Self {
        let expires_at = subject.default_expiry();
        Self::open_until(subject, expires_at)
    }

    /// Open a case with an explicit expiry from the input system.
    #[must_use]
    pub fn open_until(subject: Subject, expires_at: Timestamp) -> Self {
        Self {
            subject,
            expires_at,
            phase: Phase::Consulting,
            independent: Vec::new(),
            binding: Vec::new(),
            voted: BTreeSet::new(),
        }
    }

    /// The claim this case is about.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// Current phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// When this case stops being actionable.
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Opinions collected before the cutoff was frozen.
    pub fn independent_opinions(&self) -> impl Iterator<Item = &Opinion> {
        self.independent.iter()
    }

    /// Authors whose binding vote supports the claim.
    ///
    /// Only support is retained: a disputing vote is recorded as having been
    /// cast, so the author cannot vote again, but it never counts toward a
    /// threshold.
    pub fn binding_supporters(&self) -> impl Iterator<Item = &str> {
        self.binding
            .iter()
            .filter(|o| o.verdict().can_support_approval())
            .map(Opinion::author)
    }

    /// Freeze the consultation set and move to collecting votes.
    ///
    /// # Errors
    ///
    /// [`TransitionError::CutoffNotReached`] while the cutoff is not certainly
    /// past, and [`TransitionError::ConsultationClosed`] if already frozen —
    /// there is exactly one logical consultation per case.
    pub fn close_consultation(&mut self, clock: &impl Clock) -> Result<(), TransitionError> {
        if self.phase == Phase::CollectingVotes {
            return Err(TransitionError::ConsultationClosed);
        }
        if !clock.certainly_after(self.subject.consultation_cutoff()) {
            return Err(TransitionError::CutoffNotReached);
        }
        self.phase = Phase::CollectingVotes;
        Ok(())
    }

    /// Admit an utterance, or say why it cannot be admitted.
    ///
    /// # Errors
    ///
    /// See [`TransitionError`]. Expiry is checked first: it overrides every
    /// other deadline, so an expired case admits nothing regardless of phase.
    pub fn accept(&mut self, opinion: Opinion, clock: &impl Clock) -> Result<(), TransitionError> {
        if !opinion
            .subject()
            .same_logical_case_and_revision(&self.subject)
        {
            return Err(TransitionError::DifferentSubject);
        }
        // Expiry first, always. A case past its validity is not a case.
        if clock.certainly_after(self.expires_at) {
            return Err(TransitionError::Expired);
        }

        match (self.phase, opinion.stage()) {
            (Phase::Consulting, Stage::Independent | Stage::Consultation) => {
                self.independent.push(opinion);
                Ok(())
            }
            (Phase::Consulting, Stage::BindingSupport) => Err(TransitionError::WrongStageForPhase),
            (Phase::CollectingVotes, Stage::Independent | Stage::Consultation) => {
                // The frozen set is frozen. A late opinion belongs in a log, not
                // in a closed consultation.
                Err(TransitionError::ConsultationClosed)
            }
            (Phase::CollectingVotes, Stage::BindingSupport) => {
                // A binding vote is a commitment, so it needs certainty about
                // validity, not merely the absence of proven expiry.
                if clock.uncertain_about(self.expires_at) {
                    return Err(TransitionError::TimeUncertain);
                }
                if !self.voted.insert(opinion.author().to_string()) {
                    return Err(TransitionError::AlreadyVoted);
                }
                let counts = opinion.verdict() == Verdict::Support;
                if counts || opinion.verdict() != Verdict::Support {
                    self.binding.push(opinion);
                }
                Ok(())
            }
        }
    }
}
