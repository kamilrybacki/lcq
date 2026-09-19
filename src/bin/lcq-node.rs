//! One node of the fleet, as a real process.
//!
//! Its own journal on disk, its own clock with its own error, its own state
//! machine, talking to the others only through the channel emulator. Nothing
//! here is shared with another node except the group key and the manifest,
//! which is the point: every earlier measurement in this crate came from one
//! process where sharing was accidental and invisible.
//!
//! It is written to be killed. Everything that must survive is in the journal
//! before it is exposed, so a node restarted mid-vote recovers its lock, does
//! **not** vote again, and resends the frame it already committed.
//!
//! Usage: `lcq-node --index 0 --fleet 10 --hub 127.0.0.1:PORT --journal PATH
//! [--scale 100] [--offset 0] [--epoch 1000000] [--slots] [--guard-ms 200]`

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use lcq::application::{Journal, OutgoingFrame};
use lcq::domain::contracts::{CONSULTATION_CUTOFF_SECONDS, Opinion, Stage, Subject, Verdict};
use lcq::domain::quorum::{Policy, evaluate};
use lcq::domain::state::Case;
use lcq::domain::time::{Clock, MAX_CLOCK_SKEW_SECONDS, Timestamp};
use lcq::infrastructure::{LogJournal, ScaledClock};
use lcq::sim::airtime_ms;
use lcq::wire::{
    CompactEnvelope, GroupKey, SigningKey, VerifyingKey, decode_compact, encode_compact,
    open_frame, seal_frame,
};

/// Every node in a run derives the same group key from this.
const GROUP_KEY: [u8; 32] = [0x5a; 32];

/// The claim under deliberation. Fixed, because the radio is what is under test.
const CONTENT_HASH: [u8; 32] = [0x22; 32];

/// Stage code reserved for the frame that opens a round.
///
/// It carries no opinion and is never admitted to a case. Its only job is to
/// give every member the same instant to count slots from, which is the trigger
/// anchor D6 forced: a member's slot must not move because its clock is wrong.
/// Anchoring instead on each process's own start makes the schedule inherit
/// process spawn jitter, and under a scaled clock that jitter is multiplied by
/// the scale.
const TRIGGER_STAGE: u8 = 0;

// A binary's entry point, and a linear one: open the journal, join the
// channel, run the round, report. Splitting it further would move steps behind
// names without making the order any easier to follow, and the order is the
// thing a reader needs.
#[allow(clippy::too_many_lines)]
fn main() {
    let options = Options::from_args();
    let epoch = Timestamp::from_secs(options.epoch);
    // Provisional, for the log lines before a round exists. The real one starts
    // when the trigger does: a case begins when the frame that opens it goes on
    // the air. Starting this clock at process start instead meant a vessel that
    // waited for the fleet to assemble burned that wait as protocol time, and
    // finished the whole deliberation before anybody had spoken.
    let mut clock = ScaledClock::new(epoch, options.scale, options.offset);

    let subject = Subject::new("mission", "evt-1", 0, CONTENT_HASH, epoch).expect("valid subject");
    let keys: Vec<SigningKey> = (0..options.fleet)
        .map(|i| SigningKey::from_seed(seed_for(i)))
        .collect();
    let manifest: Vec<VerifyingKey> = keys.iter().map(SigningKey::verifying_key).collect();
    let group = GroupKey::from_bytes(GROUP_KEY);
    let policy = Policy::new((0..options.fleet).map(|i| (member_id(i), 1))).expect("policy");

    // Recovering the journal IS the restart path. Anything already committed is
    // read back here, including a vote lock that must not be taken twice.
    let mut journal = LogJournal::open(&options.journal).expect("journal opens");
    let recovered_vote = journal.has_voted(&subject, &member_id(options.index));
    let mut case = Case::open(subject.clone());

    report(
        &options,
        &clock,
        &format!(
            "{{\"event\":\"start\",\"index\":{},\"recovered_vote\":{recovered_vote},\"pending\":{}}}",
            options.index,
            journal.pending().count()
        ),
    );

    let started = Instant::now();
    let mut link = Link::connect(&options.hub, options.index);
    let mut sent = [false; 3];
    let mut closed = false;
    let mut began: Option<Instant> = None;

    let stage_window = options.stage_window_s();
    // The first stage cannot open the instant the trigger ends: leave more
    // than one frame's airtime of clearance, or the opener collides with the
    // first slot it just scheduled.
    let lead_in = 3;
    let starts = [
        lead_in,
        lead_in + stage_window,
        CONSULTATION_CUTOFF_SECONDS + MAX_CLOCK_SKEW_SECONDS + 1,
    ];
    let finish = starts[2] + stage_window + 5;

    // Transmission times are wall-clock instants, not whole protocol seconds.
    // A frame is 1.3 protocol seconds long, so scheduling to the second puts
    // neighbouring slots on top of each other -- the schedule has to be at
    // least as fine as the thing it is scheduling.
    //
    // They are counted from this process's own start, not from its clock, which
    // is the trigger anchor of D6: a member's slot does not move because its
    // clock is wrong. Process start jitter stands in for the spread in when
    // members decide the opening frame ended.
    let offsets: Vec<u64> = starts
        .iter()
        .map(|start| clock.wall_ms(start * 1_000 + options.slot_offset_ms()))
        .collect();

    loop {
        let now = clock.now().as_secs().saturating_sub(options.epoch);

        // The originator opens the round, then anchors on its own frame. It
        // waits first: a member that opens a round before the fleet is even
        // listening has opened it for nobody.
        if began.is_none()
            && options.trigger
            && started.elapsed() >= Duration::from_millis(options.trigger_delay_ms)
        {
            let bytes = build_trigger(
                &subject,
                &keys[options.index],
                &group,
                &mut journal,
                &options,
            );
            if let Ok(bytes) = bytes {
                // The anchor is when the trigger ENDS, not when it starts.
                // Receivers learn of it only once the whole frame has arrived,
                // so an originator that counted from its own first symbol would
                // run a whole airtime ahead of everybody else -- which lands its
                // slot k on top of their slot k-1.
                let air = Duration::from_millis(clock.wall_ms(airtime_ms(bytes.len())));
                link.send(&bytes);
                began = Some(Instant::now() + air);
                clock = ScaledClock::new(epoch, options.scale, options.offset);
                report(
                    &options,
                    &clock,
                    "{\"event\":\"triggered\",\"by\":\"self\"}",
                );
            }
        }
        if began.is_none() {
            // Nothing to schedule against yet: the only thing worth doing is
            // listening for the frame that opens the round.
            if let Some(frame) = link.poll()
                && is_trigger(&frame, &group)
            {
                began = Some(Instant::now());
                clock = ScaledClock::new(epoch, options.scale, options.offset);
                report(
                    &options,
                    &clock,
                    "{\"event\":\"triggered\",\"by\":\"heard\"}",
                );
            }
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let anchor = began.expect("checked just above");

        if !closed && clock.certainly_after(subject.consultation_cutoff()) {
            match case.close_consultation(&clock) {
                Ok(()) => {
                    closed = true;
                    report(&options, &clock, "{\"event\":\"consultation_closed\"}");
                }
                Err(error) => report(
                    &options,
                    &clock,
                    &format!("{{\"event\":\"close_refused\",\"why\":\"{error}\"}}"),
                ),
            }
        }

        for (index, stage) in [
            Stage::Independent,
            Stage::Consultation,
            Stage::BindingSupport,
        ]
        .into_iter()
        .enumerate()
        {
            if sent[index] || Instant::now() < anchor + Duration::from_millis(offsets[index]) {
                continue;
            }
            if stage == Stage::BindingSupport && !closed {
                continue;
            }
            match build(stage, &subject, &keys, &group, &mut journal, &options) {
                Ok(bytes) => {
                    link.send(&bytes);
                    // A node needs no radio to know its own utterance, and the
                    // emulator does not echo. Without this its own binding vote
                    // is missing from its own tally -- which happened to clear
                    // the threshold for a fleet of five and would not have for
                    // any other size.
                    admit(
                        &bytes, &subject, &manifest, &group, &mut case, &clock, &options,
                    );
                    sent[index] = true;
                    report(
                        &options,
                        &clock,
                        &format!(
                            "{{\"event\":\"sent\",\"stage\":\"{}\",\"bytes\":{}}}",
                            stage_name(stage),
                            bytes.len()
                        ),
                    );
                }
                Err(why) => {
                    sent[index] = true;
                    report(
                        &options,
                        &clock,
                        &format!(
                            "{{\"event\":\"not_sent\",\"stage\":\"{}\",\"why\":\"{why}\"}}",
                            stage_name(stage)
                        ),
                    );
                }
            }
        }

        // At most one frame per turn, and only after the send checks above have
        // had theirs. Verifying a signature is not free, and a node that misses
        // its own slot because it was busy reading is a node that collides with
        // whoever comes next.
        if let Some(frame) = link.poll()
            && !is_trigger(&frame, &group)
        {
            admit(
                &frame, &subject, &manifest, &group, &mut case, &clock, &options,
            );
        }

        if now >= finish {
            break;
        }
        // Finer than the shortest thing being timed: a frame at scale 100 is
        // 13 ms of wall time, so a 5 ms poll would miss slots.
        thread::sleep(Duration::from_millis(1));
    }

    let supporters: Vec<String> = case.binding_supporters().map(ToString::to_string).collect();
    let approved = evaluate(&policy, supporters.clone()).is_ok_and(|o| o.approved());
    report(
        &options,
        &clock,
        &format!(
            "{{\"event\":\"final\",\"index\":{},\"supporters\":{},\"threshold\":{},\"endorsed\":{},\"recovered_vote\":{recovered_vote}}}",
            options.index,
            supporters.len(),
            policy.min_signers(),
            approved
        ),
    );
}

/// Build this node's frame for a stage, journalling a binding vote first.
///
/// On a restart the lock is already held, so the committed frame is resent from
/// the outbox rather than rebuilt: the lock stops a second *decision*, never a
/// second *transmission*.
fn build(
    stage: Stage,
    subject: &Subject,
    keys: &[SigningKey],
    group: &GroupKey,
    journal: &mut LogJournal,
    options: &Options,
) -> Result<Vec<u8>, String> {
    let voter = member_id(options.index);
    if stage == Stage::BindingSupport && journal.has_voted(subject, &voter) {
        return journal
            .pending()
            .next()
            .map(|frame| frame.bytes().to_vec())
            .ok_or_else(|| "already voted, nothing left in the outbox".to_string());
    }

    let sequence = journal
        .reserve_sequence()
        .map_err(|error| error.to_string())?;
    let author = u16::try_from(options.index).unwrap_or(u16::MAX);
    let envelope = CompactEnvelope::new(
        1,
        1,
        0,
        *subject.content_hash(),
        subject.started_at().as_secs(),
        author,
        stage_code(stage),
        1,
        sequence,
    );
    let signed = encode_compact(&envelope.sign(&keys[options.index])).map_err(|e| e.to_string())?;
    let sealed = seal_frame(group, author, sequence, &signed).map_err(|e| e.to_string())?;

    if stage == Stage::BindingSupport {
        // Lock and outbox entry commit together, before the bytes leave here.
        journal
            .commit_vote(
                subject,
                &voter,
                OutgoingFrame::new(sealed.clone(), sequence),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(sealed)
}

/// The frame that opens a round: authenticated, carrying no opinion.
fn build_trigger(
    subject: &Subject,
    key: &SigningKey,
    group: &GroupKey,
    journal: &mut LogJournal,
    options: &Options,
) -> Result<Vec<u8>, String> {
    let sequence = journal
        .reserve_sequence()
        .map_err(|error| error.to_string())?;
    let author = u16::try_from(options.index).unwrap_or(u16::MAX);
    let envelope = CompactEnvelope::new(
        1,
        1,
        0,
        *subject.content_hash(),
        subject.started_at().as_secs(),
        author,
        TRIGGER_STAGE,
        0,
        sequence,
    );
    let signed = encode_compact(&envelope.sign(key)).map_err(|e| e.to_string())?;
    seal_frame(group, author, sequence, &signed).map_err(|e| e.to_string())
}

/// Whether a frame is a round opener rather than an utterance.
fn is_trigger(sealed: &[u8], group: &GroupKey) -> bool {
    let Ok((_, _, plain)) = open_frame(group, sealed) else {
        return false;
    };
    decode_compact(&plain).is_ok_and(|f| f.envelope().stage() == TRIGGER_STAGE)
}

/// Open, verify and offer a received frame to the state machine.
fn admit(
    sealed: &[u8],
    subject: &Subject,
    manifest: &[VerifyingKey],
    group: &GroupKey,
    case: &mut Case,
    clock: &impl Clock,
    options: &Options,
) {
    let Ok((_, _, plain)) = open_frame(group, sealed) else {
        return;
    };
    let Ok(received) = decode_compact(&plain) else {
        return;
    };
    let claimed = received.envelope().author_index() as usize;
    if claimed >= manifest.len() || received.verify(&manifest[claimed]).is_err() {
        report(
            options,
            clock,
            "{\"event\":\"refused\",\"why\":\"signature\"}",
        );
        return;
    }
    let (Some(stage), Some(verdict)) = (
        stage_of(received.envelope().stage()),
        verdict_of(received.envelope().verdict()),
    ) else {
        return;
    };
    let opinion = Opinion::new(&member_id(claimed), subject.clone(), stage, verdict);
    match case.accept(opinion, clock) {
        Ok(()) => report(
            options,
            clock,
            &format!(
                "{{\"event\":\"admitted\",\"from\":{claimed},\"stage\":\"{}\"}}",
                stage_name(stage)
            ),
        ),
        Err(error) => report(
            options,
            clock,
            &format!("{{\"event\":\"refused\",\"from\":{claimed},\"why\":\"{error}\"}}"),
        ),
    }
}

/// The socket to the channel emulator, with its reader on its own thread.
struct Link {
    stream: TcpStream,
    inbox: Receiver<Vec<u8>>,
}

impl Link {
    fn connect(address: &str, index: usize) -> Self {
        // A node may well be powered up before whatever carries its traffic is
        // reachable, so connecting is retried rather than fatal.
        let mut stream = None;
        for _ in 0..300 {
            if let Ok(socket) = TcpStream::connect(address) {
                stream = Some(socket);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let mut stream = stream.expect("hub never became reachable");
        stream
            .write_all(&u16::try_from(index).unwrap_or(0).to_le_bytes())
            .expect("handshake");
        let _ = stream.flush();

        let mut reader = stream.try_clone().expect("clone");
        let (sender, inbox) = channel();
        thread::spawn(move || {
            let mut length = [0u8; 4];
            while reader.read_exact(&mut length).is_ok() {
                let size = u32::from_le_bytes(length) as usize;
                if size == 0 || size > 4096 {
                    return;
                }
                let mut bytes = vec![0u8; size];
                if reader.read_exact(&mut bytes).is_err() || sender.send(bytes).is_err() {
                    return;
                }
            }
        });
        Self { stream, inbox }
    }

    fn send(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes();
        let _ = self.stream.write_all(&length);
        let _ = self.stream.write_all(bytes);
        let _ = self.stream.flush();
    }

    fn poll(&mut self) -> Option<Vec<u8>> {
        self.inbox.try_recv().ok()
    }
}

/// How this node was configured.
struct Options {
    index: usize,
    fleet: usize,
    hub: String,
    journal: PathBuf,
    scale: u32,
    offset: i64,
    epoch: u64,
    slots: bool,
    guard_ms: u64,
    trigger: bool,
    trigger_delay_ms: u64,
}

impl Options {
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let value = |name: &str| -> Option<String> {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        Self {
            index: value("--index").and_then(|v| v.parse().ok()).unwrap_or(0),
            fleet: value("--fleet").and_then(|v| v.parse().ok()).unwrap_or(5),
            hub: value("--hub").unwrap_or_else(|| "127.0.0.1:9000".to_string()),
            journal: value("--journal").map_or_else(|| PathBuf::from("node.log"), PathBuf::from),
            scale: value("--scale").and_then(|v| v.parse().ok()).unwrap_or(100),
            offset: value("--offset").and_then(|v| v.parse().ok()).unwrap_or(0),
            epoch: value("--epoch")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1_000_000),
            slots: args.iter().any(|a| a == "--slots"),
            guard_ms: value("--guard-ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(200),
            trigger: args.iter().any(|a| a == "--trigger"),
            trigger_delay_ms: value("--trigger-delay-ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }

    /// Protocol seconds a stage is given.
    fn stage_window_s(&self) -> u64 {
        if self.slots {
            (self.slot_ms() * self.fleet as u64).div_ceil(1_000) + 2
        } else {
            30
        }
    }

    fn slot_ms(&self) -> u64 {
        airtime_ms(137) + self.guard_ms
    }

    /// Protocol milliseconds into a stage before this node transmits.
    fn slot_offset_ms(&self) -> u64 {
        if self.slots {
            self.slot_ms() * self.index as u64
        } else {
            // Spread deterministically but unevenly, standing in for a draw.
            ((self.index as u64 * 7_919) % 25_000) + 500
        }
    }
}

fn report(options: &Options, clock: &impl Clock, line: &str) {
    let at = clock.now().as_secs().saturating_sub(options.epoch);
    println!("{{\"t\":{at},\"node\":{},\"line\":{line}}}", options.index);
    let _ = std::io::stdout().flush();
}

const fn stage_code(stage: Stage) -> u8 {
    match stage {
        Stage::Independent => 1,
        Stage::Consultation => 2,
        Stage::BindingSupport => 3,
    }
}

const fn stage_of(code: u8) -> Option<Stage> {
    match code {
        1 => Some(Stage::Independent),
        2 => Some(Stage::Consultation),
        3 => Some(Stage::BindingSupport),
        _ => None,
    }
}

const fn verdict_of(code: u8) -> Option<Verdict> {
    match code {
        1 => Some(Verdict::Support),
        2 => Some(Verdict::Dispute),
        3 => Some(Verdict::InsufficientData),
        _ => None,
    }
}

const fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Independent => "independent",
        Stage::Consultation => "consultation",
        Stage::BindingSupport => "binding",
    }
}

fn member_id(index: usize) -> String {
    format!("n{index}")
}

fn seed_for(index: usize) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
    seed
}
