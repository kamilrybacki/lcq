//! Clocks that read a real time source.
//!
//! The domain never reaches for one: [`crate::domain::time::Clock`] is injected
//! precisely so the rules can be driven deterministically. These are the
//! adapters a running node uses instead.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::domain::time::{Clock, Timestamp};

/// The system clock, in whole seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Timestamp::from_secs(secs)
    }
}

/// A real clock running faster than wall time, with a per-node offset.
///
/// Two things it is **not**. It is not a simulated clock: it reads a monotonic
/// source that keeps running while the process is descheduled, blocked on a
/// socket or killed and restarted, so a node cannot pretend time stopped for
/// it. And it is not a shortcut around the protocol's deadlines: a 300 s cutoff
/// is still 300 protocol seconds, merely observed sooner.
///
/// Scaling exists because the protocol's shortest interval is five minutes,
/// which makes an honest multi-process test take longer than anyone will run
/// it. The offset exists because every node having the same clock is the
/// fiction that hid a whole class of scheduling failure.
#[derive(Debug, Clone, Copy)]
pub struct ScaledClock {
    origin: Instant,
    epoch: Timestamp,
    scale: u32,
    offset_s: i64,
}

impl ScaledClock {
    /// Start a clock at `epoch`, running `scale` times faster than wall time.
    ///
    /// `offset_s` is this node's error against the others, which the caller
    /// must keep inside [`crate::domain::time::MAX_CLOCK_SKEW_SECONDS`] if it
    /// wants the fleet to behave like an honest one.
    #[must_use]
    pub fn new(epoch: Timestamp, scale: u32, offset_s: i64) -> Self {
        Self::anchored_at(Instant::now(), epoch, scale, offset_s)
    }

    /// A clock whose `epoch` fell at `origin`, which may be in the past.
    ///
    /// For a node that joins a round late and has worked out from a frame it
    /// heard when the round actually began. Time already elapsed since that
    /// instant is elapsed protocol time, not a fresh start.
    #[must_use]
    pub fn anchored_at(origin: Instant, epoch: Timestamp, scale: u32, offset_s: i64) -> Self {
        Self {
            origin,
            epoch,
            scale: scale.max(1),
            offset_s,
        }
    }

    /// Wall-clock milliseconds to wait for `protocol_seconds` to pass here.
    #[must_use]
    pub const fn wall_ms_for(&self, protocol_seconds: u64) -> u64 {
        protocol_seconds.saturating_mul(1_000) / self.scale as u64
    }

    /// Scale protocol milliseconds down to wall-clock milliseconds.
    #[must_use]
    pub const fn wall_ms(&self, protocol_ms: u64) -> u64 {
        protocol_ms / self.scale as u64
    }
}

impl Clock for ScaledClock {
    fn now(&self) -> Timestamp {
        let elapsed = self.origin.elapsed().as_millis();
        // Milliseconds of wall time become `scale` milliseconds of protocol
        // time. Saturating throughout: a clock that wraps is worse than one
        // that stops.
        let protocol_ms = u64::try_from(elapsed)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::from(self.scale));
        let seconds = self.epoch.as_secs().saturating_add(protocol_ms / 1_000);
        Timestamp::from_secs(seconds.saturating_add_signed(self.offset_s))
    }
}
