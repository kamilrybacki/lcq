//! Count and competence thresholds, reported separately because they fail
//! differently.

use lcq::domain::quorum::{Competence, EvaluationError, Policy, evaluate};

/// A fleet whose members all count the same, at the top of the scale.
fn equal_fleet(n: usize) -> Vec<(String, u8)> {
    (0..n)
        .map(|i| (i.to_string(), Competence::FULL.value()))
        .collect()
}

fn fleet(pairs: &[(&str, u8)]) -> Policy {
    Policy::new(pairs.iter().map(|(id, c)| ((*id).to_string(), *c))).expect("valid fleet")
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn equal_competence_five_needs_four() {
    let policy = Policy::new(equal_fleet(5)).expect("valid fleet");
    assert!(
        !evaluate(&policy, ids(&["0", "1", "2"]))
            .expect("known signers")
            .approved()
    );
    assert!(
        evaluate(&policy, ids(&["0", "1", "2", "3"]))
            .expect("known signers")
            .approved()
    );
}

#[test]
fn exactly_two_thirds_is_not_enough() {
    // The competence threshold is STRICTLY greater than two thirds.
    let policy = Policy::with_settings(equal_fleet(3), 0, 3).expect("valid fleet");
    let result = evaluate(&policy, ids(&["0", "1"])).expect("known signers");
    assert!(result.count_met());
    assert!(!result.competence_met());
    assert!(!result.approved());
}

#[test]
fn a_highly_competent_member_can_block_despite_count() {
    // Documented consequence: the 3:1 cap does not stop one member at the top
    // of the scale from holding the competence threshold hostage. 99 and 33
    // are the widest spread the cap allows.
    let policy = fleet(&[("a", 99), ("b", 33), ("c", 33), ("d", 33), ("e", 33)]);
    let result = evaluate(&policy, ids(&["b", "c", "d", "e"])).expect("known signers");
    assert_eq!(
        (result.signer_count(), result.support_competence()),
        (4, 132)
    );
    assert!(result.count_met());
    assert!(!result.competence_met());
    assert!(!result.approved());
}

#[test]
fn competence_cannot_replace_required_member_count() {
    let policy = fleet(&[("a", 99), ("b", 99), ("c", 99), ("d", 33), ("e", 33)]);
    let result = evaluate(&policy, ids(&["a", "b", "c"])).expect("known signers");
    assert!(result.competence_met());
    assert!(!result.count_met());
    assert!(!result.approved());
}

#[test]
fn duplicate_signers_do_not_increase_support() {
    let policy = Policy::new(equal_fleet(5)).expect("valid fleet");
    let repeated = vec!["0".to_string(); 100];
    assert_eq!(
        evaluate(&policy, repeated).expect("known signers"),
        evaluate(&policy, ids(&["0"])).expect("known signers")
    );
}

#[test]
fn unknown_member_is_rejected_not_added_to_denominator() {
    let policy = fleet(&[("a", 100), ("b", 100)]);
    let err = evaluate(&policy, ids(&["a", "intruder"])).unwrap_err();
    assert_eq!(err, EvaluationError::UnknownSigner);
    assert_eq!(policy.total_competence(), 200);
}

#[test]
fn empty_support_is_not_approved() {
    let policy = fleet(&[("a", 100)]);
    let result = evaluate(&policy, Vec::<String>::new()).expect("no signers is not an error");
    assert!(!result.approved());
    assert_eq!(result.signer_count(), 0);
    assert_eq!(result.support_competence(), 0);
}

#[test]
fn signer_order_does_not_change_the_outcome() {
    // Arrival order over a radio is arbitrary; the verdict must not depend on it.
    let policy = Policy::new(equal_fleet(5)).expect("valid fleet");
    let forward = evaluate(&policy, ids(&["0", "1", "2", "3"])).expect("known signers");
    let reverse = evaluate(&policy, ids(&["3", "2", "1", "0"])).expect("known signers");
    assert_eq!(forward, reverse);
}

#[test]
fn a_fleet_may_mix_members_that_decide_by_different_means() {
    // The manifest is the only thing that says what a member's judgment is
    // worth. Nothing here knows that some of these reach a verdict with a
    // language model, some with a deterministic calculation and some from an
    // instrument: the protocol sees five normalised scores and nothing else.
    let policy = fleet(&[
        ("model", 99),
        ("calculation", 66),
        ("second-calculation", 66),
        ("instrument", 33),
        ("second-instrument", 33),
    ]);
    assert_eq!(policy.total_competence(), 297);

    let four = evaluate(
        &policy,
        ids(&["model", "calculation", "second-calculation", "instrument"]),
    )
    .expect("known signers");
    assert_eq!(four.support_competence(), 264);
    assert!(four.count_met(), "four of five clears the count threshold");
    assert!(four.competence_met(), "264 of 297 is more than two thirds");
    assert!(four.approved());

    // And the members of lesser standing cannot carry it without the greatest
    // one, even at full count: 198 of 297 is exactly two thirds, not more.
    let without = evaluate(
        &policy,
        ids(&[
            "calculation",
            "second-calculation",
            "instrument",
            "second-instrument",
        ]),
    )
    .expect("known signers");
    assert_eq!(without.support_competence(), 198);
    assert!(without.count_met());
    assert!(!without.competence_met());
}
