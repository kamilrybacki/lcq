//! A virtual-time laboratory for the protocol.
//!
//! Deterministic by construction: a seeded generator drives loss, so a failing
//! scenario can be replayed exactly. Time is virtual, so a thirty-minute
//! validity window costs microseconds.
//!
//! # What this does not model
//!
//! Loss here is an independent per-link probability. That is **not** a `LoRa` PHY
//! model: it has no collisions, no capture effect, no path loss, no fading and
//! no duty-cycle enforcement. Airtime is computed from a published formula for
//! one spreading factor, so it is an accounting figure rather than a
//! measurement. Nothing here supports a claim about real maritime range.

mod channel;
mod scenario;

pub use channel::{Topology, airtime_ms};
pub use scenario::{Report, Scenario};
