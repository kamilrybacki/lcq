//! What is waiting to go out, and in what order.
//!
//! Three decisions, all of them deliberate.
//!
//! **Bounded.** Memory on a node is finite and `LoRa` is slow enough that a burst
//! outlives it. An unbounded queue converts a flood into an out-of-memory death
//! instead of into dropped traffic, which is strictly worse: a node that dies
//! stops relaying for everyone.
//!
//! **Priority, but not strict priority.** Distress goes first, yet routine
//! traffic is guaranteed a share, because a node whose routine traffic never
//! leaves has silently stopped participating while still looking healthy.
//!
//! **Priority is assigned locally.** A neighbour's claim that its frame is
//! urgent is an input, never an instruction. Otherwise one hostile peer marks
//! everything distress and owns the whole queue.

use alloc::collections::{BTreeSet, VecDeque};
use core::fmt;

use crate::application::journal::OutgoingFrame;
use crate::domain::time::Timestamp;

extern crate alloc;

/// Why a frame was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    /// The queue is at its item or byte cap.
    Full,
    /// This frame is already queued or was recently sent.
    Duplicate,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Full => "queue is at capacity",
            Self::Duplicate => "frame already seen",
        };
        f.write_str(message)
    }
}

impl core::error::Error for QueueError {}

/// How urgently a frame should leave, as decided by *this* node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Ordinary protocol traffic.
    Routine,
    /// Safety traffic that should pre-empt routine work.
    Distress,
}

/// A bounded outgoing queue with weighted fairness.
#[derive(Debug, Clone)]
pub struct RadioQueue {
    distress: VecDeque<OutgoingFrame>,
    routine: VecDeque<OutgoingFrame>,
    seen: BTreeSet<u64>,
    seen_order: VecDeque<u64>,
    max_items: usize,
    max_bytes: usize,
    queued_bytes: usize,
    distress_streak: u8,
}

impl RadioQueue {
    /// Distress frames sent back-to-back before routine traffic gets a turn.
    ///
    /// Chosen rather than derived: at 8:1 a sustained distress burst still lets
    /// routine traffic through roughly every ninth slot, which is enough to keep
    /// a node visible without meaningfully delaying safety traffic.
    pub const DISTRESS_BURST: u8 = 8;

    /// How many recently-seen frame identities are remembered for de-duplication.
    ///
    /// Bounded on purpose: an unbounded dedup set is a slower memory leak. The
    /// cost of forgetting is one redundant relay, which the next hop drops.
    pub const MAX_DEDUP_ENTRIES: usize = 1_024;

    /// A queue with explicit item and byte caps.
    #[must_use]
    pub fn with_capacity(max_items: usize, max_bytes: usize) -> Self {
        Self {
            distress: VecDeque::new(),
            routine: VecDeque::new(),
            seen: BTreeSet::new(),
            seen_order: VecDeque::new(),
            max_items,
            max_bytes,
            queued_bytes: 0,
            distress_streak: 0,
        }
    }

    /// Frames currently queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.distress.len() + self.routine.len()
    }

    /// Whether anything is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many frame identities are remembered.
    #[must_use]
    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }

    /// Offer a frame for transmission.
    ///
    /// # Errors
    ///
    /// [`QueueError::Full`] at either cap, and [`QueueError::Duplicate`] if this
    /// frame was already queued or recently sent — store-and-forward means the
    /// same frame arrives from several neighbours.
    pub fn offer(&mut self, frame: OutgoingFrame, priority: Priority) -> Result<(), QueueError> {
        if self.seen.contains(&frame.sequence()) {
            return Err(QueueError::Duplicate);
        }
        if self.len() >= self.max_items
            || self.queued_bytes.saturating_add(frame.bytes().len()) > self.max_bytes
        {
            return Err(QueueError::Full);
        }

        self.remember(frame.sequence());
        self.queued_bytes += frame.bytes().len();
        match priority {
            Priority::Distress => self.distress.push_back(frame),
            Priority::Routine => self.routine.push_back(frame),
        }
        Ok(())
    }

    /// Take the next frame to send.
    ///
    /// Named `take_next` rather than `next`: this is not an iterator, and a
    /// queue that silently satisfied `Iterator` would invite `for` loops that
    /// drain the radio backlog in one pass.
    #[must_use]
    pub fn take_next(&mut self) -> Option<OutgoingFrame> {
        // Let routine traffic through once the distress burst is spent, so the
        // low class cannot be starved indefinitely.
        let take_routine = self.distress.is_empty() || self.distress_streak >= Self::DISTRESS_BURST;

        let frame = if take_routine && !self.routine.is_empty() {
            self.distress_streak = 0;
            self.routine.pop_front()
        } else if let Some(frame) = self.distress.pop_front() {
            self.distress_streak = self.distress_streak.saturating_add(1);
            Some(frame)
        } else {
            self.routine.pop_front()
        }?;

        self.queued_bytes = self.queued_bytes.saturating_sub(frame.bytes().len());
        Some(frame)
    }

    /// Discard frames whose validity has passed.
    ///
    /// Forwarding never extends a signed validity, so a frame that outlived its
    /// subject is not worth the airtime.
    pub fn drop_expired(&mut self, now: Timestamp) {
        let expired = |frame: &OutgoingFrame| frame.expires_at().is_some_and(|at| at < now);
        for queue in [&mut self.distress, &mut self.routine] {
            queue.retain(|frame| !expired(frame));
        }
        self.queued_bytes = self
            .distress
            .iter()
            .chain(self.routine.iter())
            .map(|f| f.bytes().len())
            .sum();
    }

    fn remember(&mut self, sequence: u64) {
        self.seen.insert(sequence);
        self.seen_order.push_back(sequence);
        while self.seen_order.len() > Self::MAX_DEDUP_ENTRIES {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
    }
}
