//! Fleet manifest validation and threshold arithmetic.

use lcq::domain::quorum::Competence;
use lcq::{Policy, PolicyError};

/// A fleet whose members all count the same, at the top of the scale.
fn equal_fleet(n: usize) -> Vec<(String, u8)> {
    (0..n)
        .map(|i| (i.to_string(), Competence::FULL.value()))
        .collect()
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
        assert_eq!(
            policy.total_competence(),
            n as u64 * u64::from(Competence::SCALE)
        );
    }
}

#[test]
fn empty_fleet_is_rejected() {
    assert_eq!(Policy::new([]).unwrap_err(), PolicyError::EmptyFleet);
}

#[test]
fn a_competence_off_the_scale_is_rejected() {
    // Zero is not a member, and the scale has a ceiling so that no manifest
    // can invent a member of unbounded standing.
    assert_eq!(
        Policy::new([("a".to_string(), 0)]).unwrap_err(),
        PolicyError::CompetenceOutOfRange
    );
    assert_eq!(
        Policy::new([("a".to_string(), 101)]).unwrap_err(),
        PolicyError::CompetenceOutOfRange
    );
    assert_eq!(
        Competence::try_new(0).unwrap_err(),
        PolicyError::CompetenceOutOfRange
    );
    assert_eq!(Competence::try_new(1).expect("on the scale").value(), 1);
    assert_eq!(Competence::try_new(100).expect("on the scale").value(), 100);
}

#[test]
fn blank_member_id_is_rejected() {
    let err = Policy::new([(String::new(), 1)]).unwrap_err();
    assert_eq!(err, PolicyError::BlankMemberId);
}

#[test]
fn duplicate_member_id_is_rejected() {
    // A manifest that names the same member twice has an ambiguous denominator.
    let err = Policy::new([("a".to_string(), 50), ("a".to_string(), 60)]).unwrap_err();
    assert_eq!(err, PolicyError::DuplicateMemberId);
}

#[test]
fn competence_ratio_cap_is_enforced() {
    let err = Policy::new([("a".to_string(), 100), ("b".to_string(), 25)]).unwrap_err();
    assert_eq!(err, PolicyError::CompetenceRatioExceeded);
    // 3:1 is the cap, not a forbidden value.
    Policy::new([("a".to_string(), 99), ("b".to_string(), 33)]).expect("3:1 is allowed");
}

#[test]
fn the_band_makes_any_independently_scored_manifest_legal() {
    // A service that scores members one at a time cannot see the fleet's
    // spread. Scoring into the band the cap implies removes the question.
    let (floor, ceiling) = Competence::band(3);
    assert_eq!((floor, ceiling), (34, 100));
    Policy::new([("a".to_string(), ceiling), ("b".to_string(), floor)])
        .expect("the band is legal at its extremes");
    assert_eq!(Competence::band(1), (100, 100), "no spread at all");
    assert_eq!(Competence::band(2), (50, 100));
    assert_eq!(Competence::band(100), (1, 100), "the whole scale");
}

#[test]
fn fault_budget_must_be_below_ten_thousand_basis_points() {
    let err = Policy::with_settings(equal_fleet(3), 10_000, 3).unwrap_err();
    assert_eq!(err, PolicyError::FaultBudgetOutOfRange);
    Policy::with_settings(equal_fleet(3), 9_999, 3).expect("just under the ceiling is allowed");
}

#[test]
fn competence_ratio_must_be_positive() {
    let err = Policy::with_settings(equal_fleet(3), 4_000, 0).unwrap_err();
    assert_eq!(err, PolicyError::NonPositiveCompetenceRatio);
}

#[test]
fn competences_are_owned_by_the_policy() {
    // The caller's vector is consumed; there is no shared handle left to mutate
    // a threshold after the policy was built.
    let mut source = vec![("a".to_string(), 50), ("b".to_string(), 100)];
    let policy = Policy::new(source.clone()).expect("valid fleet");
    source[0].1 = 99;
    assert_eq!(
        policy.competence_of("a").map(Competence::value),
        Some(50),
        "the policy kept its own copy"
    );
}

#[test]
fn unreachable_members_still_count_in_the_denominator() {
    // Silence is not consent. Shrinking the denominator when the radio degrades
    // would lower the bar exactly when conditions are worst.
    let policy = Policy::new(equal_fleet(10)).expect("valid fleet");
    assert_eq!(policy.size(), 10);
    assert_eq!(policy.min_signers(), 8);
}
