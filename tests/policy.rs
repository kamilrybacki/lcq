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

#[test]
fn weighting_can_only_raise_the_cost_of_capture_never_lower_it() {
    // The question a reader arrives with is "what is this protocol's 51 %
    // attack". The answer turns on approval being the AND of two thresholds,
    // headcount and competence, rather than weight replacing headcount the way
    // stake does on a chain.
    //
    // This pins the half that is not obvious: under the ratio cap, the
    // competence threshold is always reachable by FEWER members than the count
    // threshold needs. So competence never lets an attacker succeed with a
    // smaller group -- it can only impose a second requirement on top of the
    // count. Checked over every fleet this protocol admits.
    for size in 3..=lcq::wire::Heard::CAPACITY {
        // The most concentrated fleet the 3:1 cap allows: some members at the
        // top of the scale, the rest at a third of it.
        for high in 1..size {
            let members: Vec<(String, u8)> = (0..size)
                .map(|index| {
                    let competence = if index < high { 100 } else { 34 };
                    (format!("m{index}"), competence)
                })
                .collect();
            let policy = Policy::new(members).expect("100 against 34 is inside a 3:1 cap");

            // The fewest members that could hold more than two thirds of the
            // competence: take them from the top of the scale.
            let total = policy.total_competence();
            let mut carried = 0u64;
            let mut for_competence = 0usize;
            for index in 0..size {
                if 3 * carried > 2 * total {
                    break;
                }
                carried += if index < high { 100u64 } else { 34 };
                for_competence += 1;
            }

            assert!(
                policy.min_signers() >= for_competence,
                "a fleet of {size} with {high} at the top of the scale needs \
                 {} signers by count and {for_competence} by competence: \
                 weighting would be the weaker of the two, and an attacker \
                 would only have to buy the heavy members",
                policy.min_signers()
            );
        }
    }
}

#[test]
fn forcing_a_verdict_takes_at_least_seventy_per_cent_of_the_fleet() {
    // The number a reader wants next to "51 %". Forcing a verdict takes this
    // share of the fleet; the rest plus one can block one by staying silent,
    // which is the cheap attack here as it is on a chain.
    //
    // Measured rather than asserted from the formula: 70 % is the floor, and
    // it is reached only in the larger fleets. Small ones are stricter, and a
    // fleet of three is unanimous.
    let mut lowest = 100;
    for size in 3..=lcq::wire::Heard::CAPACITY {
        let policy = Policy::new(equal_fleet(size)).expect("an equal fleet is lawful");
        let share = 100 * policy.min_signers() / size;
        assert!(
            share >= 70,
            "a fleet of {size} needs {} of {size}, which is only {share} %",
            policy.min_signers()
        );
        assert!(
            policy.min_signers() <= size,
            "a fleet cannot need more signers than it has members"
        );
        lowest = lowest.min(share);
    }
    assert_eq!(lowest, 70, "the floor across every lawful fleet size");

    // The liveness margin is worst exactly where a first deployment starts:
    // below four members the threshold is the whole fleet, so one member
    // staying silent blocks everything.
    for size in 3..4 {
        let policy = Policy::new(equal_fleet(size)).expect("lawful");
        assert_eq!(policy.min_signers(), size);
    }
}
