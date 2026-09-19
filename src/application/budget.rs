//! A transmitter's legal airtime, metered over a sliding hour.
//!
//! EU 868 MHz sub-band g1 permits a 1 % duty cycle: 36 s of transmission in any
//! 3600 s. Exceeding it is not a performance problem, it is unlawful
//! transmission, so a node refuses the frame rather than sending it. The
//! simulator has enforced this since the radio model was built; a real node
//! transmits more than the simulator ever did -- retries, repair requests,
//! resends on request -- and had no meter at all.

use alloc::collections::VecDeque;

extern crate alloc;

/// Airtime a node may use per hour, in milliseconds.
pub const DUTY_CYCLE_BUDGET_MS: u64 = 36_000;

/// The hour the budget is measured over, in milliseconds.
pub const DUTY_CYCLE_WINDOW_MS: u64 = 3_600_000;

/// Whether a node may legally send `next_ms` more airtime given `used_ms`
/// already spent in the current window.
#[must_use]
pub const fn duty_cycle_ok(used_ms: u64, next_ms: u64) -> bool {
    used_ms.saturating_add(next_ms) <= DUTY_CYCLE_BUDGET_MS
}

/// What one node has put on the air in the last hour of protocol time.
///
/// Time is supplied by the caller in protocol milliseconds and must not go
/// backwards; the meter is a property of the transmitter, not of any clock the
/// protocol reasons about, so it takes no [`crate::domain::time::Clock`].
#[derive(Debug, Clone, Default)]
pub struct AirtimeBudget {
    /// `(sent at, airtime)` for every transmission still inside the window.
    sent: VecDeque<(u64, u64)>,
    total_ms: u64,
}

impl AirtimeBudget {
    /// Nothing spent yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A budget with `prior_ms` already spent at time zero.
    ///
    /// Endorsement is not the only traffic a node carries; this is how a test
    /// puts a node on the air with most of its hour already gone.
    #[must_use]
    pub fn with_prior(prior_ms: u64) -> Self {
        let mut budget = Self::default();
        if prior_ms > 0 {
            budget.charge(0, prior_ms);
        }
        budget
    }

    /// Airtime spent in the window ending at `now_ms`.
    pub fn used_ms(&mut self, now_ms: u64) -> u64 {
        self.evict(now_ms);
        self.total_ms
    }

    /// Whether `air_ms` more may legally go out at `now_ms`.
    pub fn allows(&mut self, now_ms: u64, air_ms: u64) -> bool {
        self.evict(now_ms);
        duty_cycle_ok(self.total_ms, air_ms)
    }

    /// Record a transmission of `air_ms` at `now_ms`.
    pub fn charge(&mut self, now_ms: u64, air_ms: u64) {
        self.sent.push_back((now_ms, air_ms));
        self.total_ms = self.total_ms.saturating_add(air_ms);
    }

    /// Charge for a transmission if it is allowed, and say whether it was.
    ///
    /// The only method a transmitter needs: a `false` means the frame does not
    /// go out, and nothing was recorded for it.
    pub fn transmit(&mut self, now_ms: u64, air_ms: u64) -> bool {
        if !self.allows(now_ms, air_ms) {
            return false;
        }
        self.charge(now_ms, air_ms);
        true
    }

    /// Forget transmissions that have left the window.
    fn evict(&mut self, now_ms: u64) {
        let horizon = now_ms.saturating_sub(DUTY_CYCLE_WINDOW_MS);
        while let Some(&(at, air)) = self.sent.front() {
            if at >= horizon {
                break;
            }
            self.sent.pop_front();
            self.total_ms = self.total_ms.saturating_sub(air);
        }
    }
}
