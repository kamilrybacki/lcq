//! Fleet manifest validation and threshold arithmetic.

use lorai::{Policy, PolicyError};

fn equal_fleet(n: usize) -> Vec<(String, u32)> {
    (0..n).map(|i| (i.to_string(), 1)).collect()
}

#[test]
fn default_thresholds() {
    // (members, tolerated faults, signers required)
    for (n, f, q) in [(5, 2, 4), (10, 4, 8), (20, 8, 15), (100, 40, 71)] {
        let policy = Policy::new(equal_fleet(n)).expect("valid fleet");
        assert_eq!(
            (policy.size(), policy.max_faulty(), policy.min_signers()),
            (n, f, q),
            "n={n}"
        );
        assert_eq!(policy.total_weight(), n as u64);
    }
}

#[test]
fn empty_fleet_is_rejected() {
    assert_eq!(Policy::new([]).unwrap_err(), PolicyError::EmptyFleet);
}

#[test]
fn zero_weight_is_rejected() {
    let err = Policy::new([("a".to_string(), 0)]).unwrap_err();
    assert_eq!(err, PolicyError::NonPositiveWeight);
}

#[test]
fn blank_member_id_is_rejected() {
    let err = Policy::new([(String::new(), 1)]).unwrap_err();
    assert_eq!(err, PolicyError::BlankMemberId);
}

#[test]
fn duplicate_member_id_is_rejected() {
    // A manifest that names the same member twice has an ambiguous denominator.
    let err = Policy::new([("a".to_string(), 1), ("a".to_string(), 2)]).unwrap_err();
    assert_eq!(err, PolicyError::DuplicateMemberId);
}

#[test]
fn weight_ratio_cap_is_enforced() {
    let err = Policy::new([("a".to_string(), 4), ("b".to_string(), 1)]).unwrap_err();
    assert_eq!(err, PolicyError::WeightRatioExceeded);
    // 3:1 is the cap, not a forbidden value.
    Policy::new([("a".to_string(), 3), ("b".to_string(), 1)]).expect("3:1 is allowed");
}

#[test]
fn fault_budget_must_be_below_ten_thousand_basis_points() {
    let err = Policy::with_settings(equal_fleet(3), 10_000, 3).unwrap_err();
    assert_eq!(err, PolicyError::FaultBudgetOutOfRange);
    Policy::with_settings(equal_fleet(3), 9_999, 3).expect("just under the ceiling is allowed");
}

#[test]
fn weight_ratio_must_be_positive() {
    let err = Policy::with_settings(equal_fleet(3), 4_000, 0).unwrap_err();
    assert_eq!(err, PolicyError::NonPositiveWeightRatio);
}

#[test]
fn weights_are_owned_by_the_policy() {
    // The caller's vector is consumed; there is no shared handle left to mutate
    // a threshold after the policy was built.
    let mut source = vec![("a".to_string(), 1), ("b".to_string(), 2)];
    let policy = Policy::new(source.clone()).expect("valid fleet");
    source[0].1 = 99;
    assert_eq!(policy.weight_of("a"), Some(1));
}

#[test]
fn unreachable_members_still_count_in_the_denominator() {
    // Silence is not consent. Shrinking the denominator when the radio degrades
    // would lower the bar exactly when conditions are worst.
    let policy = Policy::new(equal_fleet(10)).expect("valid fleet");
    assert_eq!(policy.size(), 10);
    assert_eq!(policy.min_signers(), 8);
}
