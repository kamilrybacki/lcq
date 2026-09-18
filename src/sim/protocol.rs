//! The whole protocol, over the simulated radio.
//!
//! [`super::Scenario`] models **one round of one stage**: it signs frames,
//! collides them and counts signatures. That is enough to study the channel and
//! nothing like enough to claim the protocol works, because it never touches
//! the state machine, the journal, the clock or the group seal.
//!
//! This module drives the real thing. Each node owns a [`Case`], a
//! [`MemoryJournal`] and a signing key; frames are sealed with the group key
//! before they go on the air; and the phases advance through a [`Clock`] that
//! respects [`MAX_CLOCK_SKEW_SECONDS`]. Anything the state machine refuses is
//! counted rather than quietly dropped.
//!
//! The headline consequence, and it is not a small one: a binding vote cannot
//! be admitted until consultation is closed, and consultation cannot close
//! until the clock is *certainly* past the cutoff. **No endorsement can complete
//! in less than `CONSULTATION_CUTOFF_SECONDS + MAX_CLOCK_SKEW_SECONDS`**, which
//! is 330 seconds, however fast the radio is.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::application::{Journal, OutgoingFrame};
use crate::domain::contracts::{CONSULTATION_CUTOFF_SECONDS, Opinion, Stage, Subject, Verdict};
use crate::domain::quorum::{Policy, evaluate};
use crate::domain::state::{Case, TransitionErrorCount};
use crate::domain::time::{Clock, FixedClock, MAX_CLOCK_SKEW_SECONDS, Timestamp};
use crate::infrastructure::MemoryJournal;
use crate::sim::channel::{Topology, airtime_ms};
use crate::sim::medium::{Reception, Transmission, duty_cycle_ok, receive};
use crate::sim::phy::{Link, RicianFading, TX_POWER_DBM, rssi_dbm};
use crate::sim::rng::Rng;
use crate::sim::scenario::Access;
use crate::wire::{
    CompactEnvelope, GroupKey, SigningKey, VerifyingKey, decode_compact, encode_compact,
    open_frame, seal_frame,
};

extern crate alloc;

/// Judged from one observer's seat, as everywhere else in this crate.
const OBSERVER: usize = 0;

/// Received power assumed when no geometry is configured.
const NOMINAL_RSSI_DBM: f64 = -80.0;

/// Rician K factor, in dB, for the maritime links modelled here.
const RICIAN_K_DB: f64 = 6.0;

extern crate core;

/// What one stage of the protocol cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StageReport {
    /// Frames put on the air during this stage.
    pub frames_sent: usize,
    /// Frames destroyed by an overlapping transmission.
    pub collided: usize,
    /// Frames that never reached the observer.
    pub lost: usize,
    /// Utterances the observer's state machine admitted.
    pub admitted: usize,
    /// Utterances the state machine refused, for any reason.
    pub refused: usize,
    /// Total time on air, in milliseconds.
    pub airtime_ms: u64,
    /// How long the stage occupied the channel, in milliseconds.
    pub elapsed_ms: u64,
}

/// What a whole deliberation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliberationReport {
    /// Whether the quorum approved, judged on binding support only.
    pub endorsed: bool,
    /// Distinct members whose binding vote the state machine admitted.
    pub binding_supporters: usize,
    /// Signers the count threshold required.
    pub min_signers: usize,
    /// Independent, consultation and binding stages, in order.
    pub stages: [StageReport; 3],
    /// Seconds of protocol time from the subject starting to the vote closing.
    pub elapsed_s: u64,
    /// Of that, how much was actually spent moving frames.
    pub radio_ms: u64,
    /// Total airtime across every stage.
    pub airtime_ms: u64,
    /// On-air size of one sealed frame, in bytes.
    pub frame_bytes: usize,
    /// Transitions the state machine refused, by reason.
    pub refusals: TransitionErrorCount,
}

impl DeliberationReport {
    /// What share of the elapsed time the radio was responsible for.
    #[must_use]
    pub fn radio_share(&self) -> f64 {
        if self.elapsed_s == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.radio_ms as f64 / (self.elapsed_s as f64 * 1000.0)
        }
    }
}

/// A fleet deliberating one claim, through the real protocol.
#[derive(Debug, Clone)]
pub struct Deliberation {
    fleet: usize,
    loss: f64,
    seed: u64,
    topology: Topology,
    silent: usize,
    spacing_m: Option<f64>,
    access: Access,
}

impl Deliberation {
    /// Transmission attempts before a frame is abandoned.
    pub const MAX_RETRIES: usize = 5;

    /// The contention window for the first round of a stage, in milliseconds.
    pub const CONTENTION_WINDOW_MS: u64 = 30_000;

    /// A healthy fully-connected fleet with no loss, contending at random.
    #[must_use]
    pub fn new(fleet: usize) -> Self {
        Self {
            fleet: fleet.max(1),
            loss: 0.0,
            seed: 1,
            topology: Topology::FullyConnected,
            silent: 0,
            spacing_m: None,
            access: Access::Random,
        }
    }

    /// Residual per-frame loss probability.
    #[must_use]
    pub fn with_loss(mut self, loss: f64) -> Self {
        self.loss = loss.clamp(0.0, 1.0);
        self
    }

    /// Seed for the deterministic generator.
    #[must_use]
    pub const fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// How nodes reach each other.
    #[must_use]
    pub const fn with_topology(mut self, topology: Topology) -> Self {
        self.topology = topology;
        self
    }

    /// Members that never transmit.
    #[must_use]
    pub const fn with_silent(mut self, silent: usize) -> Self {
        self.silent = silent;
        self
    }

    /// Strings the fleet out in a line, `spacing_m` apart, observer at one end.
    #[must_use]
    pub fn with_spacing_m(mut self, spacing_m: f64) -> Self {
        self.spacing_m = Some(spacing_m.max(0.0));
        self
    }

    /// Give every member the slot its manifest index earns it.
    #[must_use]
    pub const fn with_slots(mut self, guard_ms: u64) -> Self {
        self.access = Access::Slotted { guard_ms };
        self
    }

    /// Run the whole deliberation: consult, freeze, vote.
    ///
    /// # Panics
    ///
    /// Only on an internally inconsistent fleet, which the constructors make
    /// unreachable.
    #[must_use]
    pub fn run(&self) -> DeliberationReport {
        let started_at = Timestamp::from_secs(1_000_000);
        let subject =
            Subject::new("mission", "evt-1", 0, [0x22; 32], started_at).expect("valid subject");
        let policy = Policy::new((0..self.fleet).map(|i| (member_id(i), 1)))
            .expect("equal weights are always within the cap");

        let keys: Vec<SigningKey> = (0..self.fleet)
            .map(|i| SigningKey::from_seed(seed_for(i)))
            .collect();
        let manifest: Vec<VerifyingKey> = keys.iter().map(SigningKey::verifying_key).collect();
        let group = GroupKey::from_bytes([0x5a; 32]);

        let mut journals: Vec<MemoryJournal> =
            (0..self.fleet).map(|_| MemoryJournal::default()).collect();
        let mut observer = Case::open(subject.clone());
        let mut rng = Rng::new(self.seed);
        let mut fading = RicianFading::new(self.seed ^ 0x9E37_79B9_7F4A_7C15, RICIAN_K_DB);
        let mut used_airtime: Vec<u64> = alloc::vec![0; self.fleet];
        let mut refusals = TransitionErrorCount::default();

        // The consultation stages run while the cutoff is still ahead.
        let mut clock = FixedClock::new(started_at.plus_secs(1));
        let mut radio_ms = 0u64;
        let mut stages = [StageReport::default(); 3];

        for (index, stage) in [Stage::Independent, Stage::Consultation]
            .into_iter()
            .enumerate()
        {
            stages[index] = self.run_stage(
                stage,
                &subject,
                &keys,
                &manifest,
                &group,
                &mut journals,
                &mut observer,
                &clock,
                &mut rng,
                &mut fading,
                &mut used_airtime,
                &mut refusals,
            );
            radio_ms += stages[index].elapsed_ms;
        }

        // Nothing binding may be admitted until the clock is CERTAINLY past the
        // cutoff. This wait is the protocol's, not the radio's, and it dominates
        // everything the channel does.
        let close_at = CONSULTATION_CUTOFF_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1;
        clock = FixedClock::new(started_at.plus_secs(close_at));
        if let Err(error) = observer.close_consultation(&clock) {
            refusals.record(error);
        }

        stages[2] = self.run_stage(
            Stage::BindingSupport,
            &subject,
            &keys,
            &manifest,
            &group,
            &mut journals,
            &mut observer,
            &clock,
            &mut rng,
            &mut fading,
            &mut used_airtime,
            &mut refusals,
        );
        radio_ms += stages[2].elapsed_ms;

        let supporters: Vec<String> = observer
            .binding_supporters()
            .map(ToString::to_string)
            .collect();
        let outcome =
            evaluate(&policy, supporters.clone()).expect("members come from the manifest");
        let frame_bytes = Self::frame_size(&subject, &keys[0], &group);

        DeliberationReport {
            endorsed: outcome.approved(),
            binding_supporters: supporters.len(),
            min_signers: policy.min_signers(),
            stages,
            elapsed_s: close_at + stages[2].elapsed_ms / 1000,
            radio_ms,
            airtime_ms: stages.iter().map(|s| s.airtime_ms).sum(),
            frame_bytes,
            refusals,
        }
    }

    /// How long one round of a stage lasts.
    fn window_for(
        &self,
        round: usize,
        subject: &Subject,
        key: &SigningKey,
        group: &GroupKey,
    ) -> u64 {
        match self.access {
            Access::Random => {
                let shift: u32 = match round {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    _ => 3,
                };
                Self::CONTENTION_WINDOW_MS << shift
            }
            Access::Slotted { guard_ms } => {
                let air = airtime_ms(Self::frame_size(subject, key, group));
                (air + guard_ms) * self.fleet as u64
            }
        }
    }

    /// One stage: everyone speaks once, with retries, and the observer listens.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn run_stage(
        &self,
        stage: Stage,
        subject: &Subject,
        keys: &[SigningKey],
        manifest: &[VerifyingKey],
        group: &GroupKey,
        journals: &mut [MemoryJournal],
        observer: &mut Case,
        clock: &impl Clock,
        rng: &mut Rng,
        fading: &mut RicianFading,
        used_airtime: &mut [u64],
        refusals: &mut TransitionErrorCount,
    ) -> StageReport {
        let mut report = StageReport::default();
        let mut pending: Vec<usize> = (self.silent..self.fleet).collect();
        let mut elapsed = 0u64;
        // One frame per member per stage, kept for retransmission.
        let mut outbox: Vec<Option<Vec<u8>>> = alloc::vec![None; self.fleet];

        for round in 0..Self::MAX_RETRIES {
            if pending.is_empty() {
                break;
            }
            let mut on_air: Vec<(usize, Transmission, Vec<u8>)> = Vec::new();
            let mut retry: Vec<usize> = Vec::new();

            let window = self.window_for(round, subject, &keys[0], group);

            for sender in core::mem::take(&mut pending) {
                // Store and forward: the frame is built and journalled once,
                // then the SAME bytes go out on every attempt. Rebuilding it
                // would ask the journal for a second vote lock on the same
                // case, which it would rightly refuse -- and the member would
                // then never retransmit at all, silently losing its vote to the
                // first collision.
                let bytes = if let Some(bytes) = outbox[sender].clone() {
                    bytes
                } else {
                    {
                        let Some(built) =
                            Self::frame_for(sender, stage, subject, keys, group, journals)
                        else {
                            // The journal refused: the lock is held from an
                            // earlier case, or nothing could be made durable.
                            continue;
                        };
                        outbox[sender] = Some(built.clone());
                        built
                    }
                };
                let air = airtime_ms(bytes.len());
                if !duty_cycle_ok(used_airtime[sender], air) {
                    continue;
                }
                used_airtime[sender] += air;
                report.frames_sent += 1;
                report.airtime_ms += air;

                let start = match self.access {
                    Access::Random => elapsed + rng.below(window),
                    Access::Slotted { guard_ms } => elapsed + sender as u64 * (air + guard_ms),
                };
                if sender == OBSERVER {
                    // Its own utterance needs no radio, but the transmission
                    // still deafens it for the duration.
                    on_air.push((sender, Transmission::own_transmission(start, air), bytes));
                } else if self.topology.reaches(sender, OBSERVER, self.fleet) {
                    let rssi = self.rssi_at_observer(sender, fading);
                    on_air.push((sender, Transmission::new(start, air, rssi), bytes));
                } else {
                    report.lost += 1;
                    retry.push(sender);
                }
            }

            let channel: Vec<Transmission> = on_air.iter().map(|(_, t, _)| *t).collect();
            for (index, (sender, frame, bytes)) in on_air.iter().enumerate() {
                let others: Vec<Transmission> = channel
                    .iter()
                    .enumerate()
                    .filter_map(|(other, t)| (other != index).then_some(*t))
                    .collect();
                let heard = if *sender == OBSERVER {
                    // A node always knows its own utterance.
                    Reception::Decoded
                } else {
                    receive(frame, &others)
                };
                match heard {
                    Reception::Collided => {
                        report.collided += 1;
                        retry.push(*sender);
                    }
                    Reception::TooWeak => {
                        report.lost += 1;
                        retry.push(*sender);
                    }
                    Reception::Decoded if *sender != OBSERVER && rng.next_f64() < self.loss => {
                        report.lost += 1;
                        retry.push(*sender);
                    }
                    Reception::Decoded => {
                        match Self::admit(bytes, stage, subject, manifest, group, observer, clock) {
                            Ok(()) => report.admitted += 1,
                            Err(error) => {
                                report.refused += 1;
                                refusals.record(error);
                            }
                        }
                    }
                }
            }

            let tail = on_air
                .iter()
                .map(|(_, t, _)| t.end_ms())
                .max()
                .unwrap_or(elapsed);
            elapsed = tail.max(elapsed + window);
            pending = retry;
        }

        report.elapsed_ms = elapsed;
        report
    }

    /// Build, sign, seal and journal one member's utterance.
    ///
    /// Returns `None` when the journal refuses, which for a binding vote is the
    /// lock doing its job.
    fn frame_for(
        sender: usize,
        stage: Stage,
        subject: &Subject,
        keys: &[SigningKey],
        group: &GroupKey,
        journals: &mut [MemoryJournal],
    ) -> Option<Vec<u8>> {
        let sequence = journals[sender].reserve_sequence().ok()?;
        let author = u16::try_from(sender).unwrap_or(u16::MAX);
        let envelope = CompactEnvelope::new(
            1,
            1,
            0,
            *subject.content_hash(),
            subject.started_at().as_secs(),
            author,
            stage_code(stage),
            verdict_code(Verdict::Support),
            sequence,
        );
        let signed = encode_compact(&envelope.sign(&keys[sender])).ok()?;
        // What actually goes on the air is sealed under the group key. Modelling
        // the unsealed frame understates every airtime figure by its overhead.
        let sealed = seal_frame(group, author, sequence, &signed).ok()?;

        if stage == Stage::BindingSupport {
            // The lock is taken BEFORE the frame is exposed. A journal that
            // refuses here is a member that already voted.
            journals[sender]
                .commit_vote(
                    subject,
                    &member_id(sender),
                    OutgoingFrame::new(sealed.clone(), sequence),
                )
                .ok()?;
        }
        Some(sealed)
    }

    /// Open, verify and offer one received frame to the observer's state machine.
    fn admit(
        sealed: &[u8],
        stage: Stage,
        subject: &Subject,
        manifest: &[VerifyingKey],
        group: &GroupKey,
        observer: &mut Case,
        clock: &impl Clock,
    ) -> Result<(), crate::domain::state::TransitionError> {
        use crate::domain::state::TransitionError;

        // Exactly what a real receiver does: the nonce travels in the
        // authenticated cleartext header, so nothing has to be guessed.
        let Ok((_claimed_in_header, _sequence, plain)) = open_frame(group, sealed) else {
            return Err(TransitionError::DifferentSubject);
        };
        let Ok(received) = decode_compact(&plain) else {
            return Err(TransitionError::DifferentSubject);
        };
        let claimed = received.envelope().author_index() as usize;
        if claimed >= manifest.len() || received.verify(&manifest[claimed]).is_err() {
            return Err(TransitionError::DifferentSubject);
        }
        let opinion = Opinion::new(
            &member_id(claimed),
            subject.clone(),
            stage,
            Verdict::Support,
        );
        observer.accept(opinion, clock)
    }

    /// On-air size of one sealed frame.
    fn frame_size(subject: &Subject, key: &SigningKey, group: &GroupKey) -> usize {
        let envelope = CompactEnvelope::new(
            1,
            1,
            0,
            *subject.content_hash(),
            subject.started_at().as_secs(),
            0,
            stage_code(Stage::BindingSupport),
            verdict_code(Verdict::Support),
            u64::from(u32::MAX),
        );
        let signed = encode_compact(&envelope.sign(key)).expect("encodes");
        seal_frame(group, u16::MAX, u64::from(u32::MAX), &signed)
            .expect("seals")
            .len()
    }

    /// Received power at the observer, in dBm.
    fn rssi_at_observer(&self, sender: usize, fading: &mut RicianFading) -> f64 {
        match self.spacing_m {
            None => NOMINAL_RSSI_DBM,
            Some(spacing) => {
                #[allow(clippy::cast_precision_loss)]
                let distance = spacing * sender as f64;
                rssi_dbm(&Link::new(distance), TX_POWER_DBM) + fading.sample_db()
            }
        }
    }
}

const fn stage_code(stage: Stage) -> u8 {
    match stage {
        Stage::Independent => 1,
        Stage::Consultation => 2,
        Stage::BindingSupport => 3,
    }
}

const fn verdict_code(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Support => 1,
        Verdict::Dispute => 2,
        Verdict::InsufficientData => 3,
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
