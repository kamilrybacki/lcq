//! Count and weight thresholds, reported separately because they fail differently.

use lcq::domain::quorum::{EvaluationError, Policy, evaluate};

fn equal_fleet(n: usize) -> Vec<(String, u32)> {
    (0..n).map(|i| (i.to_string(), 1)).collect()
}

fn fleet(pairs: &[(&str, u32)]) -> Policy {
    Policy::new(pairs.iter().map(|(id, w)| ((*id).to_string(), *w))).expect("valid fleet")
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn equal_weight_five_needs_four() {
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
    // The weight threshold is STRICTLY greater than two thirds.
    let policy = Policy::with_settings(equal_fleet(3), 0, 3).expect("valid fleet");
    let result = evaluate(&policy, ids(&["0", "1"])).expect("known signers");
    assert!(result.count_met());
    assert!(!result.weight_met());
    assert!(!result.approved());
}

#[test]
fn heavy_member_can_block_despite_count() {
    // Documented consequence: the 3:1 cap does not stop one heavy member from
    // holding the weight threshold hostage.
    let policy = fleet(&[("a", 3), ("b", 1), ("c", 1), ("d", 1), ("e", 1)]);
    let result = evaluate(&policy, ids(&["b", "c", "d", "e"])).expect("known signers");
    assert_eq!((result.signer_count(), result.support_weight()), (4, 4));
    assert!(result.count_met());
    assert!(!result.weight_met());
    assert!(!result.approved());
}

#[test]
fn weight_cannot_replace_required_member_count() {
    let policy = fleet(&[("a", 3), ("b", 3), ("c", 3), ("d", 1), ("e", 1)]);
    let result = evaluate(&policy, ids(&["a", "b", "c"])).expect("known signers");
    assert!(result.weight_met());
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
    let policy = fleet(&[("a", 1), ("b", 1)]);
    let err = evaluate(&policy, ids(&["a", "intruder"])).unwrap_err();
    assert_eq!(err, EvaluationError::UnknownSigner);
    assert_eq!(policy.total_weight(), 2);
}

#[test]
fn empty_support_is_not_approved() {
    let policy = fleet(&[("a", 1)]);
    let result = evaluate(&policy, Vec::<String>::new()).expect("no signers is not an error");
    assert!(!result.approved());
    assert_eq!(result.signer_count(), 0);
    assert_eq!(result.support_weight(), 0);
}

#[test]
fn signer_order_does_not_change_the_outcome() {
    // Arrival order over a radio is arbitrary; the verdict must not depend on it.
    let policy = Policy::new(equal_fleet(5)).expect("valid fleet");
    let forward = evaluate(&policy, ids(&["0", "1", "2", "3"])).expect("known signers");
    let reverse = evaluate(&policy, ids(&["3", "2", "1", "0"])).expect("known signers");
    assert_eq!(forward, reverse);
}
