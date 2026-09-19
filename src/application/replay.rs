//! What a receiver remembers of one sender's sequence numbers.
//!
//! A frame is a replay if this sender's sequence has been seen, or is so far
//! behind the newest one that nothing that old can be told apart any more.
//! "Newest wins" alone -- everything at or below the highest sequence is a
//! replay -- was the first rule, and it drops a genuine older frame carried
//! late: a vote relayed by a neighbour after its author's later frame has
//! already been heard. The window keeps one bit per recent sequence instead,
//! so an unseen frame inside it is admitted once and a seen one never, and the
//! memory per sender is fixed at [`REPLAY_WINDOW`] bits whatever the traffic.

/// How many sequences behind the newest a sender's frame may still be admitted.
pub const REPLAY_WINDOW: u32 = 64;

/// One sender's recent sequences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayWindow {
    highest: Option<u64>,
    /// Bit `i` set: the sequence `highest - i` has been seen.
    seen: u64,
}

impl ReplayWindow {
    /// Nothing seen yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            highest: None,
            seen: 0,
        }
    }

    /// The newest sequence seen from this sender.
    #[must_use]
    pub const fn highest(&self) -> Option<u64> {
        self.highest
    }

    /// Whether a frame with this sequence is a replay: already seen, or older
    /// than the window remembers.
    #[must_use]
    pub fn seen(&self, sequence: u64) -> bool {
        let Some(highest) = self.highest else {
            return false;
        };
        if sequence > highest {
            return false;
        }
        let back = highest - sequence;
        if back >= u64::from(REPLAY_WINDOW) {
            return true;
        }
        self.seen & (1u64 << back) != 0
    }

    /// Remember a sequence that was admitted.
    pub fn mark(&mut self, sequence: u64) {
        match self.highest {
            None => {
                self.highest = Some(sequence);
                self.seen = 1;
            }
            Some(highest) if sequence > highest => {
                let shift = sequence - highest;
                self.seen = if shift >= u64::from(REPLAY_WINDOW) {
                    0
                } else {
                    self.seen << shift
                };
                self.seen |= 1;
                self.highest = Some(sequence);
            }
            Some(highest) => {
                let back = highest - sequence;
                if back < u64::from(REPLAY_WINDOW) {
                    self.seen |= 1u64 << back;
                }
            }
        }
    }
}
