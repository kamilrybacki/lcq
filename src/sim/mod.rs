//! A virtual-time laboratory for the protocol.
//!
//! Deterministic by construction: one seeded generator drives start times,
//! fading and residual loss, so a failing scenario replays exactly. Time is
//! virtual, so a thirty-minute validity window costs microseconds.
//!
//! # What is modelled
//!
//! Frames occupy the channel for a computed airtime and compete for it. Two
//! that overlap destroy each other unless one leads by the capture margin. A
//! node is deaf while its own radio transmits. Signal strength follows a
//! two-ray path-loss model with Rician fading, and a frame below sensitivity is
//! simply not there. Every node has an hourly airtime budget and is refused
//! once it is spent.
//!
//! # What is not
//!
//! The propagation model is a textbook one evaluated over assumed geometry: one
//! mast height, one carrier, one spreading factor, a straight line of nodes. It
//! knows nothing of sea state, ducting, mast sway, superrefraction or any
//! transmitter outside the fleet. It distinguishes a plausible link from a
//! hopeless one. **It supports no claim about real maritime range**, and a
//! figure produced here is not a measurement.

mod channel;
mod medium;
mod phy;
mod rng;
mod scenario;

pub use channel::{DEFAULT_SPREADING_FACTOR, Topology, airtime_ms, airtime_ms_at};
pub use medium::{
    CAPTURE_THRESHOLD_DB, DUTY_CYCLE_BUDGET_MS, Reception, Transmission, capture_wins,
    duty_cycle_ok, receive,
};
pub use phy::{
    ANTENNA_GAIN_DBI, CARRIER_HZ, Link, MAST_HEIGHT_M, MAX_FADE_DB, MIN_FADE_DB, RicianFading,
    SENSITIVITY_DBM, TX_POWER_DBM, breakpoint_m, max_range_m, path_loss_db, radio_horizon_m,
    rssi_dbm, sensitivity_dbm_at,
};
pub use rng::Rng;
pub use scenario::{Outcome, Report, Scenario, TraceEntry};
