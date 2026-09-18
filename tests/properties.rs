//! Independent checks of the threshold arithmetic.
//!
//! These are an oracle, not a proof. Passing them says the integer arithmetic
//! behaves as specified over the sampled space; it says nothing about Byzantine
//! safety of the protocol, which needs an authenticated message pipeline that
//! does not exist yet.

use std::collections::BTreeSet;

use lorai::{Policy, evaluate};
use proptest::prelude::*;

fn equal_fleet(n: usize) -> Vec<(String, u32)> {
    (0..n).map(|i| (i.to_string(), 1)).collect()
}

/// Every pair of count-quorums must overlap in more members than the budget
/// permits to be compromised. That overlap is the entire reason the count
/// threshold is set where it is.
#[test]
fn all_small_count_quorums_intersect_in_more_than_the_fault_budget() {
    for n in 1..=8usize {
        let policy = Policy::new(equal_fleet(n)).expect("valid fleet");
        let members: Vec<&str> = policy.members().collect();
        let quorums: Vec<BTreeSet<&str>> = combinations(&members, policy.min_signers());
        for a in &quorums {
            for b in &quorums {
                let overlap = a.intersection(b).count();
                assert!(
                    overlap > policy.max_faulty(),
                    "n={n} overlap={overlap} budget={}",
                    policy.max_faulty()
                );
            }
        }
    }
}

fn combinations<'a>(items: &[&'a str], k: usize) -> Vec<BTreeSet<&'a str>> {
    if k == 0 {
        return vec![BTreeSet::new()];
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        for mut rest in combinations(&items[i + 1..], k - 1) {
            rest.insert(*item);
            out.push(rest);
        }
    }
    out
}

proptest! {
    /// Recompute the verdict from the definition and compare, and confirm that
    /// repeating or reordering signers changes nothing.
    #[test]
    fn integer_oracle_and_duplicate_invariance(
        weights in prop::collection::vec(1u32..=3, 1..30),
        prefix in 0usize..100,
    ) {
        let members: Vec<(String, u32)> = weights
            .iter()
            .enumerate()
            .map(|(i, w)| (i.to_string(), *w))
            .collect();
        let policy = Policy::new(members).expect("weights within the 3:1 cap");

        let signers: Vec<String> =
            policy.members().take(prefix).map(str::to_string).collect();
        let support: u64 = signers
            .iter()
            .map(|s| u64::from(policy.weight_of(s).expect("member")))
            .sum();
        let total: u64 = weights.iter().map(|w| u64::from(*w)).sum();

        let result = evaluate(&policy, signers.clone()).expect("known signers");

        // Longhand on purpose. Clippy would have this borrow `usize::midpoint`
        // and drop the `+ 1`, which is exactly what the implementation does --
        // and an oracle that shares the implementation's helper inherits the
        // implementation's bug instead of catching it. The fleet sizes here are
        // bounded at 30, so the overflow the lint guards against cannot occur.
        #[allow(clippy::manual_midpoint, clippy::int_plus_one)]
        let expected_count = {
            let n = weights.len();
            signers.len() >= (n + (2 * n) / 5) / 2 + 1
        };
        prop_assert_eq!(result.approved(), expected_count && 3 * support > 2 * total);

        let mut shuffled: Vec<String> = signers.iter().rev().cloned().collect();
        shuffled.extend(signers);
        prop_assert_eq!(result, evaluate(&policy, shuffled).expect("known signers"));
    }

    /// The threshold must be reachable, and must keep the intersection bound
    /// across the whole fault-budget range.
    #[test]
    fn threshold_is_feasible_and_intersection_bound_holds(
        n in 1usize..=100,
        bps in 0u32..10_000,
    ) {
        let policy = Policy::with_settings(equal_fleet(n), bps, 3).expect("valid fleet");

        prop_assert!(policy.min_signers() >= 1);
        prop_assert!(policy.min_signers() <= n, "threshold must be reachable");
        // Two quorums of this size cannot both avoid overlapping beyond the budget.
        prop_assert!(2 * policy.min_signers() > n + policy.max_faulty());

        let everyone: Vec<String> = policy.members().map(str::to_string).collect();
        prop_assert!(evaluate(&policy, everyone).expect("known signers").approved());
    }
}
