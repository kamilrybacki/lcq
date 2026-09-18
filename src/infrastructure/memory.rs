//! An in-memory safety journal.
//!
//! Enough for the simulation stage, where a "restart" is
//! [`MemoryJournal::restored`] from a snapshot rather than a process dying. The
//! ordering guarantees are real; the durability is not, and a database-backed
//! adapter has to supply that separately.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;

use crate::application::{Journal, JournalError, JournalSnapshot, OutgoingFrame};
use crate::domain::contracts::Subject;
use crate::domain::time::Timestamp;

extern crate alloc;

/// Identity of one vote lock: the exact case, plus who voted on it.
///
/// Built from the full subject rather than the event ID alone, so a revision or
/// a change of content is a different lock — as it must be, since those are
/// different claims.
fn lock_key(subject: &Subject, voter: &str) -> String {
    // Hex by hand rather than `format!`: this runs on every lookup, and a
    // formatter allocation per byte is not worth paying on a constrained node.
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut key = String::new();
    key.push_str(subject.mission());
    key.push('\u{1f}');
    key.push_str(subject.event());
    key.push('\u{1f}');
    for byte in subject.content_hash() {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 0x0f) as usize] as char);
    }
    key.push('\u{1f}');
    let mut revision = subject.revision();
    let mut digits = [0u8; 10];
    let mut index = digits.len();
    loop {
        index -= 1;
        digits[index] = b'0' + (revision % 10) as u8;
        revision /= 10;
        if revision == 0 {
            break;
        }
    }
    for digit in &digits[index..] {
        key.push(*digit as char);
    }
    key.push('\u{1f}');
    key.push_str(voter);
    key
}

/// A journal held in memory.
#[derive(Debug, Clone, Default)]
pub struct MemoryJournal {
    locks: BTreeSet<String>,
    pending: BTreeMap<u64, OutgoingFrame>,
    next_sequence: u64,
}

impl MemoryJournal {
    /// Rebuild from a snapshot, as a node would after a restart.
    #[must_use]
    pub fn restored(snapshot: JournalSnapshot) -> Self {
        Self {
            locks: snapshot.vote_locks.into_iter().map(|(k, _)| k).collect(),
            pending: snapshot
                .pending
                .into_iter()
                .map(|frame| (frame.sequence(), frame))
                .collect(),
            next_sequence: snapshot.next_sequence,
        }
    }
}

impl Journal for MemoryJournal {
    fn commit_vote(
        &mut self,
        subject: &Subject,
        voter: &str,
        frame: OutgoingFrame,
    ) -> Result<(), JournalError> {
        let key = lock_key(subject, voter);
        // Lock first. If this returns false the frame is dropped on the floor,
        // which is the point: a refused vote must never reach the air.
        if !self.locks.insert(key) {
            return Err(JournalError::AlreadyVoted);
        }
        self.next_sequence = self.next_sequence.max(frame.sequence().saturating_add(1));
        self.pending.insert(frame.sequence(), frame);
        Ok(())
    }

    fn has_voted(&self, subject: &Subject, voter: &str) -> bool {
        self.locks.contains(&lock_key(subject, voter))
    }

    fn reserve_sequence(&mut self) -> u64 {
        let reserved = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        reserved
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn pending(&self) -> impl Iterator<Item = &OutgoingFrame> {
        self.pending.values()
    }

    fn acknowledge(&mut self, sequence: u64) -> bool {
        self.pending.remove(&sequence).is_some()
    }

    fn drop_expired(&mut self, now: Timestamp) {
        self.pending
            .retain(|_, frame| frame.expires_at().is_none_or(|at| at >= now));
    }

    fn snapshot(&self) -> JournalSnapshot {
        JournalSnapshot {
            vote_locks: self.locks.iter().map(|k| (k.clone(), 0)).collect(),
            pending: self.pending.values().cloned().collect(),
            next_sequence: self.next_sequence,
        }
    }
}
