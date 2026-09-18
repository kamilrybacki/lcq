//! Subject identity, opinions and the line between an opinion and a vote.

use lorai::domain::contracts::{Opinion, Stage, Subject, SubjectError, Verdict};
use lorai::domain::time::Timestamp;

fn subject() -> Subject {
    Subject::new(
        "mission-1",
        "event-7",
        0,
        [0xAB; 32],
        Timestamp::from_secs(1_000),
    )
    .expect("valid subject")
}

#[test]
fn subject_identity_spans_mission_event_revision_and_content() {
    // Morsik's Event.id is a LOCAL identity. Two boats can mint the same one for
    // different events, so it cannot stand alone as a fleet-wide subject.
    let a = subject();
    let b = Subject::new(
        "mission-2",
        "event-7",
        0,
        [0xAB; 32],
        Timestamp::from_secs(1_000),
    )
    .expect("valid subject");
    assert_ne!(
        a, b,
        "same event id in a different mission is a different subject"
    );
}

#[test]
fn a_revision_is_a_different_subject() {
    let a = subject();
    let b = Subject::new(
        "mission-1",
        "event-7",
        1,
        [0xAB; 32],
        Timestamp::from_secs(1_000),
    )
    .expect("valid subject");
    assert_ne!(a, b);
    assert!(!a.same_logical_case_and_revision(&b));
}

#[test]
fn a_different_content_hash_is_a_different_subject() {
    // Endorsements must never aggregate across content. Two nodes signing
    // different text are not supporting the same claim.
    let a = subject();
    let b = Subject::new(
        "mission-1",
        "event-7",
        0,
        [0xCD; 32],
        Timestamp::from_secs(1_000),
    )
    .expect("valid subject");
    assert_ne!(a, b);
    assert!(!a.same_logical_case_and_revision(&b));
}

#[test]
fn the_same_case_and_revision_is_recognised() {
    assert!(subject().same_logical_case_and_revision(&subject()));
}

#[test]
fn blank_identifiers_are_rejected() {
    assert_eq!(
        Subject::new("", "event-7", 0, [0; 32], Timestamp::from_secs(1)).unwrap_err(),
        SubjectError::BlankIdentifier
    );
    assert_eq!(
        Subject::new("mission-1", "", 0, [0; 32], Timestamp::from_secs(1)).unwrap_err(),
        SubjectError::BlankIdentifier
    );
}

#[test]
fn deadlines_are_derived_from_the_shared_start() {
    let s = subject();
    // Timing authority is the shared evaluation_started_at, not each node's arrival.
    assert_eq!(s.consultation_cutoff().as_secs(), 1_000 + 300);
    assert_eq!(s.endorsement_target().as_secs(), 1_000 + 600);
    assert_eq!(s.default_expiry().as_secs(), 1_000 + 1_800);
}

#[test]
fn only_support_can_contribute_to_approval() {
    // There is no fleet verdict meaning "no danger". Dispute and insufficient
    // data are recorded, never counted toward a threshold.
    assert!(Verdict::Support.can_support_approval());
    assert!(!Verdict::Dispute.can_support_approval());
    assert!(!Verdict::InsufficientData.can_support_approval());
}

#[test]
fn an_independent_opinion_is_not_a_binding_vote() {
    let opinion = Opinion::new("node-a", subject(), Stage::Independent, Verdict::Support);
    assert!(!opinion.is_binding());

    let vote = Opinion::new("node-a", subject(), Stage::BindingSupport, Verdict::Support);
    assert!(vote.is_binding());
}

#[test]
fn stages_do_not_aggregate() {
    // Mixing stages when counting a quorum is exactly the error the design
    // forbids: first-round opinions are not votes.
    let a = Opinion::new("node-a", subject(), Stage::Independent, Verdict::Support);
    let b = Opinion::new("node-a", subject(), Stage::BindingSupport, Verdict::Support);
    assert_ne!(a.stage(), b.stage());
    assert!(!a.counts_with(&b));
}

#[test]
fn opinions_on_different_revisions_do_not_count_together() {
    let later = Subject::new(
        "mission-1",
        "event-7",
        1,
        [0xAB; 32],
        Timestamp::from_secs(1_000),
    )
    .expect("valid subject");
    let a = Opinion::new("node-a", subject(), Stage::BindingSupport, Verdict::Support);
    let b = Opinion::new("node-b", later, Stage::BindingSupport, Verdict::Support);
    assert!(!a.counts_with(&b));
}
