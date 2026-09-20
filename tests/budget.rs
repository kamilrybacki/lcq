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

/* ------------------------------------------------------------------ *
 * The same budget, pointed the other way: what one SENDER is allowed  *
 * to make a receiver pay for (THREAT-MODEL.md F10).                   *
 * ------------------------------------------------------------------ */

/// A default frame at the frozen profile, in milliseconds of air.
const FRAME_MS: u64 = 1_320;

#[test]
fn a_receiver_allows_exactly_what_the_sender_was_allowed_to_send() {
    // The receiver's allowance is the same rule as the sender's own budget,
    // pointed the other way, so the two agree frame for frame. That is the
    // whole argument for choosing the duty cycle as the cap: anything an
    // honest member was permitted to transmit, a receiver will pay for, and
    // the first frame it refuses is one the sender's own budget refused too.
    let mut sender = AirtimeBudget::new();
    let mut receiver = AirtimeBudget::new();
    let mut sent = 0;
    for frame in 0..60u64 {
        let at = frame * FRAME_MS;
        let allowed = sender.transmit(at, FRAME_MS);
        assert_eq!(
            allowed,
            receiver.transmit(at, FRAME_MS),
            "frame {frame}: the two sides disagreed about the same rule"
        );
        sent += u64::from(allowed);
    }
    assert_eq!(sent, 27, "the whole hour's budget and not one frame more");
}

#[test]
fn an_honest_round_pattern_is_nowhere_near_the_allowance() {
    // What a member flying the protocol actually does: three stages and one
    // retransmission each, a few rounds an hour. A cap that fired on this
    // would break a fleet rather than defend one.
    let mut allowance = AirtimeBudget::new();
    for round in 0..6u64 {
        for frame in 0..4u64 {
            let at = round * 60_000 + frame * 2_000;
            assert!(
                allowance.transmit(at, FRAME_MS),
                "frame {frame} of round {round} was refused an honest member"
            );
        }
    }
    assert!(
        allowance.used_ms(6 * 60_000) < DUTY_CYCLE_BUDGET_MS,
        "and it still has room to spare"
    );
}

#[test]
fn a_member_that_never_stops_is_cut_off_and_heard_again_later() {
    let mut allowance = AirtimeBudget::new();
    let mut accepted = 0;
    // Back to back, as fast as the radio can go.
    for frame in 0..60u64 {
        if allowance.transmit(frame * FRAME_MS, FRAME_MS) {
            accepted += 1;
        }
    }
    assert_eq!(
        accepted, 27,
        "the whole hour's budget and not one frame more"
    );

    // An hour later the window has moved and it is heard again: this is a
    // rate limit, not a ban. A member that went quiet because its radio was
    // broken must be able to come back.
    assert!(
        allowance.transmit(3_600_000 + 60 * FRAME_MS, FRAME_MS),
        "a sender that waited out the window must be heard again"
    );
}
