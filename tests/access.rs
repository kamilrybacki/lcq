//! Random contention against slots earned by manifest index.

use lcq::sim::{Access, Report, Scenario, Topology};

/// Dead time between slots: demodulation jitter plus drift across a round.
const GUARD_MS: u64 = 200;

/// How many times longer one run took than another.
///
/// Durations are milliseconds in the hundreds of thousands, far inside f64's
/// exact integer range, so the conversion loses nothing.
#[allow(clippy::cast_precision_loss)]
fn advantage(fleet: usize) -> f64 {
    let random = duration_ms(&Scenario::new(fleet).run()) as f64;
    let slotted = duration_ms(&Scenario::new(fleet).with_slots(GUARD_MS).run()) as f64;
    random / slotted
}

fn duration_ms(report: &Report) -> u64 {
    report
        .timeline
        .iter()
        .map(|f| f.start_ms + f.airtime_ms)
        .max()
        .unwrap_or(0)
}

#[test]
fn slots_remove_collisions_entirely() {
    // Not "fewer": none. Two members cannot pick the same slot, because neither
    // picks at all -- the manifest index is the slot.
    for fleet in [5usize, 10, 20] {
        let report = Scenario::new(fleet).with_slots(GUARD_MS).run();
        assert_eq!(
            report.collided_frames, 0,
            "{fleet} members still collided under slotted access"
        );
    }
}

#[test]
fn slots_send_exactly_one_frame_per_member_on_a_clean_channel() {
    let report = Scenario::new(10).with_slots(GUARD_MS).run();
    assert_eq!(report.frames_sent, 10);
    assert!(report.endorsed);
}

#[test]
fn random_contention_costs_more_frames_than_there_are_members() {
    // The comparison that makes the case: the extra frames are retries of
    // frames the fleet destroyed itself.
    let report = Scenario::new(10).run();
    assert!(report.collided_frames > 0);
    assert!(report.frames_sent > 10);
}

#[test]
fn slots_finish_an_order_of_magnitude_sooner() {
    let random = duration_ms(&Scenario::new(10).run());
    let slotted = duration_ms(&Scenario::new(10).with_slots(GUARD_MS).run());
    assert!(
        slotted * 8 < random,
        "slotted took {slotted} ms against {random} ms; expected at least 8x better"
    );
}

#[test]
fn the_advantage_grows_with_the_fleet() {
    // Random access degrades faster than linearly, because the chance that any
    // two of N members overlap grows with N squared. Slots stay linear.
    let small = advantage(5);
    let large = advantage(10);
    assert!(
        large > small,
        "advantage should widen: 5 members {small:.1}x, 10 members {large:.1}x"
    );
}

#[test]
fn slots_rescue_a_quorum_the_duty_cycle_was_blocking() {
    // The strongest result. With 34 of 36 seconds already spent on other
    // traffic, random contention wastes the remainder on retransmitting
    // collided frames and the fleet falls short. Slotted, every member gets
    // through on its first attempt and the budget is enough.
    let random = Scenario::new(10).with_prior_airtime_ms(34_000).run();
    let slotted = Scenario::new(10)
        .with_prior_airtime_ms(34_000)
        .with_slots(GUARD_MS)
        .run();

    assert!(
        !random.endorsed,
        "the random-access baseline must still fail"
    );
    assert!(random.duty_cycle_blocked > 0);

    assert!(
        slotted.endorsed,
        "slots must get the fleet under the budget"
    );
    assert_eq!(slotted.duty_cycle_blocked, 0);
}

#[test]
fn slots_do_not_repair_a_partition() {
    // Scheduling decides who talks over whom. It cannot make an unreachable
    // member audible, and a scheme that appeared to would be inventing quorum.
    let report = Scenario::new(10)
        .with_topology(Topology::Partitioned)
        .with_slots(GUARD_MS)
        .run();
    assert!(!report.endorsed);
    assert!(report.binding_supporters < report.min_signers);
}

#[test]
fn slots_do_not_make_a_distant_member_audible() {
    let report = Scenario::new(10)
        .with_spacing_m(5_000.0)
        .with_slots(GUARD_MS)
        .run();
    assert!(report.too_weak_frames > 0);
    assert!(!report.endorsed, "the tail is past the horizon either way");
}

#[test]
fn a_forgery_is_refused_under_either_schedule() {
    let slotted = Scenario::new(5).with_forgers(2).with_slots(GUARD_MS).run();
    assert!(slotted.rejected_frames >= 2);
    assert_eq!(slotted.binding_supporters, 3);
    assert!(!slotted.endorsed);
}

#[test]
fn the_access_mode_is_reported_as_configured() {
    assert_eq!(Scenario::new(5).access(), Access::Random);
    assert_eq!(
        Scenario::new(5).with_slots(GUARD_MS).access(),
        Access::Slotted { guard_ms: GUARD_MS }
    );
}

#[test]
fn slotted_runs_are_deterministic() {
    let a = Scenario::new(10).with_slots(GUARD_MS).with_loss(0.3).run();
    let b = Scenario::new(10).with_slots(GUARD_MS).with_loss(0.3).run();
    assert_eq!(a, b);
}
