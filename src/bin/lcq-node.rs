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
use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasher, Hasher};

use lcq::domain::contracts::{
    BINDING_STAGE_OPENS_SECONDS, ENDORSEMENT_TARGET_SECONDS, Opinion, Stage, Subject, Verdict,
};
use lcq::domain::quorum::{Policy, evaluate};
use lcq::domain::state::{Case, TransitionError};
use lcq::domain::time::{Clock, Timestamp};
use lcq::infrastructure::{LogJournal, ScaledClock};
use lcq::sim::airtime_ms;
use lcq::wire::{
    CompactEnvelope, GroupKey, Heard, MAX_FRAME_BYTES, RoundId, SigningKey, VerifyingKey,
    decode_compact, encode_compact, open_frame, peek_frame_header, seal_frame,
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

/// Stage code for a repair request: "these are the binding votes I hold".
///
/// Carries no opinion and is never admitted to a case. An acknowledgement bit
/// says *somebody* heard a member; under loss that is per link, and a sender
/// that stops once anyone acknowledged it leaves whoever missed it without
/// the vote for good. So after the scheduled window a member still short of
/// votes says what it holds, and any member absent from that list resends its
/// own vote once. Store-and-forward, driven by the receiver that has the gap.
const NACK_STAGE: u8 = 4;

/// Repair rounds after the scheduled binding attempts, two windows each: one
/// for requests, one for the resends they ask for.
///
/// One round is not enough on a lossy channel: the request and the resend are
/// each a frame, and each is lost as readily as the vote was. Measured at 30 %
/// loss, a single round left one member in five short about one run in five.
/// Three rounds put that below three per cent, for at most six more windows.
const REPAIR_ROUNDS: u64 = 3;

/// The subject's manifest identity, as every frame carries it.
const MISSION_EPOCH: u16 = 1;
const EVENT: u32 = 1;
const REVISION: u16 = 0;

/// Whether a frame is about the subject this node is deliberating.
///
/// Without this, an utterance about some other claim -- another event, another
/// revision, different content -- would be admitted as if it were about ours,
/// because the receiver builds the opinion from its own subject. The state
/// machine's own subject check can only catch what the receiver hands it.
fn same_subject(envelope: &CompactEnvelope, subject: &Subject) -> bool {
    envelope.mission_epoch() == MISSION_EPOCH
        && envelope.event() == EVENT
        && envelope.revision() == REVISION
        && envelope.content_hash() == subject.content_hash()
        && envelope.started_at() == subject.started_at().as_secs()
}

/// What a compromised member does. One member per run, for the tests that
/// check safety holds and measure what liveness costs.
///
/// None of these modes can be reached by accident: they are how the test
/// harness puts a faulty member on the air, so the honest members' defences
/// are exercised by a real adversary rather than described in a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adversary {
    /// Honest.
    None,
    /// Signs every frame with another member's key while claiming its own index.
    Forge,
    /// Ignores its own journal and casts a second, contradicting binding vote.
    DoubleVote,
    /// Reports having heard everyone on every frame, whether it did or not.
    LieAcks,
    /// Transmits junk in the next member's slot every window, and votes for nothing.
    Jam,
    /// Records the first frame it hears and puts it back on the air during the vote.
    Replay,
    /// Opens the round, then opens it again a second later under a new label.
    Equivocate,
}

impl Adversary {
    fn parse(name: Option<&str>) -> Self {
        match name {
            Some("forge") => Self::Forge,
            Some("double-vote") => Self::DoubleVote,
            Some("lie-acks") => Self::LieAcks,
            Some("jam") => Self::Jam,
            Some("replay") => Self::Replay,
            Some("equivocate") => Self::Equivocate,
            _ => Self::None,
        }
    }
}

/// A random offset from the operating system's entropy, in `[0, bound)`.
///
/// For retransmissions once a split is known. Not a seeded generator: an
/// adversary who can predict when a member repeats itself can be waiting there,
/// and the whole point of leaving the slot schedule is to take that away.
fn entropy_below(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    RandomState::new().build_hasher().finish() % bound
}

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
    // Who this node has heard cast a binding vote, and whether anybody has
    // reported hearing this node. Both ride on frames already being sent, so
    // acknowledgement costs eight bytes rather than a frame of its own.
    let mut heard = Heard::none();
    let mut acknowledged = false;
    // Whether this node has counted its own utterance for each stage. Keyed on
    // the fact rather than on the attempt number: a member that missed its
    // slot has already spent an attempt before it first sends, and a member
    // that restarts resends from the outbox on what is, for it, attempt one.
    let mut own_admitted = [false; 3];
    // Repair rounds this node has already asked in, the resend it owes, and
    // the round it last resent in -- one request and one resend per round.
    let mut nacked_rounds: u64 = 0;
    let mut repair_due: Option<Instant> = None;
    let mut repaired_round: Option<u64> = None;
    // The round this node joined, and whether anybody turned out to be in a
    // different one. Signatures do not stop a member saying two things, so a
    // compromised opener can leave halves of a fleet counting slots from
    // different instants. It cannot fabricate a quorum, but the halves collide
    // with each other until somebody notices.
    let mut round = RoundId::none();
    let mut split = false;
    // Members whose verified frames disagreed with our round, by label or by
    // timing. One such member proves nothing: a vote's round label is written
    // by the voter, so a single compromised member could stamp a foreign label
    // on its own frames and push the whole fleet off its slots. Two independent
    // members are the threshold, which a lone liar cannot reach.
    let mut foreign: BTreeSet<usize> = BTreeSet::new();
    // The conflicting openings themselves, kept whole: a round label says two
    // things were said, the signed frames say what.
    let mut evidence: Vec<Vec<u8>> = Vec::new();
    // When each stage's next attempt is due. Fixed once per attempt, so a
    // randomised retry does not move under the loop's feet.
    let mut next_due: [Option<Instant>; 3] = [None; 3];
    // Binding votes that arrived before this node had closed consultation.
    // Under a split -- or for a member well behind on its clock -- another
    // member's binding slot can come round while we are still consulting, and
    // the state machine rightly refuses a binding vote in that phase. Refusing
    // is not the same as forgetting: the vote is held, one per member, and
    // offered again the moment consultation closes. Nothing about safety
    // changes, since the state machine checks it then exactly as it would have.
    let mut early: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    // Votes this node admitted in an earlier life. They go through the same
    // gate as votes that arrive too early: held until consultation closes,
    // then offered to the state machine, which verifies each again.
    let mut witnessed_restored = 0usize;
    for (author, bytes) in journal.witnessed() {
        if let Some(index) = member_index(author) {
            early.insert(index, bytes.to_vec());
            witnessed_restored += 1;
        }
    }
    // Our own recovered vote counts too. The send path admits a vote when it
    // first goes out, but a member back from a restart may never send it again
    // -- the fleet already acknowledged it -- and would then be the one member
    // missing from its own tally. It goes through the same gate as the rest.
    if recovered_vote && let Some(frame) = journal.pending().next() {
        early.insert(options.index, frame.bytes().to_vec());
        own_admitted[2] = true;
    }
    if witnessed_restored > 0 {
        report(
            &options,
            &clock,
            &format!("{{\"event\":\"witnessed_restored\",\"votes\":{witnessed_restored}}}"),
        );
    }
    // Adversary bookkeeping. Unused by an honest member.
    let mut equivocate_at: Option<Instant> = None;
    let mut captured: Option<Vec<u8>> = None;
    let mut jam_next: Option<Instant> = None;
    let mut replay_next: Option<Instant> = None;
    // Highest sequence verified from each member. A frame at or below it is a
    // retransmission or a replay, and either way already known, so it is
    // dropped before paying for decryption and a signature check. Advanced only
    // after the signature verifies: the cleartext header is a claim, and anyone
    // holding the group key could otherwise poison the window with forgeries.
    let mut last_seen: Vec<Option<u64>> = vec![None; options.fleet];
    let mut attempts = [0usize; 3];

    let stage_window = options.stage_window_s();
    // The first stage cannot open the instant the trigger ends: leave more
    // than one frame's airtime of clearance, or the opener collides with the
    // first slot it just scheduled.
    let lead_in = 3;
    let starts = [
        lead_in,
        lead_in + stage_window,
        // Not `cutoff + skew`: that is when the member furthest AHEAD has
        // closed. The one furthest behind closes a whole skew budget later,
        // and until then it would skip its slot and fire into somebody else's.
        BINDING_STAGE_OPENS_SECONDS + 1,
    ];
    // The binding stage has to be long enough for every attempt it allows,
    // or the run ends before the retries it was configured for and the
    // measurement is of the window rather than of anything else.
    // Two windows past the last binding attempt: one for repair requests, one
    // for the resends they ask for.
    let finish =
        starts[2] + stage_window * (options.attempts.max(1) as u64 + 2 * REPAIR_ROUNDS) + 5;
    // The whole schedule -- every stage, every allowed attempt -- has to fit
    // before endorsement is expected. A binding stage that may legally begin
    // but cannot finish is not a schedule, and finding that out mid-round is
    // too late to do anything about it.
    if finish > ENDORSEMENT_TARGET_SECONDS {
        report(
            &options,
            &clock,
            &format!(
                "{{\"event\":\"schedule_unfit\",\"finish_s\":{finish},\"target_s\":{ENDORSEMENT_TARGET_SECONDS}}}"
            ),
        );
        std::process::exit(2);
    }

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
        // The originator opens the round, then anchors on its own frame. It
        // waits first: a member that opens a round before the fleet is even
        // listening has opened it for nobody.
        if began.is_none() && started.elapsed() >= options.open_deadline(&clock) {
            // The round is named by this very frame: who sends it, under which
            // sequence. Read before the reservation so the two agree, and
            // checked by every receiver against the frame's own header.
            round = RoundId::new(
                u16::try_from(options.index).unwrap_or(u16::MAX),
                u32::try_from(journal.next_sequence()).unwrap_or(u32::MAX),
            );
            let bytes = build_trigger(
                &subject,
                &keys[options.index],
                &group,
                &mut journal,
                &options,
                round,
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
                if options.adversary == Adversary::Equivocate {
                    // Two seconds of wall time: after the consultation stages
                    // and long before the vote, when the channel is idle. A
                    // second opening that collides with somebody's opinion
                    // tests the emulator, not the fleet.
                    equivocate_at = Some(Instant::now() + Duration::from_secs(2));
                }
            }
        }
        if began.is_none() {
            // Nothing to schedule against yet: the only thing worth doing is
            // listening for the frame that opens the round.
            if let Some(frame) = link.poll()
                && !is_replay(&frame, &last_seen)
            {
                if let Some((opened, opener, sequence)) =
                    trigger_round(&frame, &group, &manifest, &subject)
                {
                    last_seen[opener] = Some(sequence);
                    round = opened;
                    began = Some(Instant::now());
                    clock = ScaledClock::new(epoch, options.scale, options.offset);
                    report(
                        &options,
                        &clock,
                        "{\"event\":\"triggered\",\"by\":\"heard\"}",
                    );
                } else if let Some(late) = late_anchor(
                    &frame, &group, &manifest, &subject, &starts, &options, &clock,
                ) {
                    // The opening was missed -- lost on the air, or this node
                    // was not yet running -- but a vote was heard, and the
                    // schedule is deterministic: the vote's sender and stage
                    // say exactly when the round began. Waiting for a trigger
                    // that has already gone would mean never joining.
                    last_seen[late.author] = Some(late.sequence);
                    round = late.round;
                    began = Some(late.origin);
                    clock =
                        ScaledClock::anchored_at(late.origin, epoch, options.scale, options.offset);
                    report(
                        &options,
                        &clock,
                        &format!(
                            "{{\"event\":\"triggered\",\"by\":\"derived\",\"from\":{}}}",
                            late.author
                        ),
                    );
                    // The frame that anchored us is offered to the state
                    // machine like any other. Consultation is NOT closed here:
                    // the main loop closes it on its next turn and drains
                    // everything being held -- votes restored from the journal,
                    // votes that arrived too early, and this one if it was a
                    // binding vote refused for arriving before the close. The
                    // first version closed here directly and skipped that drain,
                    // so a restarted member never counted what it had restored.
                    let learned = admit(
                        &frame, &subject, &manifest, &group, &mut case, &clock, &options,
                    );
                    if let Some((from, _)) = learned.verified
                        && learned.refused == Some(TransitionError::WrongStageForPhase)
                        && learned.stage_index == Some(2)
                    {
                        early.insert(from, frame.clone());
                    }
                    if let Some(from) = learned.binding_from {
                        heard.heard_from(from);
                        let _ = journal.witness(&member_id(from), &frame);
                    }
                    if learned.acknowledges_us && !options.ignore_acks {
                        acknowledged = true;
                    }
                }
            }
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let anchor = began.expect("checked just above");
        // The repair timeline, on the anchor's axis like everything else.
        let window_wall = Duration::from_millis(clock.wall_ms(stage_window * 1_000));
        let repair_start = anchor
            + Duration::from_millis(
                clock.wall_ms((starts[2] + stage_window * options.attempts.max(1) as u64) * 1_000),
            );
        let own_slot =
            Duration::from_millis(clock.wall_ms(options.slot_ms() * options.index as u64));
        // Which repair round the clock says we are in, if any.
        let repair_round = || -> Option<u64> {
            let since = Instant::now().checked_duration_since(repair_start)?;
            let pair = window_wall.as_millis().saturating_mul(2).max(1);
            let index = u64::try_from(since.as_millis() / pair).unwrap_or(u64::MAX);
            (index < REPAIR_ROUNDS).then_some(index)
        };

        // A second opening of the same subject, validly signed, under a fresh
        // label. Nobody who heard the first re-anchors; what it produces is
        // evidence, and a single disagreeing member is below the threshold
        // for declaring a split -- which is the point being tested.
        if let Some(at) = equivocate_at
            && Instant::now() >= at
        {
            equivocate_at = None;
            let second = RoundId::new(
                u16::try_from(options.index).unwrap_or(u16::MAX),
                u32::try_from(journal.next_sequence()).unwrap_or(u32::MAX),
            );
            if let Ok(bytes) = build_trigger(
                &subject,
                &keys[options.index],
                &group,
                &mut journal,
                &options,
                second,
            ) {
                link.send(&bytes);
                report(&options, &clock, "{\"event\":\"equivocated\"}");
            }
        }
        // Junk in the next member's binding slot, every window. The junk opens
        // under nobody's key, so it is refused everywhere; what it does is
        // occupy the air where one member needs it.
        if options.adversary == Adversary::Jam {
            let target = (options.index + 1) % options.fleet;
            let first = anchor
                + Duration::from_millis(
                    clock.wall_ms(starts[2] * 1_000 + options.slot_ms() * target as u64),
                );
            let next = jam_next.get_or_insert(first);
            if Instant::now() >= *next {
                let junk: Vec<u8> = (0..MAX_FRAME_BYTES - 8)
                    .map(|i| {
                        u8::try_from(entropy_below(256)).unwrap_or(0)
                            ^ u8::try_from(i % 256).unwrap_or(0)
                    })
                    .collect();
                link.send(&junk);
                *next += Duration::from_millis(options.retry_gap_ms(&clock));
                report(
                    &options,
                    &clock,
                    &format!("{{\"event\":\"jammed\",\"slot_of\":{target}}}"),
                );
            }
        }
        // A frame heard earlier, put back on the air during the vote. Every
        // receiver has already advanced past its sequence and drops it before
        // paying for a signature check.
        if options.adversary == Adversary::Replay
            && let Some(old) = captured.as_ref()
        {
            let first = anchor + Duration::from_millis(clock.wall_ms(starts[2] * 1_000));
            let next = replay_next.get_or_insert(first);
            if Instant::now() >= *next {
                link.send(old);
                // Not half a window: that divides the schedule and lands on the
                // same two slots every window, which is jamming, and the jam
                // test covers jamming. This period drifts across the slots.
                *next += Duration::from_millis(clock.wall_ms(options.stage_window_s() * 370));
                report(&options, &clock, "{\"event\":\"replayed\"}");
            }
        }

        if !closed && clock.certainly_after(subject.consultation_cutoff()) {
            match case.close_consultation(&clock) {
                Ok(()) => {
                    closed = true;
                    report(&options, &clock, "{\"event\":\"consultation_closed\"}");
                    for (_, held) in std::mem::take(&mut early) {
                        let learned = admit(
                            &held, &subject, &manifest, &group, &mut case, &clock, &options,
                        );
                        if let Some(from) = learned.binding_from {
                            heard.heard_from(from);
                            let _ = journal.witness(&member_id(from), &held);
                        }
                        if learned.acknowledges_us && !options.ignore_acks {
                            acknowledged = true;
                        }
                    }
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
            if options.adversary == Adversary::Jam {
                break;
            }
            // Only the binding vote is worth repeating, and only until somebody
            // reports having heard it. Repeating blind costs about four and a
            // half times the airtime for nothing (`DECISIONS.md` D4).
            let allowed = if stage == Stage::BindingSupport {
                options.attempts
            } else {
                1
            };
            if attempts[index] >= allowed || sent[index] {
                continue;
            }
            let due = next_due[index].unwrap_or(anchor + Duration::from_millis(offsets[index]));
            if Instant::now() < due {
                continue;
            }
            if stage == Stage::BindingSupport && !closed {
                continue;
            }
            let slot_wall = Duration::from_millis(clock.wall_ms(options.slot_ms()));
            if Instant::now() > due + slot_wall {
                // The slot has gone -- consultation closed late, or this node
                // was busy. It is never caught up outside the slot: a late
                // frame lands in whoever's slot is current, and a liveness
                // problem for one member would become a collision for two. The
                // attempt is spent and the next one waits for this member's
                // own slot in the next window.
                attempts[index] += 1;
                next_due[index] = Some(
                    anchor
                        + Duration::from_millis(
                            offsets[index] + attempts[index] as u64 * options.retry_gap_ms(&clock),
                        ),
                );
                report(
                    &options,
                    &clock,
                    &format!(
                        "{{\"event\":\"slot_missed\",\"stage\":\"{}\",\"attempt\":{}}}",
                        stage_name(stage),
                        attempts[index]
                    ),
                );
                continue;
            }
            if stage == Stage::BindingSupport && attempts[index] > 0 && acknowledged {
                sent[index] = true;
                report(
                    &options,
                    &clock,
                    &format!(
                        "{{\"event\":\"acknowledged\",\"after_attempts\":{}}}",
                        attempts[index]
                    ),
                );
                continue;
            }
            match build(
                stage,
                &subject,
                &keys,
                &group,
                &mut journal,
                &options,
                heard,
                round,
            ) {
                Ok(bytes) => {
                    link.send(&bytes);
                    attempts[index] += 1;
                    // Our own sequence goes in the window as well. Without it a
                    // replay of our own earlier frame passes the cheap check
                    // and reaches the state machine, which refuses it -- but
                    // only after paying for a signature verification.
                    if let Ok((_, sequence)) = peek_frame_header(&bytes) {
                        last_seen[options.index] = Some(sequence);
                    }
                    // The next attempt returns to this member's own slot in the
                    // next window -- unless a split is known, in which case the
                    // schedule is what is colliding and the retry goes to a
                    // random point in the window instead.
                    let jitter = if split {
                        entropy_below(clock.wall_ms(options.stage_window_s() * 1_000))
                    } else {
                        0
                    };
                    next_due[index] = Some(
                        anchor
                            + Duration::from_millis(
                                offsets[index]
                                    + attempts[index] as u64 * options.retry_gap_ms(&clock)
                                    + jitter,
                            ),
                    );
                    // A node needs no radio to know its own utterance, and the
                    // emulator does not echo. Without this its own binding vote
                    // is missing from its own tally -- which happened to clear
                    // the threshold for a fleet of five and would not have for
                    // any other size.
                    if !own_admitted[index] {
                        own_admitted[index] = true;
                        let learned = admit(
                            &bytes, &subject, &manifest, &group, &mut case, &clock, &options,
                        );
                        if let Some(from) = learned.binding_from {
                            heard.heard_from(from);
                        }
                    }
                    if allowed == 1 {
                        sent[index] = true;
                    }
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
                    attempts[index] = allowed;
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

        // ---- repair rounds: ask for what is missing, resend what was asked ----
        if closed
            && options.adversary != Adversary::Jam
            && let Some(round_index) = repair_round()
            && nacked_rounds <= round_index
            && Instant::now()
                >= repair_start
                    + window_wall * 2 * u32::try_from(round_index).unwrap_or(u32::MAX)
                    + own_slot
        {
            nacked_rounds = round_index + 1;
            // Everyone in the manifest this node has no binding vote from. Not
            // just members heard from: a member that restarts has heard almost
            // nobody, and the ones it needs most are done transmitting. Asking
            // costs nothing extra -- the request carries what is HELD, so a
            // member that is simply absent adds no bytes and sends no reply.
            let missing: Vec<usize> = (0..options.fleet)
                .filter(|m| *m != options.index && !heard.contains(*m))
                .collect();
            if !missing.is_empty() {
                match build_nack(
                    &subject,
                    &keys[options.index],
                    &group,
                    &mut journal,
                    &options,
                    heard,
                    round,
                ) {
                    Ok(bytes) => {
                        link.send(&bytes);
                        let list: Vec<String> = missing.iter().map(ToString::to_string).collect();
                        report(
                            &options,
                            &clock,
                            &format!(
                                "{{\"event\":\"nack\",\"round\":{round_index},\"missing\":[{}]}}",
                                list.join(",")
                            ),
                        );
                    }
                    Err(why) => report(
                        &options,
                        &clock,
                        &format!("{{\"event\":\"not_sent\",\"stage\":\"nack\",\"why\":\"{why}\"}}"),
                    ),
                }
            }
        }
        if let Some(at) = repair_due
            && Instant::now() >= at
        {
            repair_due = None;
            if let Some(again) = journal.pending().next().map(|f| f.bytes().to_vec()) {
                link.send(&again);
                report(&options, &clock, "{\"event\":\"repaired\"}");
            }
        }

        // At most one frame per turn, and only after the send checks above have
        // had theirs. Verifying a signature is not free, and a node that misses
        // its own slot because it was busy reading is a node that collides with
        // whoever comes next.
        if let Some(frame) = link.poll() {
            if is_replay(&frame, &last_seen) {
                report(&options, &clock, "{\"event\":\"replay_dropped\"}");
                continue;
            }
            if let Some((other, opener, sequence)) =
                trigger_round(&frame, &group, &manifest, &subject)
            {
                last_seen[opener] = Some(sequence);
                if other != round {
                    // A second, validly signed opening for the same subject.
                    // Signatures do not stop a member saying two things; what
                    // they give is evidence, and the whole frame is kept as
                    // such. Whether it changes anything is decided below, by
                    // how many independent members disagree with us.
                    if evidence.len() < 4 {
                        evidence.push(frame.clone());
                        report(
                            &options,
                            &clock,
                            &format!(
                                "{{\"event\":\"evidence\",\"opener\":{opener},\"kept\":{}}}",
                                evidence.len()
                            ),
                        );
                    }
                    foreign.insert(opener);
                }
                split = note_split(split, &foreign, round, &options, &clock);
                continue;
            }
            let learned = admit(
                &frame, &subject, &manifest, &group, &mut case, &clock, &options,
            );
            if options.adversary == Adversary::Replay && captured.is_none() {
                captured = Some(frame.clone());
            }
            if let Some((from, sequence)) = learned.verified {
                last_seen[from] = Some(sequence);
                if learned.refused == Some(TransitionError::WrongStageForPhase)
                    && learned.stage_index == Some(2)
                {
                    early.insert(from, frame.clone());
                }
                // Somebody is short of our vote. Resend it once, in our own
                // slot of the resend window, from the outbox -- the same bytes
                // the journal committed, never a fresh decision.
                if learned.nack
                    && !learned.heard.contains(options.index)
                    && repair_due.is_none()
                    && journal.has_voted(&subject, &member_id(options.index))
                    && let Some(round_index) = repair_round()
                    && repaired_round != Some(round_index)
                {
                    repaired_round = Some(round_index);
                    let resend_window = u32::try_from(2 * round_index + 1).unwrap_or(u32::MAX);
                    repair_due = Some(repair_start + window_wall * resend_window + own_slot);
                }
            }
            if let Some((from, _)) = learned.verified {
                let by_label = learned.round.is_set() && learned.round != round;
                // A vote's timing implies where its sender thinks the round
                // began. Off by more than a slot means it is counting from a
                // different instant than we are -- the same split, seen from
                // its effect rather than its label, which is the only way it
                // shows when both halves carry the same label.
                let by_timing = options.slots
                    && learned.stage_index.is_some_and(|s| {
                        let travel = Duration::from_millis(clock.wall_ms(
                            starts[s] * 1_000
                                + options.slot_ms() * from as u64
                                + airtime_ms(frame.len()),
                        ));
                        let implied = Instant::now().checked_sub(travel);
                        implied.is_some_and(|at| {
                            let drift = if at > anchor {
                                at - anchor
                            } else {
                                anchor - at
                            };
                            // Modulo the window. A retransmission arrives one
                            // or more whole windows after the first attempt,
                            // still in its own slot, so its drift is a multiple
                            // of the window and must not read as a foreign
                            // anchor -- the first version of this check made
                            // every honest fleet with retries declare a split.
                            // The reduction loses nothing: two anchors exactly
                            // a window apart put every slot on top of its own
                            // twin, which collides with nobody.
                            let window = clock.wall_ms(options.stage_window_s() * 1_000).max(1);
                            let drift_ms = u64::try_from(drift.as_millis()).unwrap_or(u64::MAX);
                            let residue = drift_ms % window;
                            let off = residue.min(window - residue);
                            off > clock.wall_ms(options.slot_ms())
                        })
                    });
                if by_label || by_timing {
                    foreign.insert(from);
                }
                split = note_split(split, &foreign, round, &options, &clock);
            }
            if let Some(from) = learned.binding_from {
                heard.heard_from(from);
                if let Err(error) = journal.witness(&member_id(from), &frame) {
                    report(
                        &options,
                        &clock,
                        &format!(
                            "{{\"event\":\"not_durable\",\"what\":\"witness\",\"why\":\"{error}\"}}"
                        ),
                    );
                }
            }
            // Somebody reported hearing us, so repeating ourselves would buy
            // nothing. Advisory only: a liar can silence one member per round,
            // which is a liveness attack of the same class as jamming a slot
            // and never lets anyone forge a signature.
            if learned.acknowledges_us && !options.ignore_acks {
                acknowledged = true;
            }
        }

        // The schedule lives on the anchor's timeline, not on this node's
        // clock: a member fifteen seconds ahead would otherwise decide the
        // round was over before its own binding slot. Only the subject's own
        // deadlines belong to the local clock, and the state machine applies
        // those itself. Listening stops when endorsement is expected, or
        // earlier once this node has nothing left to send and has seen a
        // quorum -- never merely because its own frames are out.
        let horizon =
            anchor + Duration::from_millis(clock.wall_ms(ENDORSEMENT_TARGET_SECONDS * 1_000));
        // Not "as soon as a quorum is seen": that is reached on the fourth
        // vote of five, with the fifth member's slot still to come, and a node
        // that left then would report four. The scheduled window has to have
        // run its course first, so every member's slot has had its turn.
        let scheduled_end = anchor + Duration::from_millis(clock.wall_ms(finish * 1_000));
        let endorsed_now = Instant::now() >= scheduled_end && {
            let supporters: Vec<String> =
                case.binding_supporters().map(ToString::to_string).collect();
            evaluate(&policy, supporters).is_ok_and(|o| o.approved())
        };
        if Instant::now() >= horizon || endorsed_now {
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
            "{{\"event\":\"final\",\"index\":{},\"supporters\":{},\"threshold\":{},\"endorsed\":{},\"recovered_vote\":{recovered_vote},\"binding_attempts\":{},\"acknowledged\":{acknowledged}}}",
            options.index,
            supporters.len(),
            policy.min_signers(),
            approved,
            attempts[2]
        ),
    );
}

/// Build this node's frame for a stage, journalling a binding vote first.
#[allow(clippy::too_many_arguments)]
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
    heard: Heard,
    round: RoundId,
) -> Result<Vec<u8>, String> {
    let voter = member_id(options.index);
    let already = stage == Stage::BindingSupport && journal.has_voted(subject, &voter);
    if already && options.adversary != Adversary::DoubleVote {
        return journal
            .pending()
            .next()
            .map(|frame| frame.bytes().to_vec())
            .ok_or_else(|| "already voted, nothing left in the outbox".to_string());
    }
    // A double-voter holds a lock and builds a fresh, contradicting vote
    // regardless. The lock is the honest member's discipline; what stops the
    // second vote counting anywhere is every receiver's own state machine.
    let verdict = if already { 2 } else { 1 };
    let heard = if options.adversary == Adversary::LieAcks {
        let mut everyone = Heard::none();
        for member in 0..Heard::CAPACITY {
            everyone.heard_from(member);
        }
        everyone
    } else {
        heard
    };
    // Holding the group key gets a frame decrypted; only the manifest key for
    // the claimed index gets it believed. A forger has the first and not the
    // second.
    let signing = if options.adversary == Adversary::Forge {
        &keys[(options.index + 1) % keys.len()]
    } else {
        &keys[options.index]
    };

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
        verdict,
        sequence,
    )
    .acknowledging(heard)
    .in_round(round);
    let signed = encode_compact(&envelope.sign(signing)).map_err(|e| e.to_string())?;
    let sealed = seal_frame(group, author, sequence, &signed).map_err(|e| e.to_string())?;
    if sealed.len() > MAX_FRAME_BYTES {
        // The slot is sized to MAX_FRAME_BYTES. A wider frame would overrun
        // into the next member's slot, so it is not sent at all.
        return Err(format!(
            "frame is {} B, over the {MAX_FRAME_BYTES} B a slot is sized for",
            sealed.len()
        ));
    }

    if stage == Stage::BindingSupport && !already {
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

/// A repair request: what this member holds, so others can see what it lacks.
fn build_nack(
    subject: &Subject,
    key: &SigningKey,
    group: &GroupKey,
    journal: &mut LogJournal,
    options: &Options,
    heard: Heard,
    round: RoundId,
) -> Result<Vec<u8>, String> {
    let sequence = journal
        .reserve_sequence()
        .map_err(|error| error.to_string())?;
    let author = u16::try_from(options.index).unwrap_or(u16::MAX);
    let envelope = CompactEnvelope::new(
        MISSION_EPOCH,
        EVENT,
        REVISION,
        *subject.content_hash(),
        subject.started_at().as_secs(),
        author,
        NACK_STAGE,
        0,
        sequence,
    )
    .acknowledging(heard)
    .in_round(round);
    let signed = encode_compact(&envelope.sign(key)).map_err(|e| e.to_string())?;
    seal_frame(group, author, sequence, &signed).map_err(|e| e.to_string())
}

/// The frame that opens a round: authenticated, carrying no opinion.
fn build_trigger(
    subject: &Subject,
    key: &SigningKey,
    group: &GroupKey,
    journal: &mut LogJournal,
    options: &Options,
    round: RoundId,
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
    )
    .in_round(round);
    let signed = encode_compact(&envelope.sign(key)).map_err(|e| e.to_string())?;
    seal_frame(group, author, sequence, &signed).map_err(|e| e.to_string())
}

/// Whether a frame is a round opener rather than an utterance.
/// Decide whether the disagreements seen so far amount to a split, once.
fn note_split(
    already: bool,
    foreign: &BTreeSet<usize>,
    round: RoundId,
    options: &Options,
    clock: &impl Clock,
) -> bool {
    if already || foreign.len() < 2 {
        return already;
    }
    let members: Vec<String> = foreign.iter().map(ToString::to_string).collect();
    report(
        options,
        clock,
        &format!(
            "{{\"event\":\"split\",\"round\":[{},{}],\"disagreeing\":[{}]}}",
            round.opener(),
            round.sequence(),
            members.join(",")
        ),
    );
    true
}

/// When a round began, worked out from a vote heard inside it.
struct LateAnchor {
    origin: Instant,
    round: RoundId,
    author: usize,
    sequence: u64,
}

/// Derive the round's anchor from a verified in-round frame.
///
/// Only under slotted access, where a member's transmission time is a pure
/// function of its index and the stage; under random contention there is
/// nothing to derive from. The emulator -- and a radio -- hands over a frame
/// when it ends, so the anchor sits one airtime plus the sender's offset
/// before the moment of receipt.
fn late_anchor(
    sealed: &[u8],
    group: &GroupKey,
    manifest: &[VerifyingKey],
    subject: &Subject,
    starts: &[u64; 3],
    options: &Options,
    clock: &ScaledClock,
) -> Option<LateAnchor> {
    if !options.slots {
        return None;
    }
    let (header_author, header_sequence, plain) = open_frame(group, sealed).ok()?;
    let frame = decode_compact(&plain).ok()?;
    let envelope = frame.envelope();
    // A repair request is sent in its author's slot of a later window, so it
    // implies an anchor off by whole windows -- which the slot schedule cannot
    // tell from the right one, and which the modulo-window split check does
    // not mistake for a foreign anchor. Good enough to join on.
    let stage_index = match envelope.stage() {
        1 => 0,
        2 => 1,
        3 | NACK_STAGE => 2,
        _ => return None,
    };
    let author = usize::from(envelope.author_index());
    if !same_subject(envelope, subject)
        || author >= manifest.len()
        || frame.verify(&manifest[author]).is_err()
        || usize::from(header_author) != author
        || header_sequence != envelope.sequence()
        || !envelope.round().is_set()
    {
        return None;
    }
    let protocol_ms =
        starts[stage_index] * 1_000 + options.slot_ms() * author as u64 + airtime_ms(sealed.len());
    let origin = Instant::now().checked_sub(Duration::from_millis(clock.wall_ms(protocol_ms)))?;
    Some(LateAnchor {
        origin,
        round: envelope.round(),
        author,
        sequence: envelope.sequence(),
    })
}

/// Whether a frame repeats a sequence already verified from its sender.
fn is_replay(frame: &[u8], last_seen: &[Option<u64>]) -> bool {
    peek_frame_header(frame).is_ok_and(|(author, sequence)| {
        last_seen
            .get(usize::from(author))
            .copied()
            .flatten()
            .is_some_and(|seen| sequence <= seen)
    })
}

/// The round a frame opens, if it opens one -- and only if it is genuine.
///
/// Holding the group key gets a frame decrypted; it does not make its author
/// anybody in particular. An opening that is not signed by the member it
/// claims to be from would let any member open rounds as any other, so the
/// signature is checked here exactly as it is for a vote. The round is then
/// bound to the frame itself: named by the very member and sequence that sent
/// it, so there is nothing to forge and nothing to collide.
fn trigger_round(
    sealed: &[u8],
    group: &GroupKey,
    manifest: &[VerifyingKey],
    subject: &Subject,
) -> Option<(RoundId, usize, u64)> {
    let (header_author, header_sequence, plain) = open_frame(group, sealed).ok()?;
    let frame = decode_compact(&plain).ok()?;
    let envelope = frame.envelope();
    if envelope.stage() != TRIGGER_STAGE || !same_subject(envelope, subject) {
        return None;
    }
    let claimed = usize::from(envelope.author_index());
    if claimed >= manifest.len() || frame.verify(&manifest[claimed]).is_err() {
        return None;
    }
    if usize::from(header_author) != claimed || header_sequence != envelope.sequence() {
        return None;
    }
    let round = envelope.round();
    if usize::from(round.opener()) != claimed || u64::from(round.sequence()) != envelope.sequence()
    {
        return None;
    }
    Some((round, claimed, envelope.sequence()))
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
) -> Admitted {
    let mut learned = Admitted::default();
    let Ok((header_author, header_sequence, plain)) = open_frame(group, sealed) else {
        return learned;
    };
    let Ok(received) = decode_compact(&plain) else {
        return learned;
    };
    if !same_subject(received.envelope(), subject) {
        report(
            options,
            clock,
            "{\"event\":\"refused\",\"why\":\"subject\"}",
        );
        return learned;
    }
    let claimed = received.envelope().author_index() as usize;
    // The header is authenticated by the seal and the envelope by the
    // signature; a frame whose two authors disagree was assembled by somebody
    // with the group key and not the member's key, and is refused whole.
    if usize::from(header_author) != claimed || header_sequence != received.envelope().sequence() {
        report(options, clock, "{\"event\":\"refused\",\"why\":\"header\"}");
        return learned;
    }
    if claimed >= manifest.len() || received.verify(&manifest[claimed]).is_err() {
        report(
            options,
            clock,
            "{\"event\":\"refused\",\"why\":\"signature\"}",
        );
        return learned;
    }
    // Whatever the state machine decides about the utterance, the sender's
    // report of who IT heard is signed and worth reading -- that is the whole
    // point of carrying it.
    learned.verified = Some((claimed, received.envelope().sequence()));
    learned.round = received.envelope().round();
    learned.stage_index = match received.envelope().stage() {
        1 => Some(0),
        2 => Some(1),
        3 => Some(2),
        _ => None,
    };
    learned.acknowledges_us = received.envelope().heard().contains(options.index);
    learned.heard = received.envelope().heard();
    learned.nack = received.envelope().stage() == NACK_STAGE;
    let (Some(stage), Some(verdict)) = (
        stage_of(received.envelope().stage()),
        verdict_of(received.envelope().verdict()),
    ) else {
        return learned;
    };
    let opinion = Opinion::new(&member_id(claimed), subject.clone(), stage, verdict);
    match case.accept(opinion, clock) {
        Ok(()) => {
            if stage == Stage::BindingSupport {
                learned.binding_from = Some(claimed);
            }
            report(
                options,
                clock,
                &format!(
                    "{{\"event\":\"admitted\",\"from\":{claimed},\"stage\":\"{}\"}}",
                    stage_name(stage)
                ),
            );
        }
        Err(error) => {
            learned.refused = Some(error);
            report(
                options,
                clock,
                &format!("{{\"event\":\"refused\",\"from\":{claimed},\"why\":\"{error}\"}}"),
            );
        }
    }
    learned
}

/// What reading one frame taught this node.
#[derive(Debug, Default, Clone, Copy)]
struct Admitted {
    /// The signature verified: which member, under which sequence.
    verified: Option<(usize, u64)>,
    /// The round the sender declared it was in.
    round: RoundId,
    /// Which stage the frame belonged to, as an index into the schedule.
    stage_index: Option<usize>,
    /// A binding vote was admitted from this member.
    binding_from: Option<usize>,
    /// The sender reports having heard us, so we need not repeat ourselves.
    acknowledges_us: bool,
    /// Why the state machine refused the utterance, if it did.
    refused: Option<TransitionError>,
    /// The frame was a repair request rather than an utterance.
    nack: bool,
    /// Who the sender reports holding binding votes from.
    heard: Heard,
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
    attempts: usize,
    ignore_acks: bool,
    adversary: Adversary,
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
            // Not zero. A member that opens a round the instant it boots opens
            // it for nobody, and under distributed opening every member is a
            // potential opener.
            trigger_delay_ms: value("--trigger-delay-ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5_000),
            attempts: value("--attempts")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            ignore_acks: args.iter().any(|a| a == "--ignore-acks"),
            adversary: Adversary::parse(value("--adversary").as_deref()),
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

    /// Width of one slot in protocol milliseconds: the widest frame the
    /// protocol can send, plus the guard. Sized to the constant, never to the
    /// frame in hand, so adding a field can only make a sender refuse.
    fn slot_ms(&self) -> u64 {
        airtime_ms(MAX_FRAME_BYTES) + self.guard_ms
    }

    /// How long after start this member opens the round itself, unless it has
    /// heard an opening by then.
    ///
    /// Any member may open, so no single one is a point of failure. They take
    /// turns rather than racing: the order rotates with the subject's content
    /// so the same low index does not open every round -- a compromised member
    /// that always opened first would be the fleet's de facto scheduler -- and
    /// each waits two slot widths longer than the one before, enough for the
    /// previous member's opening to be heard before the next is sent. Explicit
    /// `--trigger` puts a member first, for harnesses that need to know who.
    fn open_deadline(&self, clock: &ScaledClock) -> Duration {
        let base = Duration::from_millis(self.trigger_delay_ms);
        if self.trigger {
            return base;
        }
        let start = usize::from(CONTENT_HASH[0]) % self.fleet.max(1);
        let rank = (self.index + self.fleet - start) % self.fleet.max(1);
        let turn = clock.wall_ms(2 * self.slot_ms());
        base + Duration::from_millis((rank as u64 + 1) * turn)
    }

    /// Wall milliseconds between retransmissions: one whole round, so a repeat
    /// lands in this member's slot again rather than in somebody else's.
    fn retry_gap_ms(&self, clock: &ScaledClock) -> u64 {
        clock.wall_ms(self.stage_window_s() * 1_000)
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

/// The index a member id names, if it is one this node issues.
fn member_index(id: &str) -> Option<usize> {
    id.strip_prefix('n')?.parse().ok()
}

fn member_id(index: usize) -> String {
    format!("n{index}")
}

fn seed_for(index: usize) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
    seed
}
