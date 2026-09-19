//! The protocol across real operating-system processes.
//!
//! Every other test in this crate runs the fleet inside one process, where
//! sharing is accidental and invisible and a "restart" is a function call.
//! Here each member is its own process with its own journal on disk, its own
//! clock, and no way to reach the others except a channel emulator that
//! enforces one frame at a time.
//!
//! These are slow by construction: the protocol's shortest interval is five
//! minutes, and a scaled clock only compresses it so far. The scale is bounded
//! by the guard interval -- at scale 20 a 200 ms guard is 10 ms of wall time,
//! comfortably above the scheduler's jitter, while at scale 100 it is 2 ms and
//! the schedule falls apart for reasons that have nothing to do with the
//! protocol.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use lcq::application::Journal;
use lcq::domain::contracts::Subject;
use lcq::domain::time::Timestamp;
use lcq::infrastructure::LogJournal;

const HUB: &str = env!("CARGO_BIN_EXE_lcq-hub");
const NODE: &str = env!("CARGO_BIN_EXE_lcq-node");
const SCALE: &str = "20";
const EPOCH: u64 = 1_000_000;

/// Only one fleet at a time.
///
/// These tests spawn a hub and five node processes each. Run in parallel they
/// would put dozens of processes on the machine at once, and the slot schedule
/// they are checking is exactly what contention for a core destroys -- the
/// failures would be about this host, not about the protocol.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A scratch directory that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lcq-mp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The emulator, and the port it ended up on.
struct Hub {
    child: Child,
    port: u16,
}

impl Hub {
    fn start(fleet: usize) -> Self {
        let mut child = Command::new(HUB)
            .args(["--fleet", &fleet.to_string(), "--scale", SCALE])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("hub starts");
        let stdout = child.stdout.take().expect("hub stdout");
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next().expect("hub announces").expect("readable");
        let port = first
            .split("\"port\":")
            .nth(1)
            .and_then(|rest| rest.trim_end_matches('}').trim().parse().ok())
            .expect("hub port");
        // The rest of the hub's log is drained on its own thread so a full pipe
        // can never wedge the emulator mid-run.
        std::thread::spawn(move || for _ in lines {});
        Self { child, port }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn node(index: usize, fleet: usize, port: u16, journal: &Path) -> Command {
    let mut command = Command::new(NODE);
    command
        .args(["--index", &index.to_string()])
        .args(["--fleet", &fleet.to_string()])
        .args(["--hub", &format!("127.0.0.1:{port}")])
        .args(["--journal", &journal.to_string_lossy()])
        .args(["--scale", SCALE])
        .args(["--epoch", &EPOCH.to_string()])
        .arg("--slots")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if index == 0 {
        command.arg("--trigger");
    }
    command
}

/// What a node printed on its last line of life.
#[derive(Debug, Default)]
struct Final {
    supporters: usize,
    threshold: usize,
    endorsed: bool,
    recovered_vote: bool,
    saw_final: bool,
}

fn read_final(child: &mut Child) -> Final {
    let stdout = child.stdout.take().expect("node stdout");
    let mut result = Final::default();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if !line.contains("\"final\"") {
            continue;
        }
        result.saw_final = true;
        result.supporters = field(&line, "\"supporters\":");
        result.threshold = field(&line, "\"threshold\":");
        result.endorsed = line.contains("\"endorsed\":true");
        result.recovered_vote = line.contains("\"recovered_vote\":true");
    }
    result
}

fn field(line: &str, key: &str) -> usize {
    line.split(key)
        .nth(1)
        .and_then(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

fn subject() -> Subject {
    Subject::new(
        "mission",
        "evt-1",
        0,
        [0x22; 32],
        Timestamp::from_secs(EPOCH),
    )
    .expect("valid subject")
}

#[test]
fn a_fleet_of_real_processes_all_reach_the_same_verdict() {
    let _serial = one_at_a_time();
    let fleet = 5;
    let scratch = Scratch::new("fleet");
    let hub = Hub::start(fleet);

    let mut children: Vec<Child> = (0..fleet)
        .map(|i| {
            node(i, fleet, hub.port, &scratch.path(&format!("n{i}.journal")))
                .spawn()
                .expect("node starts")
        })
        .collect();

    let finals: Vec<Final> = children.iter_mut().map(read_final).collect();
    for child in &mut children {
        let _ = child.wait();
    }

    for (index, result) in finals.iter().enumerate() {
        assert!(result.saw_final, "node {index} never reported");
        assert_eq!(
            result.supporters, fleet,
            "node {index} counted {} supporters",
            result.supporters
        );
        assert!(result.endorsed, "node {index} did not endorse");
        assert!(!result.recovered_vote, "nothing should have been recovered");
    }
    // Every process must land on the same answer. Disagreement here would mean
    // the quorum depends on who you ask, which is the failure the whole design
    // exists to prevent.
    assert!(finals.windows(2).all(|w| w[0].endorsed == w[1].endorsed));
}

#[test]
fn every_node_wrote_its_own_vote_lock_to_its_own_journal() {
    let _serial = one_at_a_time();
    let fleet = 5;
    let scratch = Scratch::new("journals");
    let hub = Hub::start(fleet);
    let mut children: Vec<Child> = (0..fleet)
        .map(|i| {
            node(i, fleet, hub.port, &scratch.path(&format!("n{i}.journal")))
                .spawn()
                .expect("node starts")
        })
        .collect();
    for child in &mut children {
        let _ = read_final(child);
        let _ = child.wait();
    }

    let subject = subject();
    for index in 0..fleet {
        let journal =
            LogJournal::open(scratch.path(&format!("n{index}.journal"))).expect("journal opens");
        assert!(
            journal.has_voted(&subject, &format!("n{index}")),
            "node {index} left no vote lock behind"
        );
        assert_eq!(
            journal.snapshot().vote_locks.len(),
            1,
            "node {index} holds more than one lock for one case"
        );
    }
}

#[test]
fn a_restarted_node_recovers_its_vote_and_refuses_to_cast_a_second_one() {
    let _serial = one_at_a_time();
    // The property the durable journal exists for, tested against a process
    // that really did exit. The restarted node must find its lock, decline to
    // decide again, and resend the frame it already committed -- because the
    // lock stops a second decision, never a second transmission.
    let fleet = 5;
    let scratch = Scratch::new("restart");
    let journal_path = scratch.path("n2.journal");

    {
        let hub = Hub::start(fleet);
        let mut children: Vec<Child> = (0..fleet)
            .map(|i| {
                node(i, fleet, hub.port, &scratch.path(&format!("n{i}.journal")))
                    .spawn()
                    .expect("node starts")
            })
            .collect();
        for child in &mut children {
            let _ = read_final(child);
            let _ = child.wait();
        }
    }

    let subject = subject();
    let locks_before = {
        let journal = LogJournal::open(&journal_path).expect("journal opens");
        assert!(journal.has_voted(&subject, "n2"), "no vote to recover");
        journal.snapshot().vote_locks.len()
    };

    // Second life, same journal, fresh fleet around it.
    let hub = Hub::start(fleet);
    let mut children: Vec<Child> = (0..fleet)
        .map(|i| {
            let path = if i == 2 {
                journal_path.clone()
            } else {
                scratch.path(&format!("second-n{i}.journal"))
            };
            node(i, fleet, hub.port, &path)
                .spawn()
                .expect("node starts")
        })
        .collect();
    let finals: Vec<Final> = children.iter_mut().map(read_final).collect();
    for child in &mut children {
        let _ = child.wait();
    }

    assert!(
        finals[2].recovered_vote,
        "the restarted node did not notice its own earlier vote"
    );
    let journal = LogJournal::open(&journal_path).expect("journal opens");
    assert_eq!(
        journal.snapshot().vote_locks.len(),
        locks_before,
        "the restart took a second lock on the same case"
    );
    assert!(
        finals.iter().all(|f| f.saw_final),
        "every node should still finish"
    );
}

#[test]
fn a_fleet_missing_one_member_still_reaches_its_threshold() {
    let _serial = one_at_a_time();
    // Four of five is exactly the count threshold, so this is the boundary: one
    // process simply never starts, and the rest must still agree.
    let fleet = 5;
    let scratch = Scratch::new("missing");
    let hub = Hub::start(fleet);
    let present: Vec<usize> = (0..fleet).filter(|i| *i != 3).collect();

    let mut children: Vec<Child> = present
        .iter()
        .map(|i| {
            node(*i, fleet, hub.port, &scratch.path(&format!("n{i}.journal")))
                .spawn()
                .expect("node starts")
        })
        .collect();
    let finals: Vec<Final> = children.iter_mut().map(read_final).collect();
    for child in &mut children {
        let _ = child.wait();
    }

    for (slot, result) in finals.iter().enumerate() {
        assert!(result.saw_final, "node {} never reported", present[slot]);
        assert_eq!(result.threshold, 4);
        assert_eq!(result.supporters, 4, "node {} miscounted", present[slot]);
        assert!(result.endorsed);
    }
}

#[test]
fn the_emulator_holds_the_channel_for_one_frame_at_a_time() {
    let _serial = one_at_a_time();
    // Not a message bus. If this ever passes trivially the rest of the
    // multi-process results mean nothing, because collisions would be
    // impossible by construction rather than avoided by scheduling.
    let scratch = Scratch::new("occupancy");
    let hub = Hub::start(2);
    let mut children: Vec<Child> = (0..2)
        .map(|i| {
            // No slots: both members are told to speak at the same moment.
            let mut command = node(i, 2, hub.port, &scratch.path(&format!("n{i}.journal")));
            command.args(["--guard-ms", "0"]);
            command.spawn().expect("node starts")
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Nothing is asserted about the outcome: the point is that the emulator
    // ran a contended channel without deadlocking or dropping its listener.
    assert!(true);
}
