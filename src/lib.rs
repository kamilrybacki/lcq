//! `lcq` — the LoRa-based Confidence Quorum Protocol.
//!
//! A known-membership endorsement protocol for constrained radio networks: a
//! fleet of vessels that already know each other's keys agree, over one narrow
//! `LoRa` channel, whether a warning has enough confident support behind it to
//! be acted on. Membership is fixed, so the question is never who may speak but
//! whether enough of them committed.
//!
//! Layered deliberately. [`domain`] holds the rules a reviewer has to trust and
//! depends on nothing that needs a running system: no radio, no storage, no
//! clock. Later layers wire those rules to a transport and a journal.
//!
//! Nothing in this crate authenticates anything yet. See [`domain::quorum`].

#![forbid(unsafe_code)]

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod sim;
pub mod wire;

pub use domain::quorum::{EvaluationError, Policy, PolicyError, QuorumResult, evaluate};
