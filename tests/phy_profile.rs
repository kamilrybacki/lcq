//! The frozen profile: every figure the crate assumes about the channel, in one
//! place, pinned. An emulator run and a hardware run are compared on this and
//! nothing else.

use lcq::application::PhyProfile;
use lcq::sim::{
    CAPTURE_THRESHOLD_DB, CARRIER_HZ, DEFAULT_SPREADING_FACTOR, TX_POWER_DBM, airtime_ms,
    sensitivity_dbm_at, snr_db,
};
use lcq::wire::MAX_FRAME_BYTES;
use lora_modulation::{Bandwidth, BaseBandModulationParams, CodingRate, SpreadingFactor};

#[test]
fn the_profile_is_the_one_everything_was_measured_on() {
    let profile = PhyProfile::eu868_sf10();
    assert_eq!(profile.name, "eu868-sf10-v1");
    assert_eq!(profile.spreading_factor, 10);
    assert_eq!(profile.bandwidth_hz, 125_000);
    assert_eq!(profile.coding_rate_denominator, 5);
    assert_eq!(profile.preamble_symbols, 8);
    assert!(profile.explicit_header);
    assert!(profile.crc_on);
    assert!(!profile.iq_inverted);
    assert!(!profile.low_data_rate_optimize);
    assert_eq!(profile.frequency_hz, 868_100_000);
    assert_eq!(profile.tx_power_dbm, 14);
    assert_eq!(profile.sync_word, 0x12);
    assert!(profile.rx_continuous);
    assert_eq!(
        (
            profile.cad_symbols,
            profile.cad_detect_peak,
            profile.cad_detect_min
        ),
        (8, 23, 10)
    );
    assert_eq!(profile.preamble_symbols_to_lock, 6);
    assert_eq!(profile.capture_threshold_db, 6);
}

#[test]
fn the_simulation_describes_the_same_profile() {
    let profile = PhyProfile::eu868_sf10();
    assert_eq!(DEFAULT_SPREADING_FACTOR, profile.spreading_factor);
    assert!((CARRIER_HZ - f64::from(profile.frequency_hz)).abs() < f64::EPSILON);
    assert!((TX_POWER_DBM - f64::from(profile.tx_power_dbm)).abs() < f64::EPSILON);
    assert!((CAPTURE_THRESHOLD_DB - f64::from(profile.capture_threshold_db)).abs() < f64::EPSILON);
    assert!((sensitivity_dbm_at(profile.spreading_factor) - (-132.0)).abs() < f64::EPSILON);
    // At the sensitivity floor the reported SNR is the SF10 demodulation limit.
    assert!((snr_db(-132.0) - (-15.0)).abs() < f64::EPSILON);
}

/// The chip's own time-on-air rule, from the driver's crate.
fn chip_airtime_ms(payload: u8) -> f64 {
    let profile = PhyProfile::eu868_sf10();
    let params =
        BaseBandModulationParams::new(SpreadingFactor::_10, Bandwidth::_125KHz, CodingRate::_4_5);
    assert_eq!(params.ldro, profile.low_data_rate_optimize);
    let preamble = u8::try_from(profile.preamble_symbols).expect("fits");
    f64::from(params.time_on_air_us(Some(preamble), profile.explicit_header, payload)) / 1_000.0
}

#[test]
fn the_emulator_and_the_chip_agree_on_airtime() {
    for payload in [10u8, 32, 77, 105, 137, 176, 255] {
        let hub = airtime_ms(usize::from(payload));
        let chip = chip_airtime_ms(payload);
        assert!(
            (f64::from(u32::try_from(hub).expect("airtime fits u32")) - chip).abs() <= 1.0,
            "{payload} bytes: hub says {hub} ms, chip says {chip:.1} ms"
        );
    }
    assert!(
        u8::try_from(MAX_FRAME_BYTES).is_ok(),
        "the largest frame fits the chip"
    );
}

#[test]
fn one_symbol_is_8192_microseconds() {
    let params =
        BaseBandModulationParams::new(SpreadingFactor::_10, Bandwidth::_125KHz, CodingRate::_4_5);
    assert_eq!(params.symbol_duration_us(), 8_192);
}
