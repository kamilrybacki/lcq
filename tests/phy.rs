//! Propagation, collisions, capture and duty cycle.

use lcq::sim::{
    CAPTURE_THRESHOLD_DB, DUTY_CYCLE_BUDGET_MS, Link, Reception, RicianFading, SENSITIVITY_DBM,
    Transmission, capture_wins, duty_cycle_ok, path_loss_db, radio_horizon_m, receive, rssi_dbm,
};

/// Free space spreads over a sphere: doubling the distance costs 6 dB.
const FREE_SPACE_DOUBLING_DB: f64 = 6.02;
/// Two-ray ground reflection: the sea-surface bounce arrives antiphase and
/// cancels the direct path, so doubling costs 12 dB instead.
const TWO_RAY_DOUBLING_DB: f64 = 12.04;

#[test]
fn signal_weakens_with_distance() {
    assert!(path_loss_db(10_000.0) > path_loss_db(1_000.0));
}

#[test]
fn below_the_breakpoint_loss_grows_at_the_free_space_rate() {
    let doubling = path_loss_db(800.0) - path_loss_db(400.0);
    assert!(
        (doubling - FREE_SPACE_DOUBLING_DB).abs() < 0.1,
        "near-field doubling was {doubling} dB, expected ~{FREE_SPACE_DOUBLING_DB}"
    );
}

#[test]
fn beyond_the_breakpoint_loss_grows_at_twice_the_free_space_rate() {
    // Checked well inside the radio horizon, where the two-ray model is the
    // right one. Comparing against free space rather than against another
    // doubling of the same model keeps the assertion tied to the physics
    // instead of to the mast height that sets the breakpoint.
    let doubling = path_loss_db(10_000.0) - path_loss_db(5_000.0);
    assert!(
        (doubling - TWO_RAY_DOUBLING_DB).abs() < 0.1,
        "far-field doubling was {doubling} dB, expected ~{TWO_RAY_DOUBLING_DB}"
    );
    assert!(doubling > FREE_SPACE_DOUBLING_DB * 1.5);
}

#[test]
fn nothing_is_received_past_the_radio_horizon() {
    // Not "very attenuated" — absent. Masthead antennas see a bulge of water,
    // and a model that returns a finite number past the horizon invites a
    // range claim it cannot support.
    let horizon = radio_horizon_m();
    assert!(
        (25_000.0..32_000.0).contains(&horizon),
        "horizon {horizon} m"
    );
    assert!(path_loss_db(horizon + 1.0).is_infinite());
    assert!(path_loss_db(horizon - 100.0).is_finite());
}

#[test]
fn a_link_within_sensitivity_is_decodable() {
    assert!(rssi_dbm(&Link::new(2_000.0), 14.0) > SENSITIVITY_DBM);
}

#[test]
fn a_link_beyond_the_horizon_is_not_decodable() {
    assert!(rssi_dbm(&Link::new(400_000.0), 14.0) < SENSITIVITY_DBM);
}

#[test]
fn fading_varies_but_stays_bounded() {
    // Rician: a dominant line-of-sight component plus scatter. Over sea the LOS
    // path usually exists, so deep Rayleigh nulls are the wrong model.
    let mut fading = RicianFading::new(42, 6.0);
    let samples: Vec<f64> = (0..200).map(|_| fading.sample_db()).collect();
    assert!(samples.iter().any(|d| *d < 0.0), "some samples must fade");
    assert!(
        samples.iter().any(|d| *d > 0.0),
        "some must add constructively"
    );
    assert!(samples.iter().all(|d| d.abs() < 40.0));
}

#[test]
// Bit-exact comparison is the point: a replayable scenario needs the fading
// sequence to be identical, not merely close.
#[allow(clippy::float_cmp)]
fn fading_is_deterministic_for_a_seed() {
    // Sampled from ONE instance, twice: this has to pin the sequence, not just
    // the constructor. A fresh instance per sample would compare a constant
    // with itself and pass while proving nothing.
    let mut first = RicianFading::new(7, 6.0);
    let a: Vec<f64> = (0..20).map(|_| first.sample_db()).collect();
    let mut second = RicianFading::new(7, 6.0);
    let b: Vec<f64> = (0..20).map(|_| second.sample_db()).collect();
    assert_eq!(a, b);
    assert!(a.windows(2).any(|w| w[0] != w[1]), "the sequence must move");
}

#[test]
fn the_stronger_signal_captures_when_it_is_far_enough_ahead() {
    assert!(capture_wins(-100.0, -100.0 - CAPTURE_THRESHOLD_DB));
    assert!(
        !capture_wins(-100.0, -102.0),
        "2 dB lead is a mutual collision"
    );
    assert!(
        !capture_wins(-100.0, -99.0),
        "the weaker one does not win either"
    );
}

#[test]
fn overlapping_frames_of_similar_strength_destroy_each_other() {
    let target = Transmission::new(0, 1_000, -100.0);
    let clash = Transmission::new(500, 1_000, -101.0);
    assert_eq!(receive(&target, &[clash]), Reception::Collided);
}

#[test]
fn a_strong_frame_survives_a_weak_overlapping_one() {
    let target = Transmission::new(0, 1_000, -90.0);
    let weak = Transmission::new(500, 1_000, -110.0);
    assert_eq!(receive(&target, &[weak]), Reception::Decoded);
}

#[test]
fn frames_that_do_not_overlap_in_time_do_not_interfere() {
    let target = Transmission::new(0, 1_000, -100.0);
    let later = Transmission::new(1_000, 1_000, -100.0);
    assert_eq!(receive(&target, &[later]), Reception::Decoded);
}

#[test]
fn a_frame_below_sensitivity_is_lost_even_on_an_idle_channel() {
    let target = Transmission::new(0, 1_000, SENSITIVITY_DBM - 1.0);
    assert_eq!(receive(&target, &[]), Reception::TooWeak);
}

#[test]
fn a_receiver_is_deaf_while_its_own_radio_transmits() {
    // Half duplex. One antenna, one chain: a node cannot hear the fleet answer
    // while it is still answering itself.
    let target = Transmission::new(0, 1_000, -60.0);
    let own = Transmission::own_transmission(500, 1_000);
    assert_eq!(receive(&target, &[own]), Reception::Collided);
}

#[test]
fn duty_cycle_refuses_a_node_that_has_used_its_hourly_budget() {
    // EU 868 MHz sub-band g1: 1 % per hour, i.e. 36 s of airtime.
    assert_eq!(DUTY_CYCLE_BUDGET_MS, 36_000);
    assert!(duty_cycle_ok(30_000, 1_000));
    assert!(duty_cycle_ok(DUTY_CYCLE_BUDGET_MS, 0));
    assert!(!duty_cycle_ok(DUTY_CYCLE_BUDGET_MS, 1));
}

#[test]
fn the_usable_range_is_limited_by_the_horizon_not_the_link_budget() {
    // Over water the curvature runs out before the signal does: at 14 dBm the
    // budget would still close past 30 km, but nothing is there to hear it.
    use lcq::sim::max_range_m;
    let range = max_range_m(14.0);
    assert!(
        (range - radio_horizon_m()).abs() < 1.0,
        "range {range} m against a horizon of {} m",
        radio_horizon_m()
    );
}

#[test]
fn a_weak_transmitter_is_limited_by_its_budget_instead() {
    use lcq::sim::max_range_m;
    let range = max_range_m(-40.0);
    assert!(
        range < radio_horizon_m(),
        "a 0.1 uW link cannot reach the horizon"
    );
    assert!(
        range > 100.0,
        "it should still reach something, got {range} m"
    );
}
