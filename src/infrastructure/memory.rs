//! An in-memory safety journal.
//!
//! Enough for the simulation stage, where a "restart" is
//! [`MemoryJournal::restored`] from a snapshot rather than a process dying. The
//! ordering guarantees are real; the durability is not. A node that must
//! survive its own process dying wants [`super::LogJournal`].

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;

use crate::infrastructure::lock_key::lock_key;

use crate::application::{Journal, JournalError, JournalSnapshot, OutgoingFrame};
use crate::domain::contracts::Subject;
use crate::domain::time::Timestamp;

extern crate alloc;

/// A journal held in memory.
#[derive(Debug, Clone, Default)]
pub struct MemoryJournal {
    epoch: Option<u16>,
    seen: BTreeMap<u16, u64>,
    locks: BTreeSet<String>,
    pending: BTreeMap<u64, OutgoingFrame>,
    next_sequence: u64,
    witnessed: BTreeMap<String, alloc::vec::Vec<u8>>,
}

impl MemoryJournal {
    /// Rebuild from a snapshot, as a node would after a restart.
    #[must_use]
    pub fn restored(snapshot: JournalSnapshot) -> Self {
        Self {
            epoch: snapshot.epoch,
            seen: snapshot.seen.into_iter().collect(),
            locks: snapshot.vote_locks.into_iter().map(|(k, _)| k).collect(),
            pending: snapshot
                .pending
                .into_iter()
                .map(|frame| (frame.sequence(), frame))
                .collect(),
            next_sequence: snapshot.next_sequence,
            witnessed: snapshot.witnessed.into_iter().collect(),
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

    fn reserve_sequence(&mut self) -> Result<u64, JournalError> {
        let reserved = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        // Memory cannot fail to persist because it never persists at all.
        Ok(reserved)
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

    fn witness(&mut self, author: &str, frame: &[u8]) -> Result<(), JournalError> {
        self.witnessed
            .entry(String::from(author))
            .or_insert_with(|| frame.to_vec());
        Ok(())
    }

    fn witnessed(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.witnessed
            .iter()
            .map(|(author, frame)| (author.as_str(), frame.as_slice()))
    }

    fn epoch(&self) -> Option<u16> {
        self.epoch
    }

    fn enter_epoch(&mut self, epoch: u16) -> Result<(), JournalError> {
        if self.epoch.is_some_and(|held| held >= epoch) {
            return Ok(());
        }
        self.seen.clear();
        self.epoch = Some(epoch);
        Ok(())
    }

    fn highest_seen(&self, sender: u16) -> Option<u64> {
        self.seen.get(&sender).copied()
    }

    fn mark_seen(&mut self, sender: u16, sequence: u64) -> Result<(), JournalError> {
        let held = self.seen.entry(sender).or_default();
        *held = (*held).max(sequence);
        Ok(())
    }

    fn snapshot(&self) -> JournalSnapshot {
        JournalSnapshot {
            epoch: self.epoch,
            seen: self
                .seen
                .iter()
                .map(|(sender, sequence)| (*sender, *sequence))
                .collect(),
            vote_locks: self.locks.iter().map(|k| (k.clone(), 0)).collect(),
            pending: self.pending.values().cloned().collect(),
            next_sequence: self.next_sequence,
            witnessed: self
                .witnessed
                .iter()
                .map(|(a, f)| (a.clone(), f.clone()))
                .collect(),
        }
    }
}
