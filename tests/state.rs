//! Stage transitions: the acceptance list from the roadmap, as tests.

use lorai::domain::contracts::{Opinion, Stage, Subject, Verdict};
use lorai::domain::state::{Case, Phase, TransitionError};
use lorai::domain::time::{FixedClock, Timestamp};

fn subject_at(start: u64) -> Subject {
    Subject::new("m1", "e1", 0, [7; 32], Timestamp::from_secs(start)).expect("valid")
}

fn clock(at: u64) -> FixedClock {
    FixedClock::new(Timestamp::from_secs(at))
}

fn opinion(author: &str, s: &Subject, stage: Stage, v: Verdict) -> Opinion {
    Opinion::new(author, s.clone(), stage, v)
}

#[test]
fn a_healthy_unanimous_trace_reaches_endorsement() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());

    for node in ["a", "b", "c", "d", "e"] {
        case.accept(
            opinion(node, &s, Stage::Independent, Verdict::Support),
            &clock(1_010),
        )
        .expect("within the window");
    }
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    assert_eq!(case.phase(), Phase::CollectingVotes);

    for node in ["a", "b", "c", "d"] {
        case.accept(
            opinion(node, &s, Stage::BindingSupport, Verdict::Support),
            &clock(1_410),
        )
        .expect("votes accepted");
    }
    assert_eq!(case.binding_supporters().count(), 4);
}

#[test]
fn opinions_do_not_count_as_binding_votes() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    for node in ["a", "b", "c", "d", "e"] {
        case.accept(
            opinion(node, &s, Stage::Independent, Verdict::Support),
            &clock(1_010),
        )
        .expect("within the window");
    }
    assert_eq!(case.binding_supporters().count(), 0);
}

#[test]
fn stage_isolation_a_binding_vote_before_consultation_closes_is_refused() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    let err = case
        .accept(
            opinion("a", &s, Stage::BindingSupport, Verdict::Support),
            &clock(1_010),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::WrongStageForPhase);
}

#[test]
fn the_cutoff_freezes_once_and_late_opinions_do_not_reopen_it() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.accept(
        opinion("a", &s, Stage::Independent, Verdict::Support),
        &clock(1_010),
    )
    .expect("in window");
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");

    let err = case
        .accept(
            opinion("b", &s, Stage::Independent, Verdict::Support),
            &clock(1_410),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::ConsultationClosed);
    assert_eq!(
        case.independent_opinions().count(),
        1,
        "closed set is frozen"
    );
}

#[test]
fn only_one_logical_consultation_ever_happens() {
    let s = subject_at(1_000);
    let mut case = Case::open(s);
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    let err = case.close_consultation(&clock(1_500)).unwrap_err();
    assert_eq!(err, TransitionError::ConsultationClosed);
}

#[test]
fn consultation_cannot_close_before_its_cutoff_is_certain() {
    let s = subject_at(1_000);
    let mut case = Case::open(s);
    // 1_299 is before the cutoff; 1_310 is inside the skew band.
    assert_eq!(
        case.close_consultation(&clock(1_299)).unwrap_err(),
        TransitionError::CutoffNotReached
    );
    assert_eq!(
        case.close_consultation(&clock(1_310)).unwrap_err(),
        TransitionError::CutoffNotReached
    );
}

#[test]
fn expiry_takes_precedence_over_every_other_deadline() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");

    // Past default expiry plus the skew budget: nothing more may be accepted.
    let err = case
        .accept(
            opinion("a", &s, Stage::BindingSupport, Verdict::Support),
            &clock(3_000),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::Expired);
}

#[test]
fn a_node_uncertain_about_expiry_does_not_cast_a_binding_vote() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    // Expiry is 2_800; inside the skew band the node must abstain.
    let err = case
        .accept(
            opinion("a", &s, Stage::BindingSupport, Verdict::Support),
            &clock(2_790),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::TimeUncertain);
}

#[test]
fn a_case_refuses_utterances_about_another_subject() {
    let s = subject_at(1_000);
    let other = Subject::new("m1", "e1", 1, [7; 32], Timestamp::from_secs(1_000)).expect("valid");
    let mut case = Case::open(s);
    let err = case
        .accept(
            opinion("a", &other, Stage::Independent, Verdict::Support),
            &clock(1_010),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::DifferentSubject);
}

#[test]
fn a_conflict_on_one_subject_does_not_veto_another() {
    let disputed = subject_at(1_000);
    let unrelated = Subject::new("m1", "e2", 0, [9; 32], Timestamp::from_secs(1_000)).expect("ok");

    let mut a = Case::open(disputed.clone());
    a.accept(
        opinion("x", &disputed, Stage::Independent, Verdict::Dispute),
        &clock(1_010),
    )
    .expect("dispute recorded");

    let mut b = Case::open(unrelated.clone());
    b.accept(
        opinion("x", &unrelated, Stage::Independent, Verdict::Support),
        &clock(1_010),
    )
    .expect("unaffected by the other case");
    assert_eq!(b.independent_opinions().count(), 1);
}

#[test]
fn a_node_votes_at_most_once_per_case() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    case.accept(
        opinion("a", &s, Stage::BindingSupport, Verdict::Support),
        &clock(1_410),
    )
    .expect("first vote");
    let err = case
        .accept(
            opinion("a", &s, Stage::BindingSupport, Verdict::Support),
            &clock(1_420),
        )
        .unwrap_err();
    assert_eq!(err, TransitionError::AlreadyVoted);
    assert_eq!(case.binding_supporters().count(), 1);
}

#[test]
fn only_support_is_retained_as_a_binding_supporter() {
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    case.accept(
        opinion("a", &s, Stage::BindingSupport, Verdict::Dispute),
        &clock(1_410),
    )
    .expect("recorded");
    assert_eq!(case.binding_supporters().count(), 0, "dispute never counts");
}

#[test]
fn a_late_node_may_still_record_but_not_fabricate_a_closed_phase() {
    // Roadmap: a late node forwards valid messages but does not fabricate a
    // completed independent phase.
    let s = subject_at(1_000);
    let mut case = Case::open(s.clone());
    case.close_consultation(&clock(1_400))
        .expect("cutoff passed");
    assert_eq!(case.independent_opinions().count(), 0);
    assert_eq!(case.phase(), Phase::CollectingVotes);
}
