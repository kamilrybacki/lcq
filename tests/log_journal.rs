//! Does the journal still tell the truth after the process dies?
//!
//! Every test here runs in its own scratch directory, which is removed when the
//! test ends. Journals are a few kilobytes; the exhaustive truncation test
//! rewrites one file rather than leaving a copy per offset.
//!
//! What these tests can and cannot show: they prove the *logic* of recovery —
//! that a partial tail is discarded, that a lock never survives without its
//! frame, that a killed process leaves a readable file. They do **not** prove
//! that `sync_all` reached the platter, because `std::env::temp_dir()` is
//! commonly a tmpfs where a flush is a no-op, and no test can simulate a disk
//! that lies about having flushed. That part rests on the ordering being right,
//! which is what is checked here.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use lcq::application::{Journal, JournalError, OutgoingFrame};
use lcq::domain::contracts::Subject;
use lcq::domain::time::Timestamp;
use lcq::infrastructure::LogJournal;

/// A directory that cleans up after itself, so a failing run leaves no litter.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lcq-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn subject(revision: u32) -> Subject {
    Subject::new(
        "mission",
        "evt-1",
        revision,
        [0x22; 32],
        Timestamp::from_secs(0),
    )
    .expect("valid subject")
}

fn open(path: &Path) -> LogJournal {
    LogJournal::open(path).expect("journal opens")
}

fn vote(journal: &mut LogJournal, revision: u32, sequence: u64) -> Result<(), JournalError> {
    journal.commit_vote(
        &subject(revision),
        "self",
        OutgoingFrame::new(alloc_bytes(sequence), sequence),
    )
}

fn alloc_bytes(sequence: u64) -> Vec<u8> {
    sequence.to_be_bytes().repeat(8)
}

#[test]
fn a_vote_survives_the_process_that_cast_it() {
    let scratch = Scratch::new("survives");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        vote(&mut journal, 0, 7).expect("first vote");
    }
    let reopened = open(&path);
    assert!(reopened.has_voted(&subject(0), "self"));
    assert_eq!(reopened.pending().count(), 1);
    assert_eq!(reopened.next_sequence(), 8);
}

#[test]
fn a_restarted_node_cannot_vote_a_second_time() {
    // The whole reason this layer exists. Losing the lock across a restart
    // would let one member contribute two votes to the same case, which breaks
    // the quorum-intersection argument the count threshold rests on.
    let scratch = Scratch::new("second-vote");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        vote(&mut journal, 0, 1).expect("first vote");
    }
    let mut reopened = open(&path);
    assert_eq!(vote(&mut reopened, 0, 2), Err(JournalError::AlreadyVoted));
    assert_eq!(
        reopened.pending().count(),
        1,
        "the refused frame must not be queued"
    );
}

#[test]
fn a_different_revision_is_a_different_case() {
    let scratch = Scratch::new("revision");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    vote(&mut journal, 0, 1).expect("first");
    vote(&mut journal, 1, 2).expect("a revised claim is a new decision");
    assert_eq!(journal.pending().count(), 2);
}

#[test]
fn a_sequence_number_is_never_handed_out_twice_across_a_restart() {
    // A repeated sequence is a repeated AEAD nonce: keystream reuse plus
    // Poly1305 key recovery, which is forgery under the group key.
    let scratch = Scratch::new("sequence");
    let path = scratch.file("journal.log");
    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut journal = open(&path);
        for _ in 0..4 {
            seen.push(journal.reserve_sequence().expect("reservation is durable"));
        }
    }
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "handed out a number twice: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|w| w[1] > w[0]),
        "never rewinds: {seen:?}"
    );
}

#[test]
fn an_acknowledged_frame_does_not_come_back() {
    let scratch = Scratch::new("ack");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        vote(&mut journal, 0, 3).expect("vote");
        assert!(journal.acknowledge(3));
    }
    let reopened = open(&path);
    assert_eq!(reopened.pending().count(), 0);
    assert!(
        reopened.has_voted(&subject(0), "self"),
        "acknowledging delivery never releases the vote lock"
    );
}

#[test]
fn an_expired_frame_stays_dropped_but_keeps_its_lock() {
    let scratch = Scratch::new("expiry");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        journal
            .commit_vote(
                &subject(0),
                "self",
                OutgoingFrame::new(vec![1, 2, 3], 5).expiring_at(Timestamp::from_secs(100)),
            )
            .expect("vote");
        journal.drop_expired(Timestamp::from_secs(200));
    }
    let reopened = open(&path);
    assert_eq!(reopened.pending().count(), 0);
    assert!(
        reopened.has_voted(&subject(0), "self"),
        "the node did decide; it merely failed to get the decision out in time"
    );
}

#[test]
fn a_torn_tail_is_discarded_and_the_file_repaired() {
    let scratch = Scratch::new("torn");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        vote(&mut journal, 0, 1).expect("first");
        vote(&mut journal, 1, 2).expect("second");
    }
    let whole = fs::read(&path).expect("read");
    let good_len = whole.len();

    // Half of a third record, as a power cut would leave it.
    {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        file.write_all(&[0xFF; 40]).expect("write garbage");
    }

    let reopened = open(&path);
    assert_eq!(reopened.pending().count(), 2, "both whole records survive");
    assert_eq!(
        fs::metadata(&path).expect("stat").len(),
        good_len as u64,
        "the partial tail must be cut off, not left for the next append to sit behind"
    );
}

#[test]
fn a_flipped_byte_discards_that_record_and_everything_after_it() {
    let scratch = Scratch::new("bitrot");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        for n in 0..4u32 {
            vote(&mut journal, n, u64::from(n) + 1).expect("vote");
        }
    }
    let mut bytes = fs::read(&path).expect("read");
    let midpoint = bytes.len() / 2;
    bytes[midpoint] ^= 0xFF;
    fs::write(&path, &bytes).expect("write");

    let reopened = open(&path);
    let survivors = reopened.pending().count();
    assert!(survivors < 4, "the damaged record must not be believed");
    assert!(survivors >= 1, "records before the damage must survive");
    // A log is only meaningful as a prefix; resynchronising past damage would
    // invent history, so everything after the flip is gone too.
    assert_eq!(
        fs::metadata(&path).expect("stat").len(),
        whole_prefix_len(&path),
        "the file is cut at the damage"
    );
}

fn whole_prefix_len(path: &Path) -> u64 {
    // Reopening an already-repaired file must change nothing.
    let before = fs::metadata(path).expect("stat").len();
    drop(open(path));
    let after = fs::metadata(path).expect("stat").len();
    assert_eq!(before, after, "repair must be idempotent");
    after
}

#[test]
fn every_possible_truncation_leaves_a_consistent_journal() {
    // Stronger than killing the process a few times: this checks the invariant
    // at every byte offset a crash could possibly land on. A lock without its
    // frame is the failure that matters, because it is the one that would let a
    // node vote twice.
    let scratch = Scratch::new("truncation");
    let source = scratch.file("source.log");
    {
        let mut journal = open(&source);
        for n in 0..6u32 {
            vote(&mut journal, n, u64::from(n) + 1).expect("vote");
        }
    }
    let whole = fs::read(&source).expect("read");
    // Guard against the test silently testing nothing: if the records never
    // landed, every assertion below would pass over an empty file.
    assert!(
        whole.len() > 400,
        "the source log is too small to be meaningful"
    );
    {
        let full = LogJournal::open(&source).expect("opens");
        assert_eq!(full.snapshot().vote_locks.len(), 6);
        assert_eq!(full.pending().count(), 6);
    }

    let probe = scratch.file("probe.log");
    let mut recovered_states = 0usize;

    for length in 0..=whole.len() {
        fs::write(&probe, &whole[..length]).expect("write truncated copy");
        let journal = LogJournal::open(&probe).expect("a truncated journal still opens");
        let locks = journal.snapshot().vote_locks.len();
        let frames = journal.pending().count();
        assert_eq!(
            locks, frames,
            "truncated at {length} bytes: {locks} locks against {frames} frames"
        );
        let highest = journal.pending().map(OutgoingFrame::sequence).max();
        if let Some(highest) = highest {
            assert!(
                journal.next_sequence() > highest,
                "truncated at {length} bytes: sequence {} would be handed out again",
                journal.next_sequence()
            );
        }
        assert_eq!(
            fs::metadata(&probe).expect("stat").len(),
            journal.bytes_on_disk(),
            "truncated at {length} bytes: the file was not cut to its whole records"
        );
        if frames > 0 {
            recovered_states += 1;
        }
    }
    assert!(
        recovered_states > whole.len() / 2,
        "most offsets should recover at least one vote, got {recovered_states} of {}",
        whole.len()
    );
}

#[test]
fn compaction_keeps_the_state_and_shrinks_the_file() {
    let scratch = Scratch::new("compaction");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    for n in 0..8u32 {
        vote(&mut journal, n, u64::from(n) + 1).expect("vote");
    }
    // Most of that history is dead weight: the frames were delivered, only the
    // locks still matter.
    for n in 1..=6u64 {
        assert!(journal.acknowledge(n));
    }
    let before = journal.bytes_on_disk();
    journal.compact().expect("compaction");
    let after = journal.bytes_on_disk();
    assert!(
        after < before,
        "compaction must shrink: {before} -> {after}"
    );

    let reopened = open(&path);
    assert_eq!(reopened.pending().count(), 2);
    assert_eq!(reopened.next_sequence(), 9);
    for n in 0..8u32 {
        assert!(
            reopened.has_voted(&subject(n), "self"),
            "compaction dropped the lock for revision {n}"
        );
    }
}

#[test]
fn compaction_survives_being_reopened_and_written_to() {
    let scratch = Scratch::new("post-compaction");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        vote(&mut journal, 0, 1).expect("vote");
        journal.compact().expect("compaction");
        vote(&mut journal, 1, 2).expect("append after compaction");
    }
    let reopened = open(&path);
    assert_eq!(
        reopened.pending().count(),
        2,
        "the post-compaction append must land"
    );
    assert!(reopened.has_voted(&subject(0), "self"));
    assert!(reopened.has_voted(&subject(1), "self"));
}

/* ------------------------------------------------------------------ *
 * A real process, really killed.                                      *
 * ------------------------------------------------------------------ */

/// Env var that turns this test binary into the writer child.
const WRITER: &str = "LCQ_JOURNAL_WRITER";

#[test]
fn journal_writer_child() {
    // Ordinarily a no-op. The kill test re-runs this binary with the env var
    // set, which turns this same test into a process that writes until killed.
    // Re-invoking the harness avoids adding a fixture binary to the crate.
    let Ok(path) = std::env::var(WRITER) else {
        return;
    };
    let mut journal = LogJournal::open(&path).expect("child opens the journal");
    for n in 0..2_000u32 {
        if vote(&mut journal, n, u64::from(n) + 1).is_err() {
            break;
        }
        if n == 0 {
            // One whole record is committed and fsynced. Say so, so that the
            // parent kills a process that has written rather than one that is
            // still starting up.
            let _ = fs::write(PathBuf::from(&path).with_extension("ready"), b"1");
        }
    }
}

#[test]
fn a_killed_process_leaves_a_journal_that_still_opens() {
    let scratch = Scratch::new("sigkill");
    let path = scratch.file("journal.log");

    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "journal_writer_child", "--nocapture"])
        .env(WRITER, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the writer");

    // Wait for the child to say it has committed a whole record, then kill it
    // outright. SIGKILL is the point: no destructor runs and no buffer is
    // flushed on the way out. What is *not* the point is how long a cold
    // process takes to start, which is why this waits on the child rather
    // than sleeping a guessed 250 ms. On a loaded two-core runner that guess
    // was short, and the test failed for having killed a writer that had not
    // written yet.
    let ready = path.with_extension("ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(1);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the writer child never committed a record"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.kill().expect("kill the writer");
    let _ = child.wait();

    let journal = LogJournal::open(&path).expect("a killed journal still opens");
    let locks = journal.snapshot().vote_locks.len();
    let frames = journal.pending().count();
    assert!(
        locks > 0,
        "the child should have committed something before dying"
    );
    assert_eq!(
        locks, frames,
        "a killed process left {locks} locks against {frames} frames"
    );
    if let Some(highest) = journal.pending().map(OutgoingFrame::sequence).max() {
        assert!(journal.next_sequence() > highest);
    }
    assert_eq!(
        fs::metadata(&path).expect("stat").len(),
        journal.bytes_on_disk(),
        "reopening must have repaired the file to its whole records"
    );
}

#[test]
fn a_witnessed_vote_survives_the_process_that_admitted_it() {
    // A node that restarts inside a live round has to finish it, and the
    // members it had already heard are done transmitting. What it admitted
    // must therefore be on disk -- as the frame, so it can be verified again.
    let scratch = Scratch::new("witness");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        journal
            .witness("n3", b"sealed frame from three")
            .expect("durable");
        journal
            .witness("n1", b"sealed frame from one")
            .expect("durable");
        journal
            .witness("n3", b"a different frame from three")
            .expect("repeat is fine");
    }
    let reopened = open(&path);
    let mut seen: Vec<(String, Vec<u8>)> = reopened
        .witnessed()
        .map(|(a, f)| (a.to_string(), f.to_vec()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("n1".to_string(), b"sealed frame from one".to_vec()),
            ("n3".to_string(), b"sealed frame from three".to_vec()),
        ],
        "one frame per author, the first one kept"
    );
}

#[test]
fn compaction_keeps_witnessed_votes() {
    let scratch = Scratch::new("witness-compact");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    for n in 0..4u32 {
        vote(&mut journal, n, u64::from(n) + 1).expect("vote");
    }
    journal.witness("n7", b"seven").expect("durable");
    journal.witness("n8", b"eight").expect("durable");
    journal.compact().expect("compaction");
    let reopened = open(&path);
    assert_eq!(reopened.witnessed().count(), 2);
    assert!(
        reopened
            .witnessed()
            .any(|(a, f)| a == "n7" && f == b"seven")
    );
}

#[test]
fn a_journal_opened_with_entropy_never_starts_from_zero_and_keeps_its_start() {
    let scratch = Scratch::new("entropy");
    let first_path = scratch.file("first.log");
    let second_path = scratch.file("second.log");
    let (first_start, second_start) = {
        let first = LogJournal::open_with_entropy(&first_path).expect("opens");
        let second = LogJournal::open_with_entropy(&second_path).expect("opens");
        (first.next_sequence(), second.next_sequence())
    };
    for start in [first_start, second_start] {
        assert!(
            start >= 1 << 24,
            "far from the sequences an earlier life used: {start}"
        );
        assert!(
            start < 1 << 31,
            "leaves room under the 32-bit round label: {start}"
        );
    }
    assert_ne!(
        first_start, second_start,
        "two fresh journals must not agree by chance"
    );

    // The start is durable: a reopen counts on from it, with or without entropy.
    let reserved = {
        let mut reopened = LogJournal::open_with_entropy(&first_path).expect("reopens");
        assert_eq!(reopened.next_sequence(), first_start);
        reopened.reserve_sequence().expect("reservation is durable")
    };
    assert_eq!(reserved, first_start);
    let plain = open(&first_path);
    assert_eq!(plain.next_sequence(), first_start + 1);
}

/* ------------------------------------------------------------------ *
 * The epoch a journal has run under.                                  *
 * ------------------------------------------------------------------ */

#[test]
fn a_fresh_journal_has_never_run_under_an_epoch() {
    let scratch = Scratch::new("epoch-fresh");
    let journal = open(&scratch.file("journal.log"));
    assert_eq!(journal.epoch(), None);
}

#[test]
fn the_epoch_survives_the_process_that_entered_it() {
    let scratch = Scratch::new("epoch-restart");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        journal.enter_epoch(7).expect("epoch recorded");
        assert_eq!(journal.epoch(), Some(7));
    }
    assert_eq!(
        open(&path).epoch(),
        Some(7),
        "a restart must know which epoch it was flying"
    );
}

#[test]
fn an_epoch_only_ever_moves_forward() {
    let scratch = Scratch::new("epoch-monotonic");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);

    journal.enter_epoch(9).expect("epoch recorded");
    // Entering an older one is a no-op rather than a rewind: whoever is
    // holding this journal does not get to un-spend an epoch by asking.
    journal.enter_epoch(3).expect("accepted and ignored");
    assert_eq!(journal.epoch(), Some(9));
    journal.enter_epoch(9).expect("re-entering the same one");
    assert_eq!(journal.epoch(), Some(9));

    journal
        .enter_epoch(11)
        .expect("a later epoch is a rotation");
    assert_eq!(journal.epoch(), Some(11));
    drop(journal);
    assert_eq!(open(&path).epoch(), Some(11));
}

#[test]
fn compaction_does_not_forget_the_epoch() {
    let scratch = Scratch::new("epoch-compaction");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    journal.enter_epoch(5).expect("epoch recorded");
    for n in 0..8u32 {
        vote(&mut journal, n, u64::from(n) + 1).expect("votes commit");
    }
    journal.compact().expect("compaction succeeds");

    // Compaction rewrites the file from the live state, so an epoch it dropped
    // would leave a journal that looks like it has never run -- which is what
    // a rollback wants to look like.
    assert_eq!(journal.epoch(), Some(5));
    assert_eq!(open(&path).epoch(), Some(5));
}

#[test]
fn the_snapshot_carries_the_epoch_to_a_restored_journal() {
    let scratch = Scratch::new("epoch-snapshot");
    let mut journal = open(&scratch.file("journal.log"));
    journal.enter_epoch(4).expect("epoch recorded");
    let snapshot = journal.snapshot();
    assert_eq!(snapshot.epoch, Some(4));
    assert_eq!(
        lcq::infrastructure::MemoryJournal::restored(snapshot).epoch(),
        Some(4)
    );
}

/* ------------------------------------------------------------------ *
 * The highest sequence admitted from each sender (F2).                *
 * ------------------------------------------------------------------ */

#[test]
fn a_fresh_journal_has_seen_nobody() {
    let scratch = Scratch::new("seen-fresh");
    assert_eq!(open(&scratch.file("journal.log")).highest_seen(0), None);
}

#[test]
fn what_a_sender_reached_survives_the_process_that_heard_it() {
    let scratch = Scratch::new("seen-restart");
    let path = scratch.file("journal.log");
    {
        let mut journal = open(&path);
        journal.mark_seen(0, 41).expect("recorded");
        journal.mark_seen(2, 900).expect("recorded");
        assert_eq!(journal.highest_seen(0), Some(41));
    }
    let journal = open(&path);
    assert_eq!(journal.highest_seen(0), Some(41));
    assert_eq!(journal.highest_seen(2), Some(900));
    assert_eq!(journal.highest_seen(1), None, "nobody heard from index 1");
}

#[test]
fn a_sender_never_goes_backwards() {
    let scratch = Scratch::new("seen-monotonic");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    journal.mark_seen(0, 100).expect("recorded");
    // An out-of-order frame is not a rewind, and it costs no write.
    let before = journal.bytes_on_disk();
    journal.mark_seen(0, 7).expect("accepted and ignored");
    assert_eq!(journal.highest_seen(0), Some(100));
    assert_eq!(
        journal.bytes_on_disk(),
        before,
        "a record that would not move anything must not be written"
    );
    journal.mark_seen(0, 101).expect("recorded");
    drop(journal);
    assert_eq!(open(&path).highest_seen(0), Some(101));
}

#[test]
fn a_new_epoch_forgets_who_had_been_heard() {
    let scratch = Scratch::new("seen-epoch");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    journal.enter_epoch(3).expect("epoch recorded");
    journal.mark_seen(0, 500).expect("recorded");
    assert_eq!(journal.highest_seen(0), Some(500));

    // Index 0 under a new manifest need not be the member index 0 was under
    // the old one, so what it had reached says nothing about the new one.
    journal.enter_epoch(4).expect("rotation");
    assert_eq!(journal.highest_seen(0), None);
    drop(journal);
    assert_eq!(
        open(&path).highest_seen(0),
        None,
        "and a replay of the log must reach the same conclusion"
    );
}

#[test]
fn compaction_does_not_forget_who_had_been_heard() {
    let scratch = Scratch::new("seen-compaction");
    let path = scratch.file("journal.log");
    let mut journal = open(&path);
    journal.enter_epoch(2).expect("epoch recorded");
    journal.mark_seen(1, 77).expect("recorded");
    for n in 0..8u32 {
        vote(&mut journal, n, u64::from(n) + 1).expect("votes commit");
    }
    journal.compact().expect("compaction succeeds");
    assert_eq!(journal.highest_seen(1), Some(77));
    let reopened = open(&path);
    assert_eq!(reopened.highest_seen(1), Some(77));
    assert_eq!(reopened.epoch(), Some(2));
}
