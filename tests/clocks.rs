//! Clocks that read a real source, rather than one the test hands them.

use std::thread::sleep;
use std::time::Duration;

use lcq::domain::time::{Clock, Timestamp};
use lcq::infrastructure::{ScaledClock, SystemClock};

#[test]
fn the_system_clock_is_somewhere_in_this_century() {
    // A weak assertion on purpose: the point is that it reads the machine, not
    // that the machine is right.
    let now = SystemClock.now().as_secs();
    assert!((1_600_000_000..4_000_000_000).contains(&now), "now = {now}");
}

#[test]
fn a_scaled_clock_advances_with_real_time() {
    let clock = ScaledClock::new(Timestamp::from_secs(1_000), 100, 0);
    let before = clock.now().as_secs();
    sleep(Duration::from_millis(120));
    let after = clock.now().as_secs();
    // 120 ms of wall time at 100x is about 12 protocol seconds.
    assert!(
        after >= before + 8 && after <= before + 20,
        "advanced from {before} to {after}"
    );
}

#[test]
fn a_scaled_clock_keeps_running_while_the_thread_is_blocked() {
    // The property that makes this a real clock rather than a counter: time
    // passes while the process is doing nothing, which is exactly the case a
    // node killed mid-vote has to survive.
    let clock = ScaledClock::new(Timestamp::from_secs(0), 60, 0);
    sleep(Duration::from_millis(200));
    assert!(clock.now().as_secs() >= 8, "clock stopped while blocked");
}

#[test]
fn an_offset_moves_the_clock_without_stopping_it() {
    let steady = ScaledClock::new(Timestamp::from_secs(1_000), 1, 0);
    let ahead = ScaledClock::new(Timestamp::from_secs(1_000), 1, 25);
    assert!(ahead.now().as_secs() >= steady.now().as_secs() + 24);
}

#[test]
fn two_nodes_at_opposite_ends_of_the_budget_differ_by_the_budget() {
    use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;
    let half = i64::try_from(MAX_CLOCK_SKEW_SECONDS / 2).expect("fits");
    let slow = ScaledClock::new(Timestamp::from_secs(10_000), 1, -half);
    let fast = ScaledClock::new(Timestamp::from_secs(10_000), 1, half);
    let difference = fast.now().as_secs() - slow.now().as_secs();
    assert!(
        difference >= MAX_CLOCK_SKEW_SECONDS - 1,
        "difference was {difference} s"
    );
}

#[test]
fn scaling_converts_protocol_time_to_wall_time() {
    let clock = ScaledClock::new(Timestamp::from_secs(0), 100, 0);
    // The 330 s floor becomes 3.3 s of waiting, which is a test rather than a
    // coffee break.
    assert_eq!(clock.wall_ms_for(330), 3_300);
    assert_eq!(clock.wall_ms(1_272), 12);
}
