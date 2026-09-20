//! Use cases and the ports they need, with no opinion about how they are backed.
//!
//! The domain says what is true; this layer says what a node *does*, and names
//! the storage it needs as a trait so the protocol can be simulated in memory
//! and persisted to a database without either knowing about the other.

mod budget;
mod journal;
mod manifest;
mod queue;
mod radio;
mod replay;

pub use budget::{AirtimeBudget, DUTY_CYCLE_BUDGET_MS, DUTY_CYCLE_WINDOW_MS, duty_cycle_ok};
pub use journal::{Journal, JournalError, JournalSnapshot, OutgoingFrame};
pub use manifest::{MANIFEST_VERSION, Manifest, ManifestError, Member};
pub use queue::{Priority, QueueError, RadioQueue};
pub use radio::{PhyProfile, Radio, RadioError, RadioEvent, Received};
pub use replay::{REPLAY_WINDOW, ReplayWindow};
