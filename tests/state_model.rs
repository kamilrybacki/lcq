//! The state machine against a reference model, under random histories.
//!
//! The unit tests in `tests/state.rs` each pin one rule. This drives `Case`
//! through arbitrary sequences of utterances and closings at arbitrary clock
//! readings -- including clocks that jump backwards, since the API accepts any
//! `Clock` -- and checks every single result against a model written from the
//! rules as documented. A divergence is either a bug in the machine or a rule
//! the documentation does not state; both are worth knowing.

use std::collections::BTreeSet;

use lcq::domain::contracts::{
    CONSULTATION_CUTOFF_SECONDS, DEFAULT_VALIDITY_SECONDS, Opinion, Stage, Subject, Verdict,
};
use lcq::domain::state::{Case, Phase, TransitionError};
use lcq::domain::time::{FixedClock, MAX_CLOCK_SKEW_SECONDS, Timestamp};
use proptest::prelude::*;

const MEMBERS: usize = 6;

#[derive(Debug, Clone)]
enum Op {
    Accept {
        author: usize,
        stage: Stage,
        verdict: Verdict,
        at: u64,
    },
    Close {
        at: u64,
    },
}

fn stage() -> impl Strategy<Value = Stage> {
    prop_oneof![
        Just(Stage::Independent),
        Just(Stage::Consultation),
        Just(Stage::BindingSupport),
    ]
}

fn verdict() -> impl Strategy<Value = Verdict> {
    prop_oneof![
        Just(Verdict::Support),
        Just(Verdict::Dispute),
        Just(Verdict::InsufficientData),
    ]
}

/// Clock readings that straddle every boundary the rules care about: the
/// cutoff, the cutoff plus skew, the expiry minus skew, the expiry plus skew.
fn instant() -> impl Strategy<Value = u64> {
    prop_oneof![
        0..CONSULTATION_CUTOFF_SECONDS,
        CONSULTATION_CUTOFF_SECONDS..CONSULTATION_CUTOFF_SECONDS + 2 * MAX_CLOCK_SKEW_SECONDS,
        CONSULTATION_CUTOFF_SECONDS + 2 * MAX_CLOCK_SKEW_SECONDS
            ..DEFAULT_VALIDITY_SECONDS - MAX_CLOCK_SKEW_SECONDS,
        DEFAULT_VALIDITY_SECONDS - MAX_CLOCK_SKEW_SECONDS
            ..DEFAULT_VALIDITY_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1,
        DEFAULT_VALIDITY_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1..DEFAULT_VALIDITY_SECONDS * 2,
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..MEMBERS, stage(), verdict(), instant()).prop_map(|(author, stage, verdict, at)| {
            Op::Accept { author, stage, verdict, at }
        }),
        1 => instant().prop_map(|at| Op::Close { at }),
    ]
}

/// The rules as documented, kept as plainly as possible.
#[derive(Debug, Default)]
struct Model {
    closed: bool,
    voted: BTreeSet<usize>,
    supporters: BTreeSet<usize>,
    opinions: BTreeSet<(usize, u8)>,
}

impl Model {
    fn cutoff() -> u64 {
        CONSULTATION_CUTOFF_SECONDS
    }

    fn expiry() -> u64 {
        DEFAULT_VALIDITY_SECONDS
    }

    fn certainly_after(at: u64, deadline: u64) -> bool {
        at > deadline + MAX_CLOCK_SKEW_SECONDS
    }

    fn certainly_before(at: u64, deadline: u64) -> bool {
        at + MAX_CLOCK_SKEW_SECONDS < deadline
    }

    fn accept(
        &mut self,
        author: usize,
        stage: Stage,
        verdict: Verdict,
        at: u64,
    ) -> Result<(), TransitionError> {
        // Expiry first, always.
        if Self::certainly_after(at, Self::expiry()) {
            return Err(TransitionError::Expired);
        }
        match (self.closed, stage) {
            (false, Stage::Independent | Stage::Consultation) => {
                let code = if stage == Stage::Independent { 1 } else { 2 };
                self.opinions.insert((author, code));
                Ok(())
            }
            (false, Stage::BindingSupport) => Err(TransitionError::WrongStageForPhase),
            (true, Stage::Independent | Stage::Consultation) => {
                Err(TransitionError::ConsultationClosed)
            }
            (true, Stage::BindingSupport) => {
                let uncertain = !Self::certainly_after(at, Self::expiry())
                    && !Self::certainly_before(at, Self::expiry());
                if uncertain {
                    return Err(TransitionError::TimeUncertain);
                }
                if !self.voted.insert(author) {
                    return Err(TransitionError::AlreadyVoted);
                }
                if verdict == Verdict::Support {
                    self.supporters.insert(author);
                }
                Ok(())
            }
        }
    }

    fn close(&mut self, at: u64) -> Result<(), TransitionError> {
        if self.closed {
            return Err(TransitionError::ConsultationClosed);
        }
        if !Self::certainly_after(at, Self::cutoff()) {
            return Err(TransitionError::CutoffNotReached);
        }
        self.closed = true;
        Ok(())
    }
}

fn subject() -> Subject {
    Subject::new("m", "e", 0, [7; 32], Timestamp::from_secs(0)).expect("subject")
}

fn observed_supporters(case: &Case) -> BTreeSet<usize> {
    case.binding_supporters()
        .filter_map(|id| id.strip_prefix('n')?.parse().ok())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn the_machine_agrees_with_the_model_on_every_step(ops in prop::collection::vec(op(), 1..40)) {
        let s = subject();
        let mut case = Case::open(s.clone());
        let mut model = Model::default();

        for (step, op) in ops.iter().enumerate() {
            match op {
                Op::Accept { author, stage, verdict, at } => {
                    let clock = FixedClock::new(Timestamp::from_secs(*at));
                    let opinion = Opinion::new(&format!("n{author}"), s.clone(), *stage, *verdict);
                    let got = case.accept(opinion, &clock);
                    let want = model.accept(*author, *stage, *verdict, *at);
                    prop_assert_eq!(got, want, "step {}: {:?}", step, op);
                }
                Op::Close { at } => {
                    let clock = FixedClock::new(Timestamp::from_secs(*at));
                    let got = case.close_consultation(&clock);
                    let want = model.close(*at);
                    prop_assert_eq!(got, want, "step {}: {:?}", step, op);
                }
            }

            // Observable state, after every step.
            prop_assert_eq!(
                case.phase() == Phase::CollectingVotes,
                model.closed,
                "step {}: phase", step
            );
            prop_assert_eq!(
                observed_supporters(&case),
                model.supporters.clone(),
                "step {}: supporters", step
            );
            prop_assert_eq!(
                case.independent_opinions().count(),
                model.opinions.len(),
                "step {}: opinions", step
            );
        }
    }

    #[test]
    fn a_member_is_never_counted_twice_whatever_the_history(ops in prop::collection::vec(op(), 1..60)) {
        // The safety property on its own, stated without the model: however the
        // history goes, the supporters are distinct members, each of whom had a
        // supporting binding vote admitted, and nobody who disputed is among
        // them.
        let s = subject();
        let mut case = Case::open(s.clone());
        let mut admitted_support: BTreeSet<usize> = BTreeSet::new();
        let mut admitted_other: BTreeSet<usize> = BTreeSet::new();

        for op in &ops {
            match op {
                Op::Accept { author, stage, verdict, at } => {
                    let clock = FixedClock::new(Timestamp::from_secs(*at));
                    let opinion = Opinion::new(&format!("n{author}"), s.clone(), *stage, *verdict);
                    if case.accept(opinion, &clock).is_ok() && *stage == Stage::BindingSupport {
                        if *verdict == Verdict::Support {
                            admitted_support.insert(*author);
                        } else {
                            admitted_other.insert(*author);
                        }
                    }
                }
                Op::Close { at } => {
                    let clock = FixedClock::new(Timestamp::from_secs(*at));
                    let _ = case.close_consultation(&clock);
                }
            }
        }

        let supporters: Vec<usize> = case
            .binding_supporters()
            .filter_map(|id| id.strip_prefix('n')?.parse().ok())
            .collect();
        let distinct: BTreeSet<usize> = supporters.iter().copied().collect();
        prop_assert_eq!(supporters.len(), distinct.len(), "a member counted twice");
        prop_assert_eq!(distinct.clone(), admitted_support, "supporters are exactly the admitted supports");
        prop_assert!(distinct.is_disjoint(&admitted_other), "a dissenter among the supporters");
    }
}
