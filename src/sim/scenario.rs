//! Running a fleet through a whole endorsement and reporting what happened.

use alloc::vec::Vec;

use crate::domain::contracts::Subject;
use crate::domain::quorum::{Policy, evaluate};
use crate::domain::time::Timestamp;
use crate::sim::channel::{Topology, airtime_ms};
use crate::wire::{CompactEnvelope, SigningKey, VerifyingKey, decode_compact, encode_compact};

extern crate alloc;

/// What a run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Whether both quorum thresholds were met by verified signatures.
    pub endorsed: bool,
    /// Distinct members whose binding support verified.
    pub binding_supporters: usize,
    /// Signers the count threshold required.
    pub min_signers: usize,
    /// Frames put on the air.
    pub frames_sent: usize,
    /// Frames that failed verification and were discarded.
    pub rejected_frames: usize,
    /// Total time on air, in milliseconds.
    pub airtime_ms: u64,
}

/// A fleet, a channel and a claim to agree on.
#[derive(Debug, Clone)]
pub struct Scenario {
    fleet: usize,
    loss: f64,
    seed: u64,
    topology: Topology,
    silent: usize,
    forgers: usize,
}

impl Scenario {
    /// Transmission attempts before a frame is abandoned.
    ///
    /// Bounded rather than unlimited: retrying forever on a partitioned link
    /// burns the duty cycle that the rest of the fleet needs. Five attempts
    /// survive roughly 30 % loss while costing at most five times the airtime.
    pub const MAX_RETRIES: usize = 5;

    /// A healthy fully-connected fleet with no loss.
    #[must_use]
    pub fn new(fleet: usize) -> Self {
        Self {
            fleet: fleet.max(1),
            loss: 0.0,
            seed: 1,
            topology: Topology::FullyConnected,
            silent: 0,
            forgers: 0,
        }
    }

    /// Independent per-link loss probability.
    #[must_use]
    pub fn with_loss(mut self, loss: f64) -> Self {
        self.loss = loss.clamp(0.0, 1.0);
        self
    }

    /// Seed for the deterministic generator.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// How nodes reach each other.
    #[must_use]
    pub fn with_topology(mut self, topology: Topology) -> Self {
        self.topology = topology;
        self
    }

    /// Members that never transmit.
    #[must_use]
    pub fn with_silent(mut self, silent: usize) -> Self {
        self.silent = silent;
        self
    }

    /// Members that sign as somebody else.
    #[must_use]
    pub fn with_forgers(mut self, forgers: usize) -> Self {
        self.forgers = forgers;
        self
    }

    /// Run the scenario to completion.
    ///
    /// # Panics
    ///
    /// Only on an internally inconsistent fleet, which the constructors make
    /// unreachable: weights are equal so the ratio cap holds, and every signer
    /// comes from the manifest this function built.
    #[must_use]
    #[allow(clippy::needless_range_loop)]
    pub fn run(&self) -> Report {
        // Endorsement is judged from ONE observer's seat, not from a global
        // union of everything anyone heard. A partitioned fleet would otherwise
        // look unanimous to nobody in particular, which is precisely the
        // fabricated quorum this protocol must never produce.
        const OBSERVER: usize = 0;

        let policy = Policy::new((0..self.fleet).map(|i| (member_id(i), 1)))
            .expect("equal weights are always within the cap");

        let subject = Subject::new("sim", "evt-1", 0, [0x11; 32], Timestamp::from_secs(0))
            .expect("valid subject");

        let keys: Vec<SigningKey> = (0..self.fleet)
            .map(|i| SigningKey::from_seed(seed_for(i)))
            .collect();
        let manifest: Vec<VerifyingKey> = keys.iter().map(SigningKey::verifying_key).collect();

        let mut rng = Rng::new(self.seed);
        let mut frames_sent = 0usize;
        let mut rejected = 0usize;
        let mut airtime = 0u64;
        let mut verified: Vec<alloc::string::String> = Vec::new();

        for sender in 0..self.fleet {
            if sender < self.silent {
                continue;
            }

            // A forger signs with a key that is not the one the manifest lists
            // for the index it claims. Holding the group key is not authorship.
            let signing = if sender < self.silent + self.forgers {
                &keys[(sender + 1) % self.fleet]
            } else {
                &keys[sender]
            };

            let envelope = CompactEnvelope::new(
                1,
                1,
                0,
                *subject.content_hash(),
                subject.started_at().as_secs(),
                u16::try_from(sender).unwrap_or(u16::MAX),
                3,
                1,
                sender as u64,
            );
            let bytes = encode_compact(&envelope.sign(signing)).expect("encodes");

            // Store-and-forward: an unacknowledged frame stays in the outbox
            // and is retried. This is what makes a lossy link survivable, and
            // the reason a single dropped frame is not a lost vote. It costs
            // airtime on every attempt, which is why the budget is bounded.
            let mut reached_observer = sender == OBSERVER;
            let mut attempt = 0;
            while !reached_observer && attempt < Self::MAX_RETRIES {
                attempt += 1;
                frames_sent += 1;
                airtime += airtime_ms(bytes.len());
                if self.topology.reaches(sender, OBSERVER, self.fleet)
                    && rng.next_f64() >= self.loss
                {
                    reached_observer = true;
                }
            }
            if sender == OBSERVER {
                frames_sent += 1;
                airtime += airtime_ms(bytes.len());
            }
            if !reached_observer {
                continue;
            }

            let Ok(received) = decode_compact(&bytes) else {
                rejected += 1;
                continue;
            };
            let claimed = received.envelope().author_index() as usize;
            if received.verify(&manifest[claimed]).is_err() {
                rejected += 1;
                continue;
            }
            let id = member_id(claimed);
            if !verified.contains(&id) {
                verified.push(id);
            }
        }

        let outcome = evaluate(&policy, verified.clone()).expect("members come from the manifest");

        Report {
            endorsed: outcome.approved(),
            binding_supporters: verified.len(),
            min_signers: policy.min_signers(),
            frames_sent,
            rejected_frames: rejected,
            airtime_ms: airtime,
        }
    }
}

fn member_id(index: usize) -> alloc::string::String {
    let mut id = alloc::string::String::from("n");
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut value = index;
    loop {
        at -= 1;
        digits[at] = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for digit in &digits[at..] {
        id.push(*digit as char);
    }
    id
}

fn seed_for(index: usize) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
    seed
}

/// A small deterministic generator, so a failing scenario replays exactly.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1))
    }

    fn next_f64(&mut self) -> f64 {
        // xorshift64*: adequate for scenario loss, not for anything secret.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(2_685_821_657_736_338_717);
        // Shifted to 53 bits first, which is exactly f64's mantissa, so both
        // conversions are lossless by construction rather than by luck.
        #[allow(clippy::cast_precision_loss)]
        {
            (value >> 11) as f64 / 9_007_199_254_740_992.0
        }
    }
}
