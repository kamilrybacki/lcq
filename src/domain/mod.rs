//! Protocol rules that hold regardless of radio, storage or wire format.
//!
//! A module here may not reach for a transport, a database or a system clock.
//! Keeping that boundary is what lets the rules be reviewed on their own terms
//! rather than by running a fleet.

pub mod quorum;
