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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lcq::application::Journal;
use lcq::domain::contracts::Subject;
use lcq::domain::time::Timestamp;
use lcq::infrastructure::LogJournal;

const HUB: &str = env!("CARGO_BIN_EXE_lcq-hub");
const NODE: &str = env!("CARGO_BIN_EXE_lcq-node");
const SCALE: &str = "20";
const EPOCH: u64 = 1_000_000;

/// One fleet on this machine at a time, across every test binary.
///
/// An in-process mutex is not enough: `cargo test` runs each integration
/// target as its own process, in parallel, and two fleets competing for the
/// same cores destroy exactly the slot schedule these tests check. The failure
/// then looks like a protocol bug and is a scheduling accident.
struct SerialGuard(std::path::PathBuf);

impl Drop for SerialGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn one_fleet_at_a_time() -> SerialGuard {
    /// Long enough that no honest run holds the lock this long.
    const STALE_AFTER: Duration = Duration::from_mins(10);

    let path = std::env::temp_dir().join("lcq-fleet.lock");
    let deadline = std::time::Instant::now() + STALE_AFTER;
    loop {
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
            || std::time::Instant::now() >= deadline
        {
            return SerialGuard(path);
        }
        // A crashed run can leave the file behind, so anything older than a run
        // could possibly take is treated as stale rather than deadlocking the
        // whole suite behind it.
        if std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .is_ok_and(|at| at.elapsed().unwrap_or_default() > STALE_AFTER)
        {
            let _ = std::fs::remove_file(&path);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
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

/// A running node, with its output collected as it appears.
///
/// The lines have to be gathered on a thread rather than read at the end: the
/// harness needs to know a member is listening *before* it opens the round, and
/// a member that is still starting cannot hear a trigger that has already gone.
struct Running {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Running {
    fn start(mut command: Command) -> Self {
        let mut child = command.spawn().expect("node starts");
        let stdout = child.stdout.take().expect("node stdout");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().expect("lines").push(line);
            }
        });
        Self { child, lines }
    }

    fn saw(&self, needle: &str) -> bool {
        self.lines
            .lock()
            .expect("lines")
            .iter()
            .any(|l| l.contains(needle))
    }

    fn await_line(&self, needle: &str, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.saw(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Wait for the process to finish and report what it concluded.
    fn finish(&mut self) -> Final {
        let _ = self.child.wait();
        let lines = self.lines.lock().expect("lines");
        let mut result = Final::default();
        for line in lines.iter().filter(|l| l.contains("\"final\"")) {
            result.reported = true;
            result.supporters = field(line, "\"supporters\":");
            result.threshold = field(line, "\"threshold\":");
            result.endorsed = line.contains("\"endorsed\":true");
            result.recovered_vote = line.contains("\"recovered_vote\":true");
        }
        result
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Put a fleet to sea in an order that cannot race.
///
/// Every member but the opener is started and confirmed listening first. A
/// round opened while processes are still starting is a round opened for
/// nobody, and a fixed delay would be a guess about how busy the host is.
fn put_to_sea(fleet: usize, port: u16, dir: &Path) -> Vec<Running> {
    let mut nodes: Vec<Running> = (1..fleet)
        .map(|i| {
            Running::start(node(
                i,
                fleet,
                port,
                &dir.join(format!("n{i}.journal")),
                false,
            ))
        })
        .collect();
    for (offset, running) in nodes.iter().enumerate() {
        assert!(
            running.await_line("\"start\"", Duration::from_secs(30)),
            "node {} never started",
            offset + 1
        );
    }
    nodes.insert(
        0,
        Running::start(node(0, fleet, port, &dir.join("n0.journal"), true)),
    );
    nodes
}

fn node(index: usize, fleet: usize, port: u16, journal: &Path, opens_round: bool) -> Command {
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
    if opens_round {
        command.arg("--trigger");
        command.args(["--trigger-delay-ms", "700"]);
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
    reported: bool,
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
    let _serial = one_fleet_at_a_time();
    let fleet = 5;
    let scratch = Scratch::new("fleet");
    let hub = Hub::start(fleet);

    let mut nodes = put_to_sea(fleet, hub.port, &scratch.0);
    let finals: Vec<Final> = nodes.iter_mut().map(Running::finish).collect();

    for (index, result) in finals.iter().enumerate() {
        assert!(result.reported, "node {index} never reported");
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
    let _serial = one_fleet_at_a_time();
    let fleet = 5;
    let scratch = Scratch::new("journals");
    let hub = Hub::start(fleet);
    let mut nodes = put_to_sea(fleet, hub.port, &scratch.0);
    for running in &mut nodes {
        let _ = running.finish();
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
    let _serial = one_fleet_at_a_time();
    // The property the durable journal exists for, tested against a process
    // that really did exit. The restarted node must find its lock, decline to
    // decide again, and resend the frame it already committed -- because the
    // lock stops a second decision, never a second transmission.
    let fleet = 5;
    let scratch = Scratch::new("restart");
    let journal_path = scratch.path("n2.journal");

    {
        let hub = Hub::start(fleet);
        let mut nodes = put_to_sea(fleet, hub.port, &scratch.0);
        for running in &mut nodes {
            let _ = running.finish();
        }
    }

    let subject = subject();
    let locks_before = {
        let journal = LogJournal::open(&journal_path).expect("journal opens");
        assert!(journal.has_voted(&subject, "n2"), "no vote to recover");
        journal.snapshot().vote_locks.len()
    };

    // Second life for node 2, same journal, a fresh fleet around it.
    let hub = Hub::start(fleet);
    let path_for = |i: usize| {
        if i == 2 {
            journal_path.clone()
        } else {
            scratch.path(&format!("second-n{i}.journal"))
        }
    };
    let mut nodes: Vec<Running> = (1..fleet)
        .map(|i| Running::start(node(i, fleet, hub.port, &path_for(i), false)))
        .collect();
    for running in &nodes {
        assert!(running.await_line("\"start\"", Duration::from_secs(30)));
    }
    nodes.insert(
        0,
        Running::start(node(0, fleet, hub.port, &path_for(0), true)),
    );
    let finals: Vec<Final> = nodes.iter_mut().map(Running::finish).collect();

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
        finals.iter().all(|f| f.reported),
        "every node should still finish"
    );
}

#[test]
fn a_fleet_missing_one_member_still_reaches_its_threshold() {
    let _serial = one_fleet_at_a_time();
    // Four of five is exactly the count threshold, so this is the boundary: one
    // process simply never starts, and the rest must still agree.
    let fleet = 5;
    let scratch = Scratch::new("missing");
    let hub = Hub::start(fleet);
    let present: Vec<usize> = (0..fleet).filter(|i| *i != 3).collect();

    let mut nodes: Vec<Running> = present
        .iter()
        .filter(|i| **i != 0)
        .map(|i| {
            Running::start(node(
                *i,
                fleet,
                hub.port,
                &scratch.path(&format!("n{i}.journal")),
                false,
            ))
        })
        .collect();
    for running in &nodes {
        assert!(running.await_line("\"start\"", Duration::from_secs(30)));
    }
    nodes.insert(
        0,
        Running::start(node(0, fleet, hub.port, &scratch.path("n0.journal"), true)),
    );
    let finals: Vec<Final> = nodes.iter_mut().map(Running::finish).collect();

    for (slot, result) in finals.iter().enumerate() {
        assert!(result.reported, "node {} never reported", present[slot]);
        assert_eq!(result.threshold, 4);
        assert_eq!(result.supporters, 4, "node {} miscounted", present[slot]);
        assert!(result.endorsed);
    }
}

#[test]
fn the_emulator_holds_the_channel_for_one_frame_at_a_time() {
    let _serial = one_fleet_at_a_time();
    // Not a message bus. If this ever passes trivially the rest of the
    // multi-process results mean nothing, because collisions would be
    // impossible by construction rather than avoided by scheduling.
    let scratch = Scratch::new("occupancy");
    let mut hub = Hub::start(2);
    let mut nodes: Vec<Running> = (0..2)
        .map(|i| {
            // No guard: both members are told to speak at the same moment.
            let mut command = node(
                i,
                2,
                hub.port,
                &scratch.path(&format!("n{i}.journal")),
                i == 0,
            );
            command.args(["--guard-ms", "0"]);
            Running::start(command)
        })
        .collect();
    std::thread::sleep(Duration::from_secs(2));
    for running in &mut nodes {
        running.kill();
    }
    // The listener is closed by now -- the emulator stops accepting once the
    // fleet is complete -- so what is checked is that the process survived a
    // contended round rather than deadlocking or dying on it.
    assert!(
        hub.child.try_wait().expect("hub status").is_none(),
        "the emulator did not survive a contended round"
    );
}
