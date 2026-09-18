//! Running a fleet through a whole endorsement and reporting what happened.

use alloc::string::String;
use alloc::vec::Vec;

use crate::domain::contracts::Subject;
use crate::domain::quorum::{Policy, evaluate};
use crate::domain::time::Timestamp;
use crate::sim::channel::{Topology, airtime_ms};
use crate::sim::medium::{Reception, Transmission, duty_cycle_ok, receive};
use crate::sim::phy::{Link, RicianFading, TX_POWER_DBM, rssi_dbm};
use crate::sim::rng::Rng;
use crate::wire::{CompactEnvelope, SigningKey, VerifyingKey, decode_compact, encode_compact};

extern crate alloc;

/// Endorsement is judged from ONE observer's seat, not from a global union of
/// everything anyone heard. A partitioned fleet would otherwise look unanimous
/// to nobody in particular, which is precisely the fabricated quorum this
/// protocol must never produce.
const OBSERVER: usize = 0;

/// Separates the fading stream from the loss stream, so that turning geometry
/// on does not shift which frames the residual-loss draw discards.
const FADING_STREAM: u64 = 0x9E37_79B9_7F4A_7C15;

/// What became of one transmission, from the observer's seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Received and its signature verified.
    Decoded,
    /// The observer's own frame: transmitted, never received.
    OwnTransmission,
    /// Received, but the signature did not match the claimed author.
    Rejected,
    /// Overlapped by a frame it could not capture over.
    Collided,
    /// Arrived below the demodulator's sensitivity.
    TooWeak,
    /// Out of reach on this topology; it never arrived at all.
    Unreachable,
    /// Reached the observer but was corrupted by something unmodelled.
    LostToResidualNoise,
    /// Never keyed up: the sender had spent its hourly airtime.
    DutyCycleBlocked,
}

impl Outcome {
    /// Whether the frame actually occupied the channel.
    #[must_use]
    pub const fn went_on_air(self) -> bool {
        !matches!(self, Self::DutyCycleBlocked)
    }
}

/// One transmission, recorded so a run can be replayed and inspected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceEntry {
    /// Which retry round it belongs to, counting from zero.
    pub round: usize,
    /// The member that transmitted.
    pub sender: usize,
    /// When it started, in milliseconds after the trigger, on one clock that
    /// runs across the whole run rather than restarting each round.
    pub start_ms: u64,
    /// How long it held the channel.
    pub airtime_ms: u64,
    /// Received power at the observer, in dBm. Infinite for its own frame.
    pub rssi_dbm: f64,
    /// What became of it.
    pub outcome: Outcome,
}

/// What a run produced.
#[derive(Debug, Clone, PartialEq)]
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
    /// Frames lost to an overlapping transmission the receiver could not
    /// capture over, including those masked by its own half-duplex radio.
    pub collided_frames: usize,
    /// Frames that arrived below the demodulator's sensitivity.
    pub too_weak_frames: usize,
    /// Transmissions refused because the sender had spent its hourly airtime.
    pub duty_cycle_blocked: usize,
    /// Total time on air, in milliseconds.
    pub airtime_ms: u64,
    /// Every transmission in order, for inspection and visualisation.
    pub timeline: Vec<TraceEntry>,
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
    spacing_m: Option<f64>,
    prior_airtime_ms: u64,
}

impl Scenario {
    /// Transmission attempts before a frame is abandoned.
    ///
    /// Bounded rather than unlimited: retrying forever on a partitioned link
    /// burns the duty cycle that the rest of the fleet needs. Five attempts
    /// survive roughly 30 % loss while costing at most five times the airtime.
    pub const MAX_RETRIES: usize = 5;

    /// The spread of start times for the first response round, in milliseconds.
    ///
    /// Nodes answer a trigger by picking a uniform offset inside this window.
    /// It has to be wide relative to a frame — over a second at SF10 — or every
    /// member answers on top of every other and the channel destroys the whole
    /// round. Each retry doubles it, which is the backoff that makes a
    /// contended round converge instead of repeating its own pile-up.
    pub const CONTENTION_WINDOW_MS: u64 = 30_000;

    /// Rician K factor, in dB, for the maritime links modelled here.
    const RICIAN_K_DB: f64 = 6.0;

    /// Received power assumed when no geometry is configured.
    ///
    /// Comfortably above sensitivity and **identical for every node**, which is
    /// the honest consequence of declining to model distance: with no spread in
    /// signal strength nothing can ever capture, so every overlap destroys both
    /// frames. Configure [`Self::with_spacing_m`] to get capture back.
    const NOMINAL_RSSI_DBM: f64 = -80.0;

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
            spacing_m: None,
            prior_airtime_ms: 0,
        }
    }

    /// Residual per-frame loss probability.
    ///
    /// Everything the physical model does not name: interference from outside
    /// the fleet, a receiver that was busy, a corrupted header. Applied on top
    /// of path loss, fading and collisions rather than instead of them.
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

    /// Strings the fleet out in a line, `spacing_m` apart, observer at one end.
    ///
    /// Turns on path loss and fading: the far end of a long line falls below
    /// sensitivity and stops contributing, and the spread in signal strength
    /// lets a near node capture over a distant one. A convoy, roughly.
    #[must_use]
    pub fn with_spacing_m(mut self, spacing_m: f64) -> Self {
        self.spacing_m = Some(spacing_m.max(0.0));
        self
    }

    /// Airtime every node has already spent this hour before the endorsement.
    ///
    /// Endorsement is not the only traffic a node carries: position reports and
    /// application messages come out of the same 1 % budget. Setting this shows
    /// what happens when the regulation, rather than the channel, is what stops
    /// a member from answering.
    #[must_use]
    pub fn with_prior_airtime_ms(mut self, prior_airtime_ms: u64) -> Self {
        self.prior_airtime_ms = prior_airtime_ms;
        self
    }

    /// How many members there are.
    #[must_use]
    pub const fn fleet(&self) -> usize {
        self.fleet
    }

    /// Spacing between members along the line, in metres, when geometry is on.
    ///
    /// `None` means distance is not modelled: every node is assumed equally
    /// audible, so there are no positions to speak of and nothing may be drawn
    /// as if there were.
    #[must_use]
    pub const fn spacing_m(&self) -> Option<f64> {
        self.spacing_m
    }

    /// The contention window for a retry round, in milliseconds.
    #[must_use]
    pub const fn window_ms(round: usize) -> u64 {
        let shift: u32 = match round {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 3,
        };
        Self::CONTENTION_WINDOW_MS << shift
    }

    /// Run the scenario to completion.
    ///
    /// # Panics
    ///
    /// Only on an internally inconsistent fleet, which the constructors make
    /// unreachable: weights are equal so the ratio cap holds, and every signer
    /// comes from the manifest this function built.
    #[must_use]
    pub fn run(&self) -> Report {
        let policy = Policy::new((0..self.fleet).map(|i| (member_id(i), 1)))
            .expect("equal weights are always within the cap");
        let subject = Subject::new("sim", "evt-1", 0, [0x11; 32], Timestamp::from_secs(0))
            .expect("valid subject");

        let keys: Vec<SigningKey> = (0..self.fleet)
            .map(|i| SigningKey::from_seed(seed_for(i)))
            .collect();
        let manifest: Vec<VerifyingKey> = keys.iter().map(SigningKey::verifying_key).collect();
        let frames: Vec<Vec<u8>> = (0..self.fleet)
            .map(|sender| self.frame_for(sender, &subject, &keys))
            .collect();

        let mut run = Run::new(self, &frames, &manifest);
        let mut pending: Vec<usize> = (self.silent..self.fleet)
            .filter(|i| *i != OBSERVER)
            .collect();
        for round in 0..Self::MAX_RETRIES {
            if pending.is_empty() {
                break;
            }
            pending = run.round(round, pending);
        }

        let outcome =
            evaluate(&policy, run.verified.clone()).expect("members come from the manifest");
        Report {
            endorsed: outcome.approved(),
            binding_supporters: run.verified.len(),
            min_signers: policy.min_signers(),
            frames_sent: run.tally.frames_sent,
            rejected_frames: run.tally.rejected,
            collided_frames: run.tally.collided,
            too_weak_frames: run.tally.too_weak,
            duty_cycle_blocked: run.tally.duty_cycle_blocked,
            airtime_ms: run.tally.airtime_ms,
            timeline: run.tally.timeline,
        }
    }

    /// The signed frame a member puts on the air.
    fn frame_for(&self, sender: usize, subject: &Subject, keys: &[SigningKey]) -> Vec<u8> {
        // A forger signs with a key that is not the one the manifest lists for
        // the index it claims. Holding the group key is not authorship.
        let signing = if (self.silent..self.silent + self.forgers).contains(&sender) {
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
        encode_compact(&envelope.sign(signing)).expect("encodes")
    }
}

/// Mutable state for one execution of a [`Scenario`].
struct Run<'a> {
    scenario: &'a Scenario,
    frames: &'a [Vec<u8>],
    manifest: &'a [VerifyingKey],
    rng: Rng,
    fading: RicianFading,
    tally: Tally,
    used_airtime: Vec<u64>,
    verified: Vec<String>,
    /// Milliseconds since the trigger. Rounds are laid end to end: a round runs
    /// for its contention window plus the longest frame started inside it, so
    /// the last transmission finishes before the next round opens. That is what
    /// the collision model already assumes by evaluating one round at a time.
    elapsed_ms: u64,
}

impl<'a> Run<'a> {
    fn new(scenario: &'a Scenario, frames: &'a [Vec<u8>], manifest: &'a [VerifyingKey]) -> Self {
        Self {
            scenario,
            frames,
            manifest,
            rng: Rng::new(scenario.seed),
            fading: RicianFading::new(scenario.seed ^ FADING_STREAM, Scenario::RICIAN_K_DB),
            tally: Tally::default(),
            used_airtime: alloc::vec![scenario.prior_airtime_ms; scenario.fleet],
            verified: Vec::new(),
            elapsed_ms: 0,
        }
    }

    /// One contention round. Returns the members that must try again.
    fn round(&mut self, round: usize, pending: Vec<usize>) -> Vec<usize> {
        // Backoff: each retry spreads the survivors over twice the window,
        // capped so a long tail does not push responses past any sane deadline.
        // Without it a collided round simply collides again.
        let window = Scenario::CONTENTION_WINDOW_MS << u32::try_from(round.min(3)).unwrap_or(3);
        let mut retry = Vec::new();

        if round == 0 {
            self.observer_answers(window);
        }
        let on_air = self.schedule(round, window, pending, &mut retry);
        self.deliver(round, &on_air, &mut retry);

        let tail = on_air
            .iter()
            .map(|(_, frame)| frame.end_ms())
            .max()
            .unwrap_or(self.elapsed_ms);
        self.elapsed_ms = tail.max(self.elapsed_ms + window);
        retry
    }

    /// The observer answers once, in the first round.
    ///
    /// It needs no radio to know its own vote, but its transmission still
    /// occupies the channel and deafens it to everyone else for the duration.
    fn observer_answers(&mut self, window: u64) {
        if self.scenario.silent != 0 {
            return;
        }
        let air = airtime_ms(self.frames[OBSERVER].len());
        let charged = self.tally.charge(&mut self.used_airtime, OBSERVER, air, 0);
        let start = if charged {
            self.elapsed_ms + self.rng.below(window)
        } else {
            0
        };
        if charged {
            self.tally
                .on_air
                .push((OBSERVER, Transmission::own_transmission(start, air)));
        }
        // Its own vote is still checked against the manifest. A member that
        // signs as somebody else does not get counted for being local, so the
        // trace has to show the rejection rather than a plain transmission.
        let verdict = self.accept(OBSERVER);
        if charged {
            let outcome = if verdict == Outcome::Rejected {
                Outcome::Rejected
            } else {
                Outcome::OwnTransmission
            };
            self.tally
                .record(0, OBSERVER, start, air, f64::INFINITY, outcome);
        }
    }

    /// Key up every pending member and place its frame on the channel.
    fn schedule(
        &mut self,
        round: usize,
        window: u64,
        pending: Vec<usize>,
        retry: &mut Vec<usize>,
    ) -> Vec<(usize, Transmission)> {
        let mut on_air = core::mem::take(&mut self.tally.on_air);
        for sender in pending {
            let air = airtime_ms(self.frames[sender].len());
            if !self
                .tally
                .charge(&mut self.used_airtime, sender, air, round)
            {
                // Out of legal airtime. Not retried: the budget does not come
                // back inside the scenario's horizon.
                continue;
            }
            let start = self.elapsed_ms + self.rng.below(window);
            if self
                .scenario
                .topology
                .reaches(sender, OBSERVER, self.scenario.fleet)
            {
                let rssi = self.rssi_at_observer(sender);
                on_air.push((sender, Transmission::new(start, air, rssi)));
            } else {
                // Transmitted, and paid for in airtime, but never arrives.
                self.tally.record(
                    round,
                    sender,
                    start,
                    air,
                    f64::NEG_INFINITY,
                    Outcome::Unreachable,
                );
                retry.push(sender);
            }
        }
        on_air
    }

    /// Decide what the observer actually heard.
    fn deliver(&mut self, round: usize, on_air: &[(usize, Transmission)], retry: &mut Vec<usize>) {
        let channel: Vec<Transmission> = on_air.iter().map(|(_, frame)| *frame).collect();
        for (index, (sender, frame)) in on_air.iter().enumerate() {
            if *sender == OBSERVER {
                continue;
            }
            let others: Vec<Transmission> = channel
                .iter()
                .enumerate()
                .filter_map(|(other, frame)| (other != index).then_some(*frame))
                .collect();
            let outcome = match receive(frame, &others) {
                Reception::Collided => {
                    self.tally.collided += 1;
                    retry.push(*sender);
                    Outcome::Collided
                }
                Reception::TooWeak => {
                    self.tally.too_weak += 1;
                    retry.push(*sender);
                    Outcome::TooWeak
                }
                Reception::Decoded if self.rng.next_f64() < self.scenario.loss => {
                    retry.push(*sender);
                    Outcome::LostToResidualNoise
                }
                Reception::Decoded => self.accept(*sender),
            };
            self.tally.record(
                round,
                *sender,
                frame.start_ms(),
                frame.airtime_ms(),
                frame.rssi_dbm(),
                outcome,
            );
        }
    }

    /// Verify a received frame and record its author's support.
    fn accept(&mut self, sender: usize) -> Outcome {
        let Ok(received) = decode_compact(&self.frames[sender]) else {
            self.tally.rejected += 1;
            return Outcome::Rejected;
        };
        let claimed = received.envelope().author_index() as usize;
        if claimed >= self.manifest.len() || received.verify(&self.manifest[claimed]).is_err() {
            self.tally.rejected += 1;
            return Outcome::Rejected;
        }
        let id = member_id(claimed);
        if !self.verified.contains(&id) {
            self.verified.push(id);
        }
        Outcome::Decoded
    }

    /// Received power at the observer, in dBm.
    fn rssi_at_observer(&mut self, sender: usize) -> f64 {
        match self.scenario.spacing_m {
            None => Scenario::NOMINAL_RSSI_DBM,
            Some(spacing) => {
                // Observer at one end of the line, so index and distance agree.
                #[allow(clippy::cast_precision_loss)]
                let distance = spacing * sender as f64;
                rssi_dbm(&Link::new(distance), TX_POWER_DBM) + self.fading.sample_db()
            }
        }
    }
}

/// Running counts for one run.
#[derive(Debug, Default)]
struct Tally {
    /// Frames the observer itself put on the channel this round, carried into
    /// [`Run::schedule`] so its own transmission can deafen it.
    on_air: Vec<(usize, Transmission)>,
    timeline: Vec<TraceEntry>,
    frames_sent: usize,
    rejected: usize,
    collided: usize,
    too_weak: usize,
    duty_cycle_blocked: usize,
    airtime_ms: u64,
}

impl Tally {
    /// Charge a transmission against a node's hourly budget.
    ///
    /// Returns whether the frame may legally go out. A refusal is counted, not
    /// billed: the radio never keys up, so it costs no airtime.
    fn charge(&mut self, used: &mut [u64], node: usize, air: u64, round: usize) -> bool {
        if !duty_cycle_ok(used[node], air) {
            self.duty_cycle_blocked += 1;
            self.record(
                round,
                node,
                0,
                0,
                f64::NEG_INFINITY,
                Outcome::DutyCycleBlocked,
            );
            return false;
        }
        used[node] += air;
        self.frames_sent += 1;
        self.airtime_ms += air;
        true
    }

    fn record(
        &mut self,
        round: usize,
        sender: usize,
        start_ms: u64,
        airtime_ms: u64,
        rssi_dbm: f64,
        outcome: Outcome,
    ) {
        self.timeline.push(TraceEntry {
            round,
            sender,
            start_ms,
            airtime_ms,
            rssi_dbm,
            outcome,
        });
    }
}

fn member_id(index: usize) -> String {
    let mut id = String::from("n");
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
