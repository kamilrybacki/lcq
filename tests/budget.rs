//! A transmitter's airtime, metered over a sliding hour.

use lcq::application::{AirtimeBudget, DUTY_CYCLE_BUDGET_MS, DUTY_CYCLE_WINDOW_MS, duty_cycle_ok};

#[test]
fn the_budget_is_one_per_cent_of_an_hour() {
    assert_eq!(DUTY_CYCLE_BUDGET_MS * 100, DUTY_CYCLE_WINDOW_MS);
    assert!(duty_cycle_ok(DUTY_CYCLE_BUDGET_MS, 0));
    assert!(!duty_cycle_ok(DUTY_CYCLE_BUDGET_MS, 1));
}

#[test]
fn a_fresh_budget_allows_a_frame_and_charges_for_it() {
    let mut budget = AirtimeBudget::new();
    assert!(budget.transmit(0, 1_300));
    assert_eq!(budget.used_ms(0), 1_300);
}

#[test]
fn a_spent_budget_refuses_and_records_nothing() {
    let mut budget = AirtimeBudget::with_prior(35_000);
    assert!(!budget.transmit(1_000, 1_300), "35 s + 1.3 s is over 36 s");
    assert_eq!(
        budget.used_ms(1_000),
        35_000,
        "a refused frame is not charged"
    );
}

#[test]
fn airtime_leaves_the_window_after_an_hour() {
    // What makes it a sliding window rather than a running total: the same
    // node may deliberate again next hour.
    let mut budget = AirtimeBudget::with_prior(36_000);
    assert!(!budget.allows(DUTY_CYCLE_WINDOW_MS - 1, 1));
    assert!(budget.allows(DUTY_CYCLE_WINDOW_MS + 1, 1_300));
    assert_eq!(budget.used_ms(DUTY_CYCLE_WINDOW_MS + 1), 0);
}

#[test]
fn eviction_is_by_transmission_time_not_by_order_of_asking() {
    let mut budget = AirtimeBudget::new();
    budget.charge(0, 10_000);
    budget.charge(1_800_000, 10_000);
    // Half an hour after the second transmission the first has left the
    // window and the second has not.
    assert_eq!(
        budget.used_ms(1_800_000 + DUTY_CYCLE_WINDOW_MS / 2 + 1),
        10_000
    );
}

#[test]
fn the_whole_budget_is_exactly_twenty_seven_default_frames() {
    // 137 B at SF10 is 1313 ms; the hour holds 27 of them and refuses the 28th.
    let air = lcq::sim::airtime_ms(137);
    let mut budget = AirtimeBudget::new();
    let mut sent = 0;
    while budget.transmit(0, air) {
        sent += 1;
    }
    assert_eq!(sent, DUTY_CYCLE_BUDGET_MS / air);
}
