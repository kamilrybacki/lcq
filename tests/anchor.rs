//! Where a slot schedule counts from, and whether clock skew can be survived.

use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;
use lcq::sim::{Deliberation, SlotAnchor};

const GUARD_MS: u64 = 200;

fn run(anchor: SlotAnchor, skew: u64, seed: u64) -> lcq::sim::DeliberationReport {
    Deliberation::new(10)
        .with_slots(GUARD_MS)
        .with_anchor(anchor)
        .with_clock_skew_s(skew)
        .with_seed(seed)
        .run()
}

fn collisions(report: &lcq::sim::DeliberationReport) -> usize {
    report.stages.iter().map(|s| s.collided).sum()
}

#[test]
fn a_trigger_anchored_schedule_is_untouched_by_clock_skew() {
    // It counts from a frame every participant heard, so the origin is agreed
    // to the demodulator's accuracy. Clock error simply does not enter.
    for skew in [0, 2, 10, MAX_CLOCK_SKEW_SECONDS] {
        for seed in 1..=5 {
            let report = run(SlotAnchor::Trigger, skew, seed);
            assert_eq!(
                collisions(&report),
                0,
                "skew {skew} s, seed {seed}: trigger anchor collided"
            );
            assert!(report.endorsed);
        }
    }
}

#[test]
fn a_clock_anchored_schedule_collapses_at_any_real_skew() {
    // The finding that settles the anchor question. A slot is about 1.5 s wide
    // and the budget allows honest clocks to differ by 30 s, so members land in
    // each other's slots as soon as the clocks are allowed to disagree at all.
    // Making the slot wider than the budget would mean a 30 s slot for a 1.3 s
    // frame, which discards the entire point of scheduling.
    let mut blocked = 0;
    let mut collided = 0;
    for seed in 1..=20 {
        let report = run(SlotAnchor::Clock, 2, seed);
        collided += collisions(&report);
        if !report.endorsed {
            blocked += 1;
        }
    }
    assert!(
        collided > 500,
        "expected the schedule to fall apart, saw {collided} collisions in 20 runs"
    );
    assert!(
        blocked > 0,
        "expected at least one run to lose its quorum outright"
    );
}

#[test]
fn a_clock_anchored_schedule_is_fine_only_on_the_fiction_of_one_clock() {
    // Which is the assumption every earlier figure in this crate rested on.
    let report = run(SlotAnchor::Clock, 0, 1);
    assert_eq!(collisions(&report), 0);
    assert!(report.endorsed);
}

#[test]
fn skew_does_not_change_the_airtime_under_a_trigger_anchor() {
    let steady = run(SlotAnchor::Trigger, 0, 1);
    let skewed = run(SlotAnchor::Trigger, MAX_CLOCK_SKEW_SECONDS, 1);
    assert_eq!(steady.airtime_ms, skewed.airtime_ms);
}

#[test]
fn clock_offsets_span_the_whole_budget() {
    // A scenario that quietly used half the budget would flatter itself, so the
    // extremes are pinned: the widest pair differs by exactly the budget. The
    // visible consequence is that the skewed run costs strictly more airtime.
    let steady = run(SlotAnchor::Clock, 0, 3);
    let skewed = run(SlotAnchor::Clock, MAX_CLOCK_SKEW_SECONDS, 3);
    assert!(skewed.airtime_ms > steady.airtime_ms);
}
