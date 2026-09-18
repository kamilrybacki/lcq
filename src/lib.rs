//! `lorai` — known-membership endorsement protocol for constrained radio networks.
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
pub mod wire;

pub use domain::quorum::{EvaluationError, Policy, PolicyError, QuorumResult, evaluate};
