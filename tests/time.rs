//! Bounded clock error and what a node may conclude under it.

use lorai::domain::time::{Clock, FixedClock, MAX_CLOCK_SKEW_SECONDS, Timestamp};

#[test]
fn a_deadline_is_certainly_passed_only_beyond_the_skew_budget() {
    let started = Timestamp::from_secs(1_000);
    let deadline = started.plus_secs(300);

    // Just past the deadline, our clock could be fast: not certain.
    let unsure = FixedClock::new(Timestamp::from_secs(1_301));
    assert!(!unsure.certainly_after(deadline));

    // Beyond the skew budget, no honest clock disagrees.
    let sure = FixedClock::new(Timestamp::from_secs(1_301 + MAX_CLOCK_SKEW_SECONDS));
    assert!(sure.certainly_after(deadline));
}

#[test]
fn a_deadline_is_certainly_open_only_before_the_skew_budget() {
    let deadline = Timestamp::from_secs(1_300);

    let unsure = FixedClock::new(Timestamp::from_secs(1_299));
    assert!(!unsure.certainly_before(deadline));

    let sure = FixedClock::new(Timestamp::from_secs(1_299 - MAX_CLOCK_SKEW_SECONDS));
    assert!(sure.certainly_before(deadline));
}

#[test]
fn uncertainty_is_neither_before_nor_after() {
    // The band where a node must not claim to know. The spec is explicit: when
    // time uncertainty prevents safely establishing validity, the node does not
    // cast a binding vote. It still shows a local warning.
    let deadline = Timestamp::from_secs(1_000);
    let clock = FixedClock::new(Timestamp::from_secs(1_000));
    assert!(!clock.certainly_before(deadline));
    assert!(!clock.certainly_after(deadline));
}

#[test]
fn timestamps_are_monotonic_under_addition() {
    let t = Timestamp::from_secs(10);
    assert!(t.plus_secs(1) > t);
    assert_eq!(t.plus_secs(0), t);
}

#[test]
fn saturating_arithmetic_does_not_wrap() {
    // A corrupt or hostile validity field must not wrap a deadline into the past.
    let late = Timestamp::from_secs(u64::MAX);
    assert_eq!(late.plus_secs(10), late);
}
