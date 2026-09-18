//! Time as the protocol may reason about it: bounded, injected, and never certain.
//!
//! Clocks are synchronised before a mission and drift afterwards. Rather than
//! pretend a node knows the time, every deadline question has three answers —
//! certainly before, certainly after, and *not knowable within the skew budget*.
//!
//! That third answer is load-bearing. The design is explicit: when time
//! uncertainty prevents safely establishing validity, a node does not cast a
//! binding vote. It still raises a local warning, because refusing to vote is
//! not the same as deciding there is no danger.

/// Tolerated one-way clock error between two honest nodes, in seconds.
///
/// Chosen against the 5-minute consultation cutoff: at 30 s the uncertainty
/// band is a tenth of the window, so two honest nodes cannot disagree about
/// whether the window is open unless one of them is far outside the budget.
/// A budget approaching the cutoff would make disagreement routine.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 30;

/// The skew budget must stay an order of magnitude below the consultation
/// window, or two honest nodes could routinely disagree about whether that
/// window is open. Enforced at compile time rather than by a test, because a
/// build that violates it should not exist.
const _: () =
    assert!(MAX_CLOCK_SKEW_SECONDS * 10 <= crate::domain::contracts::CONSULTATION_CUTOFF_SECONDS);

/// A protocol instant: whole seconds since the mission epoch.
///
/// Seconds, not milliseconds — the shortest protocol interval is minutes, and
/// finer resolution would buy nothing while costing wire bytes that a `LoRa`
/// payload does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Build a timestamp from seconds since the mission epoch.
    #[must_use]
    pub const fn from_secs(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Seconds since the mission epoch.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0
    }

    /// A later instant, saturating at the maximum.
    ///
    /// Saturating rather than wrapping: a corrupt or hostile validity field
    /// must not be able to fold a deadline around into the past, which would
    /// turn "valid for another century" into "expired".
    #[must_use]
    pub const fn plus_secs(self, seconds: u64) -> Self {
        Self(self.0.saturating_add(seconds))
    }

    /// Seconds from `self` to `later`, or zero if `later` is not later.
    #[must_use]
    pub const fn secs_until(self, later: Self) -> u64 {
        later.0.saturating_sub(self.0)
    }
}

/// A source of the current protocol time.
///
/// Injected rather than read from the system so that the state machine can be
/// driven deterministically in a simulator, and so that no domain rule reaches
/// for a real clock.
pub trait Clock {
    /// The node's own reading of the current time.
    fn now(&self) -> Timestamp;

    /// Whether `deadline` has passed by more than any honest clock could differ.
    fn certainly_after(&self, deadline: Timestamp) -> bool {
        self.now().as_secs() > deadline.as_secs().saturating_add(MAX_CLOCK_SKEW_SECONDS)
    }

    /// Whether `deadline` is still ahead by more than any honest clock could differ.
    fn certainly_before(&self, deadline: Timestamp) -> bool {
        self.now().as_secs().saturating_add(MAX_CLOCK_SKEW_SECONDS) < deadline.as_secs()
    }

    /// Whether the skew budget leaves the answer genuinely unknown.
    ///
    /// A node in this band must not cast a binding vote.
    fn uncertain_about(&self, deadline: Timestamp) -> bool {
        !self.certainly_after(deadline) && !self.certainly_before(deadline)
    }
}

/// A clock frozen at one instant, for tests and for virtual-time simulation.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    now: Timestamp,
}

impl FixedClock {
    /// A clock that always reports `now`.
    #[must_use]
    pub const fn new(now: Timestamp) -> Self {
        Self { now }
    }

    /// Move the clock forward.
    #[must_use]
    pub const fn advanced_by(self, seconds: u64) -> Self {
        Self {
            now: self.now.plus_secs(seconds),
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.now
    }
}
