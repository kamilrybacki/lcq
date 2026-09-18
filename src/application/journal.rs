//! The safety journal: what must be true after a crash.
//!
//! Two invariants justify this whole layer.
//!
//! **A vote lock is taken before its frame is exposed for transmission.** If the
//! frame were queued first and the node died, it could restart, see no lock, and
//! vote a second time — while the first vote was already on the air. Two votes
//! from one member breaks the quorum-intersection argument that the count
//! threshold rests on.
//!
//! **A sequence number is never reused.** Under an existing key, a reused
//! sequence is a reused nonce, which is a decryption oracle rather than an
//! inconvenience. A restart must move the counter forward, never rewind it.

use alloc::vec::Vec;
use core::fmt;

use crate::domain::contracts::Subject;
use crate::domain::time::Timestamp;

extern crate alloc;

/// Why a journal operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    /// This node already holds a vote lock for this case.
    AlreadyVoted,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyVoted => f.write_str("a binding vote is already recorded for this case"),
        }
    }
}

impl core::error::Error for JournalError {}

/// Bytes waiting to go out, with the sequence number they were reserved under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingFrame {
    bytes: Vec<u8>,
    sequence: u64,
    expires_at: Option<Timestamp>,
}

impl OutgoingFrame {
    /// A frame that does not expire.
    #[must_use]
    pub fn new(bytes: Vec<u8>, sequence: u64) -> Self {
        Self {
            bytes,
            sequence,
            expires_at: None,
        }
    }

    /// The same frame, discarded rather than sent after `when`.
    ///
    /// Forwarding does not extend a signed validity, so a frame that outlived
    /// its subject is not worth the airtime it would cost.
    #[must_use]
    pub fn expiring_at(mut self, when: Timestamp) -> Self {
        self.expires_at = Some(when);
        self
    }

    /// Encoded bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The sequence number reserved for this frame.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// When this frame stops being worth sending.
    #[must_use]
    pub fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
    }
}

/// A restorable view of everything that must survive a restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalSnapshot {
    /// `(mission, event, revision, content hash, voter)` for every held lock.
    pub vote_locks: Vec<(alloc::string::String, u64)>,
    /// Frames not yet acknowledged.
    pub pending: Vec<OutgoingFrame>,
    /// The next sequence number that has never been handed out.
    pub next_sequence: u64,
}

/// Durable safety state.
///
/// Implementations must make [`Journal::commit_vote`] atomic: either the lock
/// and the frame are both recorded, or neither is. A partial write that queues
/// the frame without the lock is the failure this trait exists to prevent.
pub trait Journal {
    /// Take a vote lock and enqueue its frame, atomically.
    ///
    /// # Errors
    ///
    /// [`JournalError::AlreadyVoted`] if a lock is already held. The frame is
    /// then not enqueued: a refused vote must leave no trace on the air.
    fn commit_vote(
        &mut self,
        subject: &Subject,
        voter: &str,
        frame: OutgoingFrame,
    ) -> Result<(), JournalError>;

    /// Whether a lock is held for this case and voter.
    fn has_voted(&self, subject: &Subject, voter: &str) -> bool;

    /// Reserve a sequence number that will never be handed out again.
    fn reserve_sequence(&mut self) -> u64;

    /// The next sequence number that has never been used.
    fn next_sequence(&self) -> u64;

    /// Frames still awaiting acknowledgement.
    fn pending(&self) -> impl Iterator<Item = &OutgoingFrame>;

    /// Mark a frame delivered. Returns whether anything changed.
    ///
    /// Idempotent by contract: a consumer interrupted between sending and
    /// acknowledging will acknowledge again after a restart.
    fn acknowledge(&mut self, sequence: u64) -> bool;

    /// Discard frames whose validity has passed.
    ///
    /// Dropping an unsent frame never releases its vote lock. The node did
    /// decide; it merely failed to get the decision out in time, and letting it
    /// decide again would be a second vote.
    fn drop_expired(&mut self, now: Timestamp);

    /// Everything that must survive a restart.
    fn snapshot(&self) -> JournalSnapshot;
}
