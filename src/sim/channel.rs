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

/// Time on air for a payload, in milliseconds.
///
/// Semtech's formula for SF10, 125 kHz, CR 4/5, explicit header, with the
/// low-data-rate optimisation on — the profile a mid-range maritime link would
/// plausibly use. An accounting figure for duty-cycle budgeting, not a
/// measurement, and valid only for that one profile.
#[must_use]
pub fn airtime_ms(payload_bytes: usize) -> u64 {
    const SF: f64 = 10.0;
    const BW_HZ: f64 = 125_000.0;
    const CR_DENOM: f64 = 5.0;
    const PREAMBLE_SYMBOLS: f64 = 8.0;

    let symbol_ms = f64::from(1u32 << 10) / BW_HZ * 1_000.0;
    // A LoRa payload is at most 255 bytes, so this conversion is exact; the
    // cap makes that true by construction rather than by assumption.
    let payload = f64::from(u32::try_from(payload_bytes.min(255)).unwrap_or(255));

    // Semtech AN1200.13 payload symbol count, low-data-rate optimisation on.
    let numerator = 8.0 * payload - 4.0 * SF + 28.0 + 16.0;
    let denominator = 4.0 * (SF - 2.0);
    let symbols = 8.0 + (numerator / denominator).ceil().max(0.0) * CR_DENOM;

    let preamble_ms = (PREAMBLE_SYMBOLS + 4.25) * symbol_ms;
    // Airtime for a 255-byte frame at SF12 is a few seconds, so the value
    // always fits comfortably. Clamping keeps the function total without an
    // unchecked cast that clippy would rightly refuse.
    let total = (preamble_ms + symbols * symbol_ms)
        .round()
        .clamp(0.0, 4_294_967_295.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    u64::from(total as u32)
}
