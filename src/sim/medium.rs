//! The shared medium: who gets through when two radios talk at once.
//!
//! One frequency, one channel, no listen-before-talk. Anything that overlaps in
//! time competes, and the outcome depends on relative signal strength *and on
//! when the other frame arrives* (D19). A receiver locks onto a frame during
//! the last symbols of its preamble; an interferer there means it never had
//! the frame, an interferer later means it had it and lost it -- over the
//! header, so the chip reports `HeaderErr`, or over the payload, so it reports
//! `CrcErr`. An interferer that ended before the lock window opened never
//! mattered. This is `LoRaSim`'s rule with the header told apart from the
//! payload; the thresholds are the profile's, and hardware calibrates them.

use crate::application::PhyProfile;
use crate::sim::phy::SENSITIVITY_DBM;

/// Co-channel rejection, in dB: the profile's capture threshold.
///
/// `LoRa`'s chirp demodulator will lock onto the stronger of two overlapping
/// frames once it leads by roughly this margin — the *capture effect*, which is
/// why a crowded channel degrades gracefully instead of falling off a cliff.
/// Below the margin neither frame survives an overlap that matters.
#[allow(clippy::cast_lossless)]
pub const CAPTURE_THRESHOLD_DB: f64 = PhyProfile::eu868_sf10().capture_threshold_db as f64;

/// The explicit header occupies the first block of symbols after the sync
/// word, coded at 4/8 whatever the payload's rate.
const HEADER_SYMBOLS: f64 = 8.0;
/// The sync word and its quarter symbol, after the preamble.
const SYNC_SYMBOLS: f64 = 4.25;

// The duty-cycle constant and check live with the application layer, which is
// what a real transmitter uses; the simulator re-exports them so a scenario
// and a node are metered by exactly the same rule.
pub use crate::application::{DUTY_CYCLE_BUDGET_MS, duty_cycle_ok};

/// Whether the first signal is far enough ahead of the second to be demodulated.
#[must_use]
pub fn capture_wins(stronger_dbm: f64, other_dbm: f64) -> bool {
    stronger_dbm - other_dbm >= CAPTURE_THRESHOLD_DB
}

/// The moments in a frame that decide its fate, from the profile: when the
/// receiver can lock on, where the header ends, how much stronger a frame has
/// to be than an interferer. In the milliseconds the transmissions are timed
/// in, so a test that compresses time scales this too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Acquisition {
    symbol_ms: f64,
    /// Symbols from the start before the lock window opens.
    lead_symbols: f64,
    /// Symbols from the start to the end of the sync word, where the lock is.
    locked_symbols: f64,
    /// Symbols from the start to the end of the header.
    header_end_symbols: f64,
    capture_threshold_db: f64,
}

impl Acquisition {
    /// The timing the profile implies.
    #[must_use]
    pub fn from_profile(profile: &PhyProfile) -> Self {
        let symbol_ms = f64::from(1u32 << profile.spreading_factor.min(12))
            / f64::from(profile.bandwidth_hz)
            * 1_000.0;
        let preamble = f64::from(profile.preamble_symbols);
        let lead = f64::from(
            profile
                .preamble_symbols
                .saturating_sub(profile.preamble_symbols_to_lock),
        );
        let locked = preamble + SYNC_SYMBOLS;
        Self {
            symbol_ms,
            lead_symbols: lead,
            locked_symbols: locked,
            header_end_symbols: locked + HEADER_SYMBOLS,
            capture_threshold_db: f64::from(profile.capture_threshold_db),
        }
    }

    /// The timing of the profile everything else assumes.
    #[must_use]
    pub fn default_profile() -> Self {
        Self::from_profile(&PhyProfile::eu868_sf10())
    }

    /// The same timing with time compressed `scale` times.
    #[must_use]
    pub fn scaled(self, scale: u32) -> Self {
        Self {
            symbol_ms: self.symbol_ms / f64::from(scale.max(1)),
            ..self
        }
    }

    /// One symbol, in these milliseconds.
    #[must_use]
    pub const fn symbol_ms(&self) -> f64 {
        self.symbol_ms
    }

    #[allow(clippy::cast_precision_loss)]
    fn lock_window(&self, frame: &Transmission) -> (f64, f64) {
        let start = frame.start_ms() as f64;
        (
            start + self.lead_symbols * self.symbol_ms,
            start + self.locked_symbols * self.symbol_ms,
        )
    }

    #[allow(clippy::cast_precision_loss)]
    fn header_end(&self, frame: &Transmission) -> f64 {
        frame.start_ms() as f64 + self.header_end_symbols * self.symbol_ms
    }
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

    /// The receiver's own frame: nothing can be heard over it.
    #[must_use]
    pub const fn own_transmission(start_ms: u64, airtime_ms: u64) -> Self {
        Self::new(start_ms, airtime_ms, f64::INFINITY)
    }

    /// When it began.
    #[must_use]
    pub const fn start_ms(&self) -> u64 {
        self.start_ms
    }

    /// How long it lasts.
    #[must_use]
    pub const fn airtime_ms(&self) -> u64 {
        self.airtime_ms
    }

    /// When it ends.
    #[must_use]
    pub const fn end_ms(&self) -> u64 {
        self.start_ms.saturating_add(self.airtime_ms)
    }

    /// How strong it arrives.
    #[must_use]
    pub const fn rssi_dbm(&self) -> f64 {
        self.rssi_dbm
    }

    /// Whether the two share any instant on the air.
    #[must_use]
    pub const fn overlaps(&self, other: &Self) -> bool {
        self.start_ms < other.end_ms() && other.start_ms < self.end_ms()
    }
}

/// How a frame fared at one receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reception {
    /// Locked on and decoded.
    Decoded,
    /// Locked on, then another frame arrived over the payload: the payload CRC
    /// failed and the bytes are noise.
    CrcError,
    /// Locked on, then another frame arrived over the header: the header CRC
    /// failed and nothing was received.
    HeaderError,
    /// Another frame was on the air during the symbols a lock needs, and this
    /// one was not enough stronger to be locked onto over it: the receiver
    /// never had it.
    NoLock,
    /// Below the receiver's sensitivity: nothing was heard.
    TooWeak,
}

impl Reception {
    /// Whether the bytes arrived intact.
    #[must_use]
    pub const fn decoded(self) -> bool {
        matches!(self, Self::Decoded)
    }

    /// Whether the receiver locked on at all: it heard *something*, intact
    /// or not.
    #[must_use]
    pub const fn locked(self) -> bool {
        matches!(self, Self::Decoded | Self::CrcError | Self::HeaderError)
    }
}

/// Decide a frame's fate at one receiver against everything on the air there,
/// with the timing given.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn judge(
    target: &Transmission,
    others: &[Transmission],
    acquisition: &Acquisition,
) -> Reception {
    if target.rssi_dbm() < SENSITIVITY_DBM {
        return Reception::TooWeak;
    }
    let (lock_start, lock_end) = acquisition.lock_window(target);
    let header_end = acquisition.header_end(target);
    let mut worst = Reception::Decoded;
    for other in others {
        if !target.overlaps(other)
            || target.rssi_dbm() - other.rssi_dbm() >= acquisition.capture_threshold_db
        {
            // Not there, or enough weaker to be noise under this frame.
            continue;
        }
        let other_start = other.start_ms() as f64;
        let other_end = other.end_ms() as f64;
        if other_start < lock_end && other_end > lock_start {
            // Over the symbols the lock needs: nothing worse can happen.
            return Reception::NoLock;
        }
        if other_start >= lock_end {
            let outcome = if other_start < header_end {
                Reception::HeaderError
            } else {
                Reception::CrcError
            };
            worst = worst.max(outcome);
        }
        // Otherwise it ended before the lock window opened: the receiver had
        // not begun to listen for this frame yet, and it does not matter.
    }
    worst
}

/// Decide a frame's fate at one receiver under the default profile.
#[must_use]
pub fn receive(target: &Transmission, others: &[Transmission]) -> Reception {
    judge(target, others, &Acquisition::default_profile())
}
