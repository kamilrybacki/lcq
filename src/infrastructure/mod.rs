//! Adapters. Everything here is replaceable without touching a protocol rule.

mod clock;
mod crc;
pub(crate) mod lock_key;
mod log;
mod memory;

pub use clock::{ScaledClock, SystemClock};
pub use log::{LogJournal, OpenError};
pub use memory::MemoryJournal;
