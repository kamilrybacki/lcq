//! Endorsement quorum: how many members, and how much manifest weight, must agree.
//!
//! This bounded context owns two thresholds and nothing else. It does not know
//! who sent a message, whether a signature verified, when it arrived, or what
//! revision it refers to — all of which belong to layers that must run *before*
//! anything here is consulted.

mod evaluation;
mod policy;

pub use evaluation::{EvaluationError, QuorumResult, evaluate};
pub use policy::{Policy, PolicyError};
