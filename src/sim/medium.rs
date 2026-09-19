//! The shared medium: who gets through when two radios talk at once.
//!
//! One frequency, one channel, no listen-before-talk. Anything that overlaps in
//! time competes, and the outcome is decided by relative signal strength alone.

use crate::sim::phy::SENSITIVITY_DBM;

/// Co-channel rejection, in dB.
///
/// `LoRa`'s chirp demodulator will lock onto the stronger of two overlapping
/// frames once it leads by roughly this margin — the *capture effect*, which is
/// why a crowded channel degrades gracefully instead of falling off a cliff.
/// Below the margin neither frame survives.
pub const CAPTURE_THRESHOLD_DB: f64 = 6.0;

// The duty-cycle constant and check live with the application layer, which is
// what a real transmitter uses; the simulator re-exports them so a scenario
// and a node are metered by exactly the same rule.
pub use crate::application::{DUTY_CYCLE_BUDGET_MS, duty_cycle_ok};

/// Whether the first signal is far enough ahead of the second to be demodulated.
#[must_use]
pub fn capture_wins(stronger_dbm: f64, other_dbm: f64) -> bool {
    stronger_dbm - other_dbm >= CAPTURE_THRESHOLD_DB
}

/// One frame occupying the channel, as seen at a particular receiver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transmission {
    start_ms: u64,
    airtime_ms: u64,
    rssi_dbm: f64,
}

impl Transmission {
    /// A frame starting at `start_ms`, lasting `airtime_ms`, arriving at
    /// `rssi_dbm` at the receiver being modelled.
    #[must_use]
    pub const fn new(start_ms: u64, airtime_ms: u64, rssi_dbm: f64) -> Self {
        Self {
            start_ms,
            airtime_ms,
            rssi_dbm,
        }
    }

    /// The receiver's own outgoing frame.
    ///
    /// Modelled as arbitrarily strong so that nothing can capture over it: a
    /// half-duplex radio with one antenna and one chain is simply deaf while it
    /// transmits, no matter how loud the other station is.
    #[must_use]
    pub const fn own_transmission(start_ms: u64, airtime_ms: u64) -> Self {
        Self::new(start_ms, airtime_ms, f64::INFINITY)
    }

    /// When the frame started, in milliseconds after the trigger.
    #[must_use]
    pub const fn start_ms(&self) -> u64 {
        self.start_ms
    }

    /// How long the frame holds the channel, in milliseconds.
    #[must_use]
    pub const fn airtime_ms(&self) -> u64 {
        self.airtime_ms
    }

    /// When the frame leaves the air.
    #[must_use]
    pub const fn end_ms(&self) -> u64 {
        self.start_ms.saturating_add(self.airtime_ms)
    }

    /// Received power at the modelled receiver.
    #[must_use]
    pub const fn rssi_dbm(&self) -> f64 {
        self.rssi_dbm
    }

    /// Whether the two frames share any time on the channel.
    #[must_use]
    pub const fn overlaps(&self, other: &Self) -> bool {
        self.start_ms < other.end_ms() && other.start_ms < self.end_ms()
    }
}

/// What became of a frame at the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reception {
    /// Received intact.
    Decoded,
    /// Arrived below the demodulator's sensitivity.
    TooWeak,
    /// Overlapped by a frame it could not capture over.
    Collided,
}

/// Decide the fate of `target` given everything else on the channel.
///
/// `others` must contain only frames that actually reach this receiver. A node
/// out of earshot cannot jam what it cannot reach — which is also how a hidden
/// terminal gets to cause damage at one receiver and not another.
#[must_use]
pub fn receive(target: &Transmission, others: &[Transmission]) -> Reception {
    if target.rssi_dbm() < SENSITIVITY_DBM {
        return Reception::TooWeak;
    }
    for other in others {
        if target.overlaps(other) && !capture_wins(target.rssi_dbm(), other.rssi_dbm()) {
            return Reception::Collided;
        }
    }
    Reception::Decoded
}
