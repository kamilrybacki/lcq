//! What it costs that no frame on the air says "heard you".

use lorai::sim::{Acknowledgement, DUTY_CYCLE_BUDGET_MS, Scenario};

const GUARD_MS: u64 = 200;

fn informed(prior_ms: u64) -> lorai::sim::Report {
    Scenario::new(10)
        .with_slots(GUARD_MS)
        .with_prior_airtime_ms(prior_ms)
        .run()
}

fn blind(prior_ms: u64) -> lorai::sim::Report {
    Scenario::new(10)
        .with_slots(GUARD_MS)
        .with_prior_airtime_ms(prior_ms)
        .without_acknowledgement()
        .run()
}

#[test]
fn the_default_model_assumes_an_acknowledgement_the_wire_format_does_not_carry() {
    // Named so it cannot be forgotten: every other figure in this crate is
    // measured under this assumption. `Journal::acknowledge` exists at the
    // storage layer, but nothing defines a frame that carries the fact.
    assert_eq!(Scenario::new(5).acknowledgement(), Acknowledgement::Assumed);
}

#[test]
fn without_acknowledgement_a_fleet_spends_several_times_the_airtime() {
    let with = informed(0);
    let without = blind(0);
    assert_eq!(with.frames_sent, 10, "one frame per member when they know");
    assert!(
        without.frames_sent >= 4 * with.frames_sent,
        "blind retries should multiply the frames: {} against {}",
        without.frames_sent,
        with.frames_sent
    );
}

#[test]
fn the_missing_acknowledgement_costs_airtime_and_not_the_quorum() {
    // Measured rather than assumed, and it corrected the guess that came first:
    // under slots the first attempt always lands, so every blind retry is waste
    // that arrives AFTER the vote already counted. Endorsement is unaffected.
    for prior in [0u64, 30_000, 32_000, 34_000] {
        let without = blind(prior);
        assert!(
            without.endorsed,
            "blind retries cost airtime, not correctness; failed at {prior} ms prior"
        );
    }
}

#[test]
fn the_duty_cycle_clips_the_waste_rather_than_the_vote() {
    // A useful accident of ordering: the budget runs out during the retries,
    // which nobody needed, not during the first attempt, which everybody did.
    let squeezed = blind(32_000);
    assert!(squeezed.duty_cycle_blocked > 0, "the limit must bite");
    assert!(
        squeezed.endorsed,
        "and it must bite the waste, not the quorum"
    );
}

#[test]
fn an_acknowledgement_multiplies_how_often_a_fleet_may_decide() {
    // Airtime is the resource the regulation meters, so it decides how often a
    // fleet may decide anything at all. That, not latency, is what the missing
    // frame costs.
    let decisions_per_hour = |airtime: u64| DUTY_CYCLE_BUDGET_MS * 10 / airtime.max(1);
    let fast = decisions_per_hour(informed(0).airtime_ms);
    let slow = decisions_per_hour(blind(0).airtime_ms);
    assert!(
        fast >= 4 * slow,
        "informed {fast} decisions/h against blind {slow}"
    );
}
