//! Propagation: how much of the signal survives the trip.
//!
//! Every constant here describes one assumed installation — a masthead whip on
//! a small vessel, EU 868 MHz, SF10 — and the model is a textbook two-ray one.
//! It is a *plausibility* model. It tells you whether a link is roughly viable
//! or roughly hopeless; it is not a prediction of range at sea, which depends on
//! sea state, ducting, mast sway and traffic that none of this represents.

use crate::sim::rng::Rng;

/// Carrier frequency, in hertz: the EU 868 MHz band.
pub const CARRIER_HZ: f64 = 868_100_000.0;

/// Antenna height above the waterline, in metres, assumed equal at both ends.
///
/// This single number sets both the two-ray breakpoint and the radio horizon,
/// so it is the most load-bearing assumption in the module.
pub const MAST_HEIGHT_M: f64 = 12.0;

/// Antenna gain at each end, in dBi: a modest omnidirectional whip.
pub const ANTENNA_GAIN_DBI: f64 = 2.0;

/// Transmit power, in dBm. The 14 dBm ERP ceiling of EU 868 MHz sub-band g1.
pub const TX_POWER_DBM: f64 = 14.0;

/// Receiver sensitivity, in dBm, at the default spreading factor.
///
/// Tied to the same profile as [`crate::sim::airtime_ms`]: both describe SF10 /
/// 125 kHz, and changing the spreading factor must change both together. Use
/// [`sensitivity_dbm_at`] to ask about another one.
pub const SENSITIVITY_DBM: f64 = sensitivity_dbm_at(crate::sim::DEFAULT_SPREADING_FACTOR);

/// Receiver sensitivity, in dBm, for a spreading factor at 125 kHz.
///
/// The SX1276 datasheet figures. Each step buys roughly 2.5 dB, which is the
/// number that decides whether a slower spreading factor is worth its airtime:
/// over water, where loss grows 12 dB per doubling of distance, 2.5 dB is only
/// about 1.16x the range.
#[must_use]
pub const fn sensitivity_dbm_at(spreading_factor: u8) -> f64 {
    match spreading_factor {
        0..=7 => -123.0,
        8 => -126.0,
        9 => -129.0,
        10 => -132.0,
        11 => -134.5,
        _ => -137.0,
    }
}

const SPEED_OF_LIGHT_MS: f64 = 299_792_458.0;

/// Distance, in metres, past which the model reports no link at all.
///
/// The standard `4.12 * (sqrt(h1) + sqrt(h2))` kilometre approximation, which
/// folds in atmospheric refraction. Past this the two-ray model is not merely
/// pessimistic, it is invalid — so the module refuses to return a number rather
/// than returning one that could be mistaken for a range estimate.
#[must_use]
pub fn radio_horizon_m() -> f64 {
    4_120.0 * (MAST_HEIGHT_M.sqrt() + MAST_HEIGHT_M.sqrt())
}

/// Distance, in metres, where the sea-surface reflection starts to dominate.
#[must_use]
pub fn breakpoint_m() -> f64 {
    let wavelength = SPEED_OF_LIGHT_MS / CARRIER_HZ;
    4.0 * MAST_HEIGHT_M * MAST_HEIGHT_M / wavelength
}

/// Median path loss in dB, or infinity past the radio horizon.
///
/// Two regimes. Inside the breakpoint the direct ray dominates and loss follows
/// free space, 6 dB per doubling. Outside it the sea-surface reflection arrives
/// antiphase and cancels much of the direct path, giving 12 dB per doubling —
/// which is why a link that works at 2 km is not a quarter as good at 4 km but
/// a sixteenth.
#[must_use]
pub fn path_loss_db(distance_m: f64) -> f64 {
    if distance_m.is_nan() || distance_m <= 1.0 {
        // Also catches NaN. Inside a metre there is no propagation to model.
        return 0.0;
    }
    if distance_m > radio_horizon_m() {
        return f64::INFINITY;
    }
    let breakpoint = breakpoint_m();
    if distance_m <= breakpoint {
        free_space_db(distance_m)
    } else {
        free_space_db(breakpoint) + 40.0 * (distance_m / breakpoint).log10()
    }
}

fn free_space_db(distance_m: f64) -> f64 {
    let wavelength = SPEED_OF_LIGHT_MS / CARRIER_HZ;
    20.0 * (4.0 * core::f64::consts::PI * distance_m / wavelength).log10()
}

/// The furthest a link still decodes, in metres.
///
/// Found by bisection rather than algebra because the loss model has two
/// regimes and a hard horizon, and a closed form would have to special-case
/// both. Returns the horizon when the link budget outlasts the geometry, which
/// over water it does: the curvature runs out before the signal does.
#[must_use]
pub fn max_range_m(tx_power_dbm: f64) -> f64 {
    let horizon = radio_horizon_m();
    if rssi_dbm(&Link::new(horizon - 1.0), tx_power_dbm) >= SENSITIVITY_DBM {
        return horizon;
    }
    let (mut near, mut far) = (1.0f64, horizon);
    for _ in 0..60 {
        let middle = f64::midpoint(near, far);
        if rssi_dbm(&Link::new(middle), tx_power_dbm) >= SENSITIVITY_DBM {
            near = middle;
        } else {
            far = middle;
        }
    }
    near
}

/// One radio path, identified by the distance it has to cross.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    distance_m: f64,
}

impl Link {
    /// A link over the given distance in metres.
    #[must_use]
    pub const fn new(distance_m: f64) -> Self {
        Self { distance_m }
    }

    /// How far the signal has to travel, in metres.
    #[must_use]
    pub const fn distance_m(self) -> f64 {
        self.distance_m
    }
}

/// Median received power in dBm, before fading.
#[must_use]
pub fn rssi_dbm(link: &Link, tx_power_dbm: f64) -> f64 {
    tx_power_dbm + 2.0 * ANTENNA_GAIN_DBI - path_loss_db(link.distance_m())
}

/// The deepest fade the model will report, in dB.
///
/// A `LoRa` frame spans hundreds of milliseconds across many chirp symbols, so a
/// single scalar per frame is a *frame-averaged* fade, and frame-averaged gain
/// is far narrower than the instantaneous gain a Rician draw describes. The
/// bounds keep the scalar in the range that averaging actually produces instead
/// of letting a momentary null stand in for a whole frame.
pub const MIN_FADE_DB: f64 = -30.0;

/// The strongest constructive addition the model will report, in dB.
pub const MAX_FADE_DB: f64 = 10.0;

/// Rician fading: one dominant path plus diffuse scatter.
///
/// Over open water a line-of-sight component almost always exists, so Rayleigh —
/// which assumes it does not — would model the wrong sea. The K factor is the
/// ratio of dominant to scattered power; higher means a steadier link.
#[derive(Debug, Clone)]
pub struct RicianFading {
    rng: Rng,
    line_of_sight: f64,
    scatter: f64,
}

impl RicianFading {
    /// Fading with the given K factor in dB, driven by a seeded generator.
    #[must_use]
    pub fn new(seed: u64, k_factor_db: f64) -> Self {
        let k = 10f64.powf(k_factor_db / 10.0);
        // Normalised so mean received power is unchanged: the fade redistributes
        // power over time, it does not invent or destroy any.
        Self {
            rng: Rng::new(seed),
            line_of_sight: (k / (k + 1.0)).sqrt(),
            scatter: (1.0 / (2.0 * (k + 1.0))).sqrt(),
        }
    }

    /// The next gain deviation, in dB, to add to a median RSSI.
    pub fn sample_db(&mut self) -> f64 {
        let (real, imaginary) = self.rng.normal_pair();
        let in_phase = self.line_of_sight + self.scatter * real;
        let quadrature = self.scatter * imaginary;
        let amplitude = in_phase.mul_add(in_phase, quadrature * quadrature).sqrt();
        (20.0 * amplitude.log10()).clamp(MIN_FADE_DB, MAX_FADE_DB)
    }
}
