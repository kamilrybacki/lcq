//! What a spreading factor costs, measured rather than assumed.

use lorai::sim::{DUTY_CYCLE_BUDGET_MS, airtime_ms, airtime_ms_at, sensitivity_dbm_at};

/// A signed, compacted endorsement frame, as `examples/spreading` measures it.
const FRAME_BYTES: usize = 105;

/// Loss per doubling of distance in the two-ray far field over water.
const DB_PER_DOUBLING: f64 = 12.04;

/// Ratio of two airtimes.
///
/// Airtimes are milliseconds in the thousands, far inside f64's exact integer
/// range, so the conversion loses nothing.
#[allow(clippy::cast_precision_loss)]
fn ratio(slower: u64, faster: u64) -> f64 {
    slower as f64 / faster as f64
}

fn reach_against_sf10(spreading_factor: u8) -> f64 {
    let gain = sensitivity_dbm_at(10) - sensitivity_dbm_at(spreading_factor);
    2f64.powf(gain / DB_PER_DOUBLING)
}

#[test]
fn the_default_profile_agrees_with_the_explicit_one() {
    assert_eq!(airtime_ms(FRAME_BYTES), airtime_ms_at(10, FRAME_BYTES));
}

#[test]
fn airtime_roughly_doubles_with_each_spreading_factor() {
    for sf in 7..10u8 {
        let step = ratio(
            airtime_ms_at(sf + 1, FRAME_BYTES),
            airtime_ms_at(sf, FRAME_BYTES),
        );
        assert!(
            (1.7..2.0).contains(&step),
            "SF{sf} -> SF{}: ratio {step:.2}",
            sf + 1
        );
    }
}

#[test]
fn the_low_data_rate_optimisation_costs_extra_above_sf10() {
    // It is enabled only where the symbol time passes about 16 ms, which at
    // 125 kHz means SF11 and SF12. It shortens the usable symbol, so those two
    // cost MORE than the plain doubling. Applying it at SF10 as well -- which
    // this crate used to do -- inflated every airtime figure, and airtime is
    // what the duty cycle is spent on.
    let sf10_to_sf11 = ratio(
        airtime_ms_at(11, FRAME_BYTES),
        airtime_ms_at(10, FRAME_BYTES),
    );
    assert!(
        sf10_to_sf11 > 2.0,
        "SF11 should cost more than double SF10, got {sf10_to_sf11:.2}"
    );
}

#[test]
fn sf12_buys_little_range_for_what_it_costs() {
    let cost = ratio(
        airtime_ms_at(12, FRAME_BYTES),
        airtime_ms_at(10, FRAME_BYTES),
    );
    let gain = reach_against_sf10(12);
    assert!(cost > 3.5, "SF12 airtime multiple was {cost:.2}");
    assert!(gain < 1.5, "SF12 range multiple was {gain:.2}");
}

#[test]
fn two_sf10_hops_beat_one_sf12_hop_on_both_airtime_and_range() {
    // The decision in D2, as an assertion. Relaying is not extra machinery: the
    // protocol already stores and forwards, and a signature makes a relay unable
    // to alter what it carries.
    let two_hops = 2 * airtime_ms_at(10, FRAME_BYTES);
    let one_hop = airtime_ms_at(12, FRAME_BYTES);
    assert!(
        two_hops < one_hop,
        "two SF10 hops cost {two_hops} ms against {one_hop} ms for one SF12 hop"
    );
    assert!(
        reach_against_sf10(12) < 2.0,
        "two hops reach twice as far; SF12 reaches {:.2}x",
        reach_against_sf10(12)
    );
}

#[test]
fn the_hourly_budget_barely_covers_a_single_contested_sf12_vote() {
    // Five attempts is the retry ceiling. At SF12 one contested vote can eat
    // more than half of everything a node is allowed to transmit in an hour.
    let attempt = airtime_ms_at(12, FRAME_BYTES);
    let contested = attempt * 5;
    assert!(
        contested > DUTY_CYCLE_BUDGET_MS / 2,
        "one contested SF12 vote costs {contested} ms of a {DUTY_CYCLE_BUDGET_MS} ms budget"
    );
    assert!(
        DUTY_CYCLE_BUDGET_MS / attempt < 10,
        "a node gets fewer than ten SF12 frames an hour"
    );
}

#[test]
fn sf10_leaves_room_for_the_traffic_endorsement_is_not() {
    // Endorsement shares the budget with position reports and application
    // messages. SF10 leaves most of the hour for them; SF12 does not.
    let attempt = airtime_ms(FRAME_BYTES);
    assert!(
        DUTY_CYCLE_BUDGET_MS / attempt >= 30,
        "SF10 should allow at least thirty frames an hour"
    );
}

#[test]
fn a_faster_spreading_factor_is_less_sensitive() {
    for sf in 7..12u8 {
        assert!(
            sensitivity_dbm_at(sf) > sensitivity_dbm_at(sf + 1),
            "SF{} must hear more than SF{sf}",
            sf + 1
        );
    }
}
