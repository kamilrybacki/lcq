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

/// The largest difference **between any two honest clocks**, in seconds.
///
/// Pairwise, not per-node. This distinction decides whether [`Clock`] is
/// correct: `certainly_after` concludes that a deadline has passed for
/// *everybody* from the fact that it has passed here by more than this budget,
/// and that inference needs the bound on how far two nodes can differ from each
/// other — not on how far each differs from some reference.
///
/// The deployment consequence follows directly. If nodes discipline their
/// clocks against a common source and each may be up to `e` seconds off it,
/// two of them can be `2e` apart, so the source must hold every node inside
/// **half** this budget: 15 s for the 30 s configured here. A deployment that
/// reads this as a per-node allowance has quietly doubled the real skew and
/// broken every `certainly_after` in the protocol.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 30;

/// Per-node accuracy a time source must hold to honour the pairwise budget.
///
/// Stated as its own constant because it is the number an operator configures,
/// and halving is exactly the step that gets forgotten.
pub const REQUIRED_SOURCE_ACCURACY_SECONDS: u64 = MAX_CLOCK_SKEW_SECONDS / 2;

/// The skew budget must stay well below the consultation window, or two honest
/// nodes could routinely disagree about whether that window is open. Enforced
/// at compile time rather than by a test, because a build that violates it
/// should not exist.
///
/// The factor of ten is a **chosen margin, not a derived bound**. What is
/// actually required is only that the window exceed the uncertainty band, which
/// would allow a ratio near two; below about four the band starts to occupy
/// enough of the window that ordinary drift produces disagreement, and ten
/// leaves room for a deployment to raise the budget without revisiting the
/// cutoff. Recorded plainly because an unexplained constant invites someone to
/// treat it as load-bearing arithmetic.
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
