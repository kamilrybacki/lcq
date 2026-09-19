//! The radio, as much of it as is honest to model.

/// How nodes can reach each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// Every node hears every other.
    FullyConnected,
    /// Two halves that cannot hear each other at all.
    Partitioned,
}

impl Topology {
    /// Whether `from` can be heard by `to` at all.
    #[must_use]
    pub fn reaches(self, from: usize, to: usize, fleet: usize) -> bool {
        match self {
            Self::FullyConnected => from != to,
            Self::Partitioned => from != to && (from < fleet / 2) == (to < fleet / 2),
        }
    }
}

/// The spreading factor the rest of the crate assumes.
///
/// Changing this changes [`crate::sim::SENSITIVITY_DBM`] too: the two describe
/// one radio profile and are meaningless apart.
pub const DEFAULT_SPREADING_FACTOR: u8 =
    crate::application::PhyProfile::eu868_sf10().spreading_factor;

/// Time on air for a payload at the default spreading factor, in milliseconds.
#[must_use]
pub fn airtime_ms(payload_bytes: usize) -> u64 {
    airtime_ms_at(DEFAULT_SPREADING_FACTOR, payload_bytes)
}

/// Time on air at a given spreading factor, in milliseconds.
///
/// Semtech AN1200.13 for 125 kHz, CR 4/5, explicit header, 8 preamble symbols.
/// The low-data-rate optimisation is applied at SF11 and SF12 and nowhere else,
/// which is the rule the chip actually follows: it exists to cope with symbol
/// times past about 16 ms, and at 125 kHz only those two spreading factors
/// reach that. Applying it lower inflates the airtime and, since airtime is
/// what the duty cycle is spent on, would make the whole budget wrong.
///
/// An accounting figure, not a measurement, and valid only for that profile.
#[must_use]
pub fn airtime_ms_at(spreading_factor: u8, payload_bytes: usize) -> u64 {
    const BW_HZ: f64 = 125_000.0;
    const CR_DENOM: f64 = 5.0;
    const PREAMBLE_SYMBOLS: f64 = 8.0;

    let sf = f64::from(spreading_factor.clamp(7, 12));
    let symbol_ms = f64::from(1u32 << spreading_factor.clamp(7, 12)) / BW_HZ * 1_000.0;
    let low_data_rate = if spreading_factor >= 11 { 1.0 } else { 0.0 };

    // A LoRa payload is at most 255 bytes, so this conversion is exact; the cap
    // makes that true by construction rather than by assumption.
    let payload = f64::from(u32::try_from(payload_bytes.min(255)).unwrap_or(255));

    let numerator = 8.0 * payload - 4.0 * sf + 28.0 + 16.0;
    let denominator = 4.0 * (sf - 2.0 * low_data_rate);
    let symbols = 8.0 + (numerator / denominator).ceil().max(0.0) * CR_DENOM;

    let preamble_ms = (PREAMBLE_SYMBOLS + 4.25) * symbol_ms;
    // Airtime for a 255-byte frame at SF12 is a few seconds, so the value always
    // fits. Clamping keeps the function total without an unchecked cast.
    let total = (preamble_ms + symbols * symbol_ms)
        .round()
        .clamp(0.0, 4_294_967_295.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    u64::from(total as u32)
}
