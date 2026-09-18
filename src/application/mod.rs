//! Use cases and the ports they need, with no opinion about how they are backed.
//!
//! The domain says what is true; this layer says what a node *does*, and names
//! the storage it needs as a trait so the protocol can be simulated in memory
//! and persisted to a database without either knowing about the other.

mod journal;
mod queue;

pub use journal::{Journal, JournalError, JournalSnapshot, OutgoingFrame};
pub use queue::{Priority, QueueError, RadioQueue};
