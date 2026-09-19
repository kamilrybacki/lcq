//! The fleet as containers: one per vessel, isolated from each other.
//!
//! The multi-process tests put five members on one host, sharing a filesystem,
//! a network stack and a process tree. Here each is a container with its own
//! root filesystem, its own network namespace and its own journal on a mount
//! only it can see, reaching the others over a real network.
//!
//! The harness drives Docker from code: it builds the image, launches the
//! ships, sinks them and refloats them, and tears everything down. There is no
//! compose file and no fixture to keep in step with the test.
//!
//! Skipped when Docker is unavailable, and it says so rather than passing
//! quietly -- a container test that silently becomes a no-op is worse than one
//! that fails.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lcq::application::Journal;
use lcq::domain::contracts::Subject;
use lcq::domain::time::Timestamp;
use lcq::infrastructure::LogJournal;

const SCALE: &str = "20";
const EPOCH: u64 = 1_000_000;
const TARGET: &str = "x86_64-unknown-linux-musl";

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

fn uid() -> String {
    run_capturing("id", &["-u"])
}

fn gid() -> String {
    run_capturing("id", &["-g"])
}

fn run_capturing(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn docker_available() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Build the static binaries and import them as an image.
///
/// `docker import` turns a tarball into an image directly, so a statically
/// linked binary needs no base image, no package manager and no Dockerfile:
/// the whole vessel is two files and a kernel.
fn build_image(tag: &str) -> Result<(), String> {
    let built = Command::new("cargo")
        .args(["build", "--release", "--bins", "--target", TARGET])
        .status()
        .map_err(|e| e.to_string())?;
    if !built.success() {
        return Err("static build failed".to_string());
    }
    let dir = format!("target/{TARGET}/release");
    let tar = Command::new("tar")
        .args(["-C", &dir, "-cf", "-", "lcq-node", "lcq-hub"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let imported = Command::new("docker")
        .args(["import", "-", tag])
        .stdin(Stdio::from(tar.stdout.expect("tar stdout")))
        .stdout(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if imported.success() {
        Ok(())
    } else {
        Err("docker import failed".to_string())
    }
}

/// The network, the image and everything launched on it.
struct Sea {
    tag: String,
    network: String,
    scratch: PathBuf,
    ships: Vec<String>,
}

impl Sea {
    fn new(name: &str) -> Result<Self, String> {
        let unique = format!("lcq-{name}-{}", std::process::id());
        let scratch = std::env::temp_dir().join(&unique);
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

        build_image(&unique)?;
        Command::new("docker")
            .args(["network", "create", &unique])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;

        Ok(Self {
            tag: unique.clone(),
            network: unique,
            scratch,
            ships: Vec::new(),
        })
    }

    /// The channel everything shares.
    fn launch_hub(&mut self, fleet: usize) -> String {
        self.launch_hub_with(fleet, &[])
    }

    /// The channel, with extra emulator arguments such as `--loss`.
    ///
    /// Never quiet: the emulator's log is where collisions are counted, and a
    /// container test that cannot count them is not checking the schedule.
    fn launch_hub_with(&mut self, fleet: usize, extra: &[&str]) -> String {
        let name = format!("{}-hub", self.network);
        let mut command: Vec<String> = vec![
            "/lcq-hub".to_string(),
            "--bind".into(),
            "0.0.0.0".into(),
            "--port".into(),
            "9000".into(),
            "--fleet".into(),
            fleet.to_string(),
            "--scale".into(),
            SCALE.into(),
        ];
        command.extend(extra.iter().map(ToString::to_string));
        self.run(&name, &[], &command);
        name
    }

    /// How many frames the emulator destroyed by overlap so far.
    fn collisions(hub: &str) -> usize {
        Command::new("docker")
            .args(["logs", hub])
            .output()
            .map_or(usize::MAX, |o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| l.contains("\"collision\""))
                    .count()
            })
    }

    /// One vessel, with a journal on a mount only it can see.
    ///
    /// `opens_round` decides whether it is the one that puts the trigger on the
    /// air. The caller launches every other vessel first and waits for each to
    /// report, then launches this one: a round opened while containers are
    /// still starting is a round opened for nobody, and a fixed delay would be
    /// a guess about how long Docker takes today.
    fn launch_ship(&mut self, index: usize, fleet: usize, hub: &str, opens_round: bool) -> Ship {
        self.launch_ship_with(index, fleet, hub, opens_round, &[])
    }

    /// The same, with extra arguments handed to the node process.
    fn launch_ship_with(
        &mut self,
        index: usize,
        fleet: usize,
        hub: &str,
        opens_round: bool,
        extra: &[&str],
    ) -> Ship {
        let name = format!("{}-ship{index}", self.network);
        let journal_dir = self.scratch.join(format!("ship{index}"));
        std::fs::create_dir_all(&journal_dir).expect("journal dir");

        let mut command: Vec<String> = vec![
            "/lcq-node".into(),
            "--index".into(),
            index.to_string(),
            "--fleet".into(),
            fleet.to_string(),
            "--hub".into(),
            format!("{hub}:9000"),
            "--journal".into(),
            "/journal/vote.log".into(),
            "--scale".into(),
            SCALE.into(),
            "--epoch".into(),
            EPOCH.to_string(),
            "--slots".into(),
        ];
        if opens_round {
            command.push("--trigger".into());
            // Short, because the harness has already confirmed the rest of the
            // fleet is listening. This only covers this container's own start.
            command.push("--trigger-delay-ms".into());
            command.push("1500".into());
        } else {
            // Any member may open once its turn comes; a waiting member's turn
            // must not come before the designated opener has had its chance,
            // or the test has several rounds where it meant to have one.
            command.push("--trigger-delay-ms".into());
            command.push("8000".into());
        }
        command.extend(extra.iter().map(ToString::to_string));
        let mount = format!("{}:/journal", journal_dir.display());
        // As the invoking user, not root. A vessel's node has no business
        // running as root, and it also means the journal it leaves behind is
        // one the harness can open -- LogJournal opens for writing, because
        // recovering a torn tail is a write.
        let user = format!("{}:{}", uid(), gid());
        self.run(&name, &["-v", &mount, "--user", &user], &command);
        Ship {
            name,
            journal: journal_dir.join("vote.log"),
            index,
        }
    }

    fn run(&mut self, name: &str, extra: &[&str], command: &[String]) {
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.to_string(),
            "--network".into(),
            self.network.clone(),
            "--network-alias".into(),
            name.to_string(),
        ];
        args.extend(extra.iter().map(ToString::to_string));
        args.push(self.tag.clone());
        args.extend(command.iter().cloned());

        let output = Command::new("docker")
            .args(&args)
            .output()
            .expect("docker run");
        assert!(
            output.status.success(),
            "docker run {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.ships.push(name.to_string());
    }
}

impl Drop for Sea {
    fn drop(&mut self) {
        for name in &self.ships {
            let _ = Command::new("docker")
                .args(["rm", "-f", name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = Command::new("docker")
            .args(["network", "rm", &self.network])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("docker")
            .args(["rmi", "-f", &self.tag])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// One vessel.
struct Ship {
    name: String,
    journal: PathBuf,
    index: usize,
}

impl Ship {
    /// Stop it the way a vessel stops: without warning.
    fn sink(&self) {
        let _ = Command::new("docker")
            .args(["kill", "-s", "KILL", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Bring it back with the journal it left behind.
    fn refloat(&self) {
        let output = Command::new("docker")
            .args(["start", &self.name])
            .output()
            .expect("docker start");
        assert!(
            output.status.success(),
            "refloating {} failed: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn logs(&self) -> String {
        let output = Command::new("docker")
            .args(["logs", &self.name])
            .output()
            .expect("docker logs");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    }

    /// Wait for a line to appear in this vessel's log.
    fn await_log(&self, needle: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.logs().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// How many times this vessel has announced itself so far.
    fn starts(&self) -> usize {
        self.logs()
            .lines()
            .filter(|l| l.contains("\"start\""))
            .count()
    }

    /// Wait for a fresh start line and say whether it recovered a vote.
    fn await_restart(&self, already: usize, within: Duration) -> Option<bool> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let logs = self.logs();
            if logs.lines().filter(|l| l.contains("\"start\"")).count() > already {
                let line = logs
                    .lines()
                    .rev()
                    .find(|l| l.contains("\"start\""))
                    .unwrap_or_default();
                return Some(line.contains("\"recovered_vote\":true"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    /// How many times this vessel has reported so far.
    ///
    /// `docker logs` keeps everything the container ever printed, including
    /// previous lives. Waiting for "a final line" after a restart therefore
    /// finds the one from before the restart and returns instantly, which is
    /// how this harness first convinced itself a refloated vessel had not
    /// recovered its vote.
    fn reports(&self) -> usize {
        self.logs()
            .lines()
            .filter(|l| l.contains("\"final\""))
            .count()
    }

    /// Wait for this vessel to report, or give up.
    fn await_final(&self, within: Duration) -> Option<Final> {
        self.await_report(0, within)
    }

    /// Wait for a report beyond the `already` this vessel had already made.
    fn await_report(&self, already: usize, within: Duration) -> Option<Final> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let logs = self.logs();
            if logs.lines().filter(|l| l.contains("\"final\"")).count() > already {
                return last_final(&logs);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    fn voted(&self) -> bool {
        LogJournal::open(&self.journal)
            .is_ok_and(|journal| journal.has_voted(&subject(), &format!("n{}", self.index)))
    }

    fn locks(&self) -> usize {
        LogJournal::open(&self.journal).map_or(0, |journal| journal.snapshot().vote_locks.len())
    }
}

/// Put a fleet to sea with extra node arguments.
fn put_to_sea_with(sea: &mut Sea, fleet: usize, hub: &str, extra: &[&str]) -> Vec<Ship> {
    let mut ships: Vec<Ship> = (1..fleet)
        .map(|i| sea.launch_ship_with(i, fleet, hub, false, extra))
        .collect();
    for ship in &ships {
        assert!(
            ship.await_log("\"start\"", Duration::from_mins(1)),
            "jednostka {} nie wstala",
            ship.index
        );
    }
    ships.insert(0, sea.launch_ship_with(0, fleet, hub, true, extra));
    ships
}

/// Put a fleet to sea with one compromised member.
///
/// Everybody gets `common`; the member at `bad` also gets `bad_args`. Launch
/// order is the usual one -- everyone but the opener first, confirmed
/// listening, then the opener -- so the only thing different from an honest
/// run is what the compromised member does.
fn put_to_sea_with_adversary(
    sea: &mut Sea,
    fleet: usize,
    hub: &str,
    common: &[&str],
    bad: usize,
    bad_args: &[&str],
) -> Vec<Ship> {
    let args_for = |index: usize| -> Vec<&str> {
        let mut args: Vec<&str> = common.to_vec();
        if index == bad {
            args.extend_from_slice(bad_args);
        }
        args
    };
    let mut ships: Vec<Ship> = (1..fleet)
        .map(|i| sea.launch_ship_with(i, fleet, hub, false, &args_for(i)))
        .collect();
    for ship in &ships {
        assert!(
            ship.await_log("\"start\"", Duration::from_mins(1)),
            "jednostka {} nie wstala",
            ship.index
        );
    }
    ships.insert(0, sea.launch_ship_with(0, fleet, hub, true, &args_for(0)));
    ships
}

/// How many of a vessel's log lines contain `needle`.
fn count_in_log(ship: &Ship, needle: &str) -> usize {
    ship.logs().lines().filter(|l| l.contains(needle)).count()
}

/// Wait for every vessel's verdict.
fn await_all(ships: &[Ship], within: Duration) -> Vec<Final> {
    ships
        .iter()
        .map(|ship| {
            ship.await_final(within)
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect()
}

/// What a vessel concluded.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Final {
    supporters: usize,
    threshold: usize,
    endorsed: bool,
    recovered_vote: bool,
    binding_attempts: usize,
    acknowledged: bool,
}

fn last_final(logs: &str) -> Option<Final> {
    logs.lines()
        .rev()
        .find(|l| l.contains("\"final\""))
        .map(|line| Final {
            supporters: field(line, "\"supporters\":"),
            threshold: field(line, "\"threshold\":"),
            endorsed: line.contains("\"endorsed\":true"),
            recovered_vote: line.contains("\"recovered_vote\":true"),
            binding_attempts: field(line, "\"binding_attempts\":"),
            acknowledged: line.contains("\"acknowledged\":true"),
        })
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

/// Put a fleet to sea in an order that cannot race.
///
/// Every vessel but the opener is launched and confirmed listening first, so
/// the trigger reaches all of them. Returns the ships in index order.
fn put_to_sea(sea: &mut Sea, fleet: usize, hub: &str) -> Vec<Ship> {
    let mut ships: Vec<Ship> = (1..fleet)
        .map(|i| sea.launch_ship(i, fleet, hub, false))
        .collect();
    for ship in &ships {
        assert!(
            ship.await_log("\"start\"", Duration::from_mins(1)),
            "jednostka {} nie wstala",
            ship.index
        );
    }
    let opener = sea.launch_ship(0, fleet, hub, true);
    ships.insert(0, opener);
    ships
}

/// Print every line a vessel logged that says something went wrong, so a
/// failing assertion comes with the reason instead of a number.
fn note_anomalies(ships: &[Ship]) {
    for ship in ships {
        for line in ship.logs().lines().filter(|l| {
            [
                "\"refused\"",
                "slot_missed",
                "\"split\"",
                "not_sent",
                "schedule_unfit",
                "\"derived\"",
            ]
            .iter()
            .any(|needle| l.contains(needle))
        }) {
            note(&format!("    statek {}: {line}", ship.index));
        }
    }
}

fn note(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

#[test]
fn a_fleet_of_vessels_in_containers_reaches_one_verdict() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    let fleet = 5;
    let mut sea = Sea::new("fleet").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea(&mut sea, fleet, &hub);
    note(&format!("wyplynelo {fleet} jednostek, kanal: {hub}"));

    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(2))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();

    for (index, result) in finals.iter().enumerate() {
        note(&format!(
            "  statek {index}: poparc {} / prog {} -> {}",
            result.supporters,
            result.threshold,
            if result.endorsed {
                "ZATWIERDZONE"
            } else {
                "zablokowane"
            }
        ));
    }

    for (index, result) in finals.iter().enumerate() {
        assert_eq!(result.supporters, fleet, "statek {index} policzyl inaczej");
        assert!(result.endorsed, "statek {index} nie zatwierdzil");
    }
    // Each vessel wrote its own lock to a mount no other vessel can see.
    for ship in &ships {
        assert!(ship.voted(), "statek {} nie zostawil blokady", ship.index);
        assert_eq!(ship.locks(), 1);
    }
}

#[test]
fn a_vessel_that_sinks_and_is_refloated_does_not_vote_twice() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    let fleet = 5;
    let mut sea = Sea::new("refloat").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea(&mut sea, fleet, &hub);

    for ship in &ships {
        ship.await_final(Duration::from_mins(2))
            .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index));
    }
    let casualty = &ships[2];
    let locks_before = casualty.locks();
    assert_eq!(locks_before, 1, "nothing to recover");
    note(&format!(
        "statek 2 zatopiony z {locks_before} blokada w dzienniku"
    ));

    // SIGKILL to the container, then the same journal comes back up.
    let starts_before = casualty.starts();
    let reports_before = casualty.reports();
    casualty.sink();
    casualty.refloat();

    let recovered = casualty
        .await_restart(starts_before, Duration::from_mins(1))
        .expect("statek 2 nie wstal ponownie");
    note(&format!(
        "  po wydobyciu: odzyskany_glos={recovered} blokad={}",
        casualty.locks()
    ));

    assert!(
        recovered,
        "wydobyty statek nie zauwazyl wlasnego wczesniejszego glosu"
    );
    assert_eq!(
        casualty.locks(),
        locks_before,
        "restart wzial druga blokade na te sama sprawe"
    );

    // And it does not invent a verdict. The round it belonged to is over and
    // nobody has opened another, so a vessel with nothing to join waits --
    // which is the correct behaviour and the reason this test does not expect
    // a second report.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        casualty.reports(),
        reports_before,
        "wydobyty statek oglosil werdykt rundy, ktorej nie bylo"
    );
}

#[test]
fn a_vessel_lost_at_sea_does_not_stop_the_rest() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Four of five is exactly the count threshold, so this is the boundary.
    let fleet = 5;
    let mut sea = Sea::new("lost").expect("sea");
    let hub = sea.launch_hub(fleet);
    // Launched, confirmed listening, then lost before the round opens.
    let mut ships: Vec<Ship> = (1..fleet)
        .map(|i| sea.launch_ship(i, fleet, &hub, false))
        .collect();
    for ship in &ships {
        assert!(ship.await_log("\"start\"", Duration::from_mins(1)));
    }
    ships[fleet - 2].sink();
    note("statek 4 stracony przed otwarciem rundy");
    ships.insert(0, sea.launch_ship(0, fleet, &hub, true));

    let finals: Vec<Final> = ships[..fleet - 1]
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(2))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();

    for (index, result) in finals.iter().enumerate() {
        note(&format!(
            "  statek {index}: poparc {} / prog {} -> {}",
            result.supporters,
            result.threshold,
            if result.endorsed {
                "ZATWIERDZONE"
            } else {
                "zablokowane"
            }
        ));
        assert_eq!(result.threshold, 4);
        assert_eq!(result.supporters, 4, "statek {index} policzyl inaczej");
        assert!(result.endorsed);
    }
}

#[test]
fn a_larger_fleet_costs_no_more_time_than_a_small_one() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Twelve vessels rather than five. A slotted round grows linearly with the
    // fleet -- one slot each -- while the deliberation window does not grow at
    // all, so the whole thing still finishes in the same wall-clock time. That
    // is the property worth checking: adding members costs airtime, not delay.
    let fleet = 12;
    let mut sea = Sea::new("larger").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea(&mut sea, fleet, &hub);
    note(&format!("wyplynelo {fleet} jednostek"));

    let began = Instant::now();
    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(3))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    let took = began.elapsed();

    let agreed = finals.iter().filter(|f| f.endorsed).count();
    note(&format!(
        "  {agreed}/{fleet} zatwierdzilo, poparc {} / prog {}, zajelo {:.1} s",
        finals[0].supporters,
        finals[0].threshold,
        took.as_secs_f64()
    ));

    for (index, result) in finals.iter().enumerate() {
        assert_eq!(result.supporters, fleet, "statek {index} policzyl inaczej");
        assert!(result.endorsed, "statek {index} nie zatwierdzil");
    }
    for ship in &ships {
        assert_eq!(
            ship.locks(),
            1,
            "statek {} ma zla liczbe blokad",
            ship.index
        );
    }
}

#[test]
fn acknowledgement_stops_the_fleet_repeating_itself() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The measured case for carrying eight bytes of "who I heard" on frames the
    // protocol already sends. Both fleets are allowed four attempts at their
    // binding vote; one reads the acknowledgements and one is told to ignore
    // them. Airtime is what the duty cycle meters, so the attempts a member
    // spends are the number that matters (`DECISIONS.md` D4).
    let fleet = 5;

    let mut deaf_sea = Sea::new("ack-deaf").expect("sea");
    let deaf_hub = deaf_sea.launch_hub(fleet);
    let deaf = put_to_sea_with(
        &mut deaf_sea,
        fleet,
        &deaf_hub,
        &["--attempts", "4", "--ignore-acks"],
    );
    let deaf_finals: Vec<Final> = deaf
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(3))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    drop(deaf);
    drop(deaf_sea);

    let mut sea = Sea::new("ack-hearing").expect("sea");
    let hub = sea.launch_hub(fleet);
    let hearing = put_to_sea_with(&mut sea, fleet, &hub, &["--attempts", "4"]);
    let finals: Vec<Final> = hearing
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(3))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();

    let deaf_attempts: usize = deaf_finals.iter().map(|f| f.binding_attempts).sum();
    let heard_attempts: usize = finals.iter().map(|f| f.binding_attempts).sum();
    let acknowledged = finals.iter().filter(|f| f.acknowledged).count();
    note(&format!(
        "  prob glosu wiazacego: {deaf_attempts} bez potwierdzen, {heard_attempts} z potwierdzeniami ({acknowledged}/{fleet} uslyszalo potwierdzenie)"
    ));

    assert!(
        heard_attempts < deaf_attempts,
        "potwierdzenia musza zmniejszyc liczbe prob: {heard_attempts} wobec {deaf_attempts}"
    );
    // An honest fleet retransmitting must never mistake its own retries for
    // a split: the timing check used to, and a fleet that did would leave its
    // slots for random retries on every lossy round.
    note_anomalies(&hearing);
    for ship in &hearing {
        assert_eq!(
            count_in_log(ship, "\"event\":\"split\""),
            0,
            "statek {} oglosil split w uczciwej flocie",
            ship.index
        );
    }
    assert!(
        acknowledged > 0,
        "nikt nie zobaczyl wlasnego bitu w cudzej ramce"
    );
    // And it must not have cost the quorum.
    for (index, result) in finals.iter().enumerate() {
        assert_eq!(result.supporters, fleet, "statek {index} policzyl inaczej");
        assert!(result.endorsed);
    }
}

#[test]
fn a_fleet_with_skewed_clocks_still_agrees_without_colliding() {
    use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;

    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Every earlier container run gave the fleet one clock. The protocol's own
    // budget allows honest clocks to differ pairwise by MAX_CLOCK_SKEW_SECONDS,
    // and the thing that breaks under skew is not the slot schedule -- that is
    // anchored on the trigger -- but WHEN each member closes consultation, which
    // is on its own clock. The member furthest behind must still have closed by
    // its first binding slot, or it fires late into somebody else's.
    let fleet = 5;
    let half = i64::try_from(MAX_CLOCK_SKEW_SECONDS / 2).expect("fits");
    let mut sea = Sea::new("skew").expect("sea");
    let hub = sea.launch_hub(fleet);

    let mut ships: Vec<Ship> = (1..fleet)
        .map(|i| {
            // Spread from -half to +half, so the widest pair differs by the
            // whole budget. Which member gets which error is irrelevant.
            let members = i64::try_from(fleet).expect("fits");
            let offset = -half + (2 * half * i64::try_from(i).expect("fits")) / (members - 1);
            sea.launch_ship_with(i, fleet, &hub, false, &["--offset", &offset.to_string()])
        })
        .collect();
    for ship in &ships {
        assert!(ship.await_log("\"start\"", Duration::from_mins(1)));
    }
    ships.insert(
        0,
        sea.launch_ship_with(0, fleet, &hub, true, &["--offset", &(-half).to_string()]),
    );

    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(3))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    let collisions = Sea::collisions(&hub);
    note(&format!(
        "  skos parami {MAX_CLOCK_SKEW_SECONDS} s: {} zderzen, poparc {} / prog {}",
        collisions, finals[0].supporters, finals[0].threshold
    ));
    note_anomalies(&ships);

    assert_eq!(
        collisions, 0,
        "skewed clocks must not put members into each other's slots"
    );
    for (index, result) in finals.iter().enumerate() {
        assert_eq!(result.supporters, fleet, "statek {index} policzyl inaczej");
        assert!(result.endorsed, "statek {index} nie zatwierdzil");
    }
}

#[test]
fn a_lossy_channel_is_survived_by_retransmission() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Thirty per cent of deliveries dropped by the emulator, four attempts per
    // binding vote. Loss also eats the trigger: a member that missed it must
    // work out when the round began from the first vote it hears, or it never
    // joins at all.
    let fleet = 5;
    let mut sea = Sea::new("loss").expect("sea");
    let hub = sea.launch_hub_with(fleet, &["--loss", "0.3"]);
    let ships = put_to_sea_with(&mut sea, fleet, &hub, &["--attempts", "4"]);

    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(4))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    let attempts: usize = finals.iter().map(|f| f.binding_attempts).sum();
    let late = ships
        .iter()
        .filter(|s| s.logs().contains("\"by\":\"derived\""))
        .count();
    let nacks: usize = ships.iter().map(|s| count_in_log(s, "\"nack\"")).sum();
    let repairs: usize = ships.iter().map(|s| count_in_log(s, "\"repaired\"")).sum();
    note(&format!(
        "  30% strat: {attempts} prob glosu, {late} jednostek dolaczylo bez wyzwalacza, {nacks} zadan naprawy, {repairs} powtorzen na zadanie"
    ));

    assert!(attempts > fleet, "losses must have forced retransmissions");
    // An honest fleet retransmitting must never mistake its own retries for
    // a split: the timing check used to, and a fleet that did would leave its
    // slots for random retries on every lossy round.
    note_anomalies(&ships);
    for ship in &ships {
        assert_eq!(
            count_in_log(ship, "\"event\":\"split\""),
            0,
            "statek {} oglosil split w uczciwej flocie",
            ship.index
        );
    }
    for (index, result) in finals.iter().enumerate() {
        assert!(
            result.supporters >= result.threshold,
            "statek {index}: {} poparc przy progu {}",
            result.supporters,
            result.threshold
        );
        assert!(result.endorsed, "statek {index} nie zatwierdzil");
    }
}

#[test]
fn a_fleet_whose_designated_opener_is_dead_still_opens_a_round() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Nobody is told to open. Member 0 is never launched at all. The rest wait
    // their rotated turn and whoever's turn comes first opens; everyone else
    // hears it and joins. A single member that has to be alive for anything to
    // happen is a single point of failure, and this is the check that there
    // is none.
    let fleet = 5;
    let mut sea = Sea::new("noopener").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships: Vec<Ship> = (1..fleet)
        .map(|i| sea.launch_ship_with(i, fleet, &hub, false, &["--trigger-delay-ms", "4000"]))
        .collect();
    for ship in &ships {
        assert!(ship.await_log("\"start\"", Duration::from_mins(1)));
    }

    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(3))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    let openers = ships
        .iter()
        .filter(|s| s.logs().contains("\"by\":\"self\""))
        .map(|s| s.index.to_string())
        .collect::<Vec<_>>();
    note(&format!(
        "  bez wyznaczonego otwierajacego: otworzyl(y) {:?}, {} zderzen",
        openers,
        Sea::collisions(&hub)
    ));

    assert!(!openers.is_empty(), "nikt nie otworzyl rundy");
    for (slot, result) in finals.iter().enumerate() {
        assert_eq!(result.threshold, 4);
        assert!(
            result.supporters >= 4,
            "statek {}: {} poparc",
            ships[slot].index,
            result.supporters
        );
        assert!(
            result.endorsed,
            "statek {} nie zatwierdzil",
            ships[slot].index
        );
    }
}

#[test]
fn two_openings_of_one_subject_are_a_split_the_fleet_survives() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The emulator keeps the two halves of the fleet from hearing each other
    // for the first two and a half seconds, then joins them. The designated
    // opener is in one half; the other half hears nothing, waits its rotated
    // turn and opens its own round. From then on the fleet counts slots from
    // two instants about nineteen slots apart -- four slots modulo the window
    // -- so half the first binding attempts land on top of each other, and
    // would keep doing so on every in-slot retry. The fleet must notice (two
    // independent members disagreeing is the threshold), abandon in-slot
    // retries, and still reach quorum. Nothing about safety changes: votes
    // bind to the subject and the journal allows one each, whichever round
    // they were cast in.
    let fleet = 5;
    let mut sea = Sea::new("split").expect("sea");
    let hub = sea.launch_hub_with(fleet, &["--isolate-for-ms", "2500"]);
    // Default delays: the waiting members' turns come a few seconds after the
    // designated opener's frame, which is what the isolation window covers.
    let ships = put_to_sea_with(&mut sea, fleet, &hub, &["--attempts", "5"]);

    let finals: Vec<Final> = ships
        .iter()
        .map(|ship| {
            ship.await_final(Duration::from_mins(4))
                .unwrap_or_else(|| panic!("jednostka {} nie zameldowala sie", ship.index))
        })
        .collect();
    let opened = ships
        .iter()
        .filter(|s| s.logs().contains("\"by\":\"self\""))
        .map(|s| s.index.to_string())
        .collect::<Vec<_>>();
    let noticed = ships
        .iter()
        .filter(|s| s.logs().contains("\"event\":\"split\""))
        .count();
    let attempts: usize = finals.iter().map(|f| f.binding_attempts).sum();
    note(&format!(
        "  otworzyly {:?}, {noticed}/{fleet} wykrylo split, {} zderzen, {attempts} prob glosu",
        opened,
        Sea::collisions(&hub)
    ));

    note_anomalies(&ships);
    assert!(
        opened.len() >= 2,
        "bez dwoch otwarc nie ma splitu: {opened:?}"
    );
    assert!(
        noticed >= 2,
        "split musi zostac wykryty przez wiecej niz jednego czlonka"
    );
    for (index, result) in finals.iter().enumerate() {
        assert!(
            result.supporters >= result.threshold,
            "statek {index}: {} poparc przy progu {}",
            result.supporters,
            result.threshold
        );
        assert!(result.endorsed, "statek {index} nie zatwierdzil");
    }
}

// ---------------------------------------------------------------------------
// One compromised member per run. The fault budget allows two of five; one is
// enough to exercise each defence, and keeps the cause of any failure single.
// ---------------------------------------------------------------------------

#[test]
fn adversary_forger_holds_the_group_key_and_counts_for_nothing() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    let fleet = 5;
    let mut sea = Sea::new("forge").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea_with_adversary(&mut sea, fleet, &hub, &[], 4, &["--adversary", "forge"]);
    let finals = await_all(&ships, Duration::from_mins(3));
    let refusals: usize = ships[..4]
        .iter()
        .map(|s| count_in_log(s, "\"why\":\"signature\""))
        .sum();
    note(&format!(
        "  falszerz: {refusals} odmow podpisu u uczciwych, poparc {} / prog {}",
        finals[0].supporters, finals[0].threshold
    ));
    note_anomalies(&ships);

    assert!(refusals >= 4, "kazdy uczciwy musi odrzucic falszerza");
    for (index, result) in finals.iter().enumerate().take(4) {
        assert_eq!(
            result.supporters, 4,
            "statek {index}: falszerz nie moze sie liczyc"
        );
        assert!(result.endorsed, "czterech uczciwych to prog");
    }
}

#[test]
fn adversary_double_voter_is_counted_exactly_once() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The member ignores its own journal and casts a second, contradicting
    // vote. The lock is the honest member's discipline; what protects the
    // tally is every receiver's state machine refusing a second binding vote
    // from an author it already has one from.
    let fleet = 5;
    let mut sea = Sea::new("double").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea_with_adversary(
        &mut sea,
        fleet,
        &hub,
        &["--attempts", "2", "--ignore-acks"],
        4,
        &["--adversary", "double-vote"],
    );
    let finals = await_all(&ships, Duration::from_mins(3));
    let second_votes_refused: Vec<usize> = ships[..4]
        .iter()
        .map(|s| {
            count_in_log(
                s,
                "\"from\":4,\"why\":\"author already cast a binding vote\"",
            )
        })
        .collect();
    note(&format!(
        "  podwojny glos: odmowy drugiego glosu u uczciwych {second_votes_refused:?}, poparc {}",
        finals[0].supporters
    ));
    note_anomalies(&ships);

    for (index, refused) in second_votes_refused.iter().enumerate() {
        assert!(*refused >= 1, "statek {index} nie odrzucil drugiego glosu");
    }
    for (index, result) in finals.iter().enumerate().take(4) {
        assert_eq!(
            result.supporters, 5,
            "statek {index}: podwojny glos liczy sie raz"
        );
        assert!(result.endorsed);
    }
}

#[test]
fn adversary_lying_about_acknowledgements_cannot_inflate_a_tally() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // Under loss, a member that claims to have heard everyone silences honest
    // members whose votes were in fact lost -- they stop retrying. That is the
    // documented liveness cost of an advisory bitmap. What must not happen is
    // any tally counting a vote its owner never received.
    let fleet = 5;
    let mut sea = Sea::new("liar").expect("sea");
    let hub = sea.launch_hub_with(fleet, &["--loss", "0.3"]);
    let ships = put_to_sea_with_adversary(
        &mut sea,
        fleet,
        &hub,
        &["--attempts", "4"],
        4,
        &["--adversary", "lie-acks"],
    );
    let finals = await_all(&ships, Duration::from_mins(4));
    let endorsed = finals.iter().filter(|f| f.endorsed).count();
    let silenced = finals[..4]
        .iter()
        .filter(|f| f.acknowledged && f.binding_attempts == 1)
        .count();
    note(&format!(
        "  klamca w potwierdzeniach przy 30% strat: {endorsed}/{fleet} zatwierdzilo, {silenced} uczciwych przestalo powtarzac po jednej probie"
    ));
    note_anomalies(&ships);

    for (index, result) in finals.iter().enumerate() {
        assert!(result.supporters <= fleet);
        assert!(
            !result.endorsed || result.supporters >= result.threshold,
            "statek {index} zatwierdzil ponizej progu"
        );
    }
}

#[test]
fn adversary_jamming_one_slot_blocks_the_fleet_but_fabricates_nothing() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The member transmits junk in the next member's binding slot every window
    // and votes for nothing itself. Two of five members are then out -- the
    // jammed one cannot get through in its slot, the jammer abstains -- and
    // three is below the threshold of four. Blocking is the correct outcome
    // and the known liveness limit of a slot schedule (D3, D6). What matters
    // is that nobody approves.
    let fleet = 5;
    let mut sea = Sea::new("jam").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea_with_adversary(
        &mut sea,
        fleet,
        &hub,
        &["--attempts", "3"],
        4,
        &["--adversary", "jam"],
    );
    let finals = await_all(&ships, Duration::from_mins(4));
    let collisions = Sea::collisions(&hub);
    let jams = count_in_log(&ships[4], "\"jammed\"");
    note(&format!(
        "  zagluszacz: {jams} zagluszen, {collisions} zderzen, statek 0 potwierdzony={}, poparc {} / prog {}",
        finals[0].acknowledged, finals[1].supporters, finals[1].threshold
    ));
    note_anomalies(&ships);

    assert!(
        jams >= 2 && collisions >= 2,
        "zagluszanie musi zderzac ramki"
    );
    assert!(
        !finals[0].acknowledged,
        "zagluszony czlonek nie mogl zostac uslyszany"
    );
    // Four members cast a binding vote: 0, 1, 2 and 3. The jammer cast none.
    // Nobody may count more than those four, whatever it heard.
    for (index, result) in finals.iter().enumerate() {
        assert!(
            result.supporters <= 4,
            "statek {index} policzyl glos, ktorego nikt nie oddal"
        );
    }
    // The three who could hear each other but not the jammed member are one
    // short of the threshold and block. That is the known liveness limit of a
    // slot schedule under targeted jamming (D3, D6).
    for (index, result) in finals.iter().enumerate().skip(1) {
        assert!(
            !result.endorsed,
            "statek {index} zatwierdzil, slyszac trzech"
        );
        assert_eq!(
            result.supporters, 3,
            "statek {index} policzyl {}",
            result.supporters
        );
    }
    // The jammed member itself heard the other three and holds its own vote:
    // four genuine signatures, which IS a quorum. Jamming cannot make anyone
    // fabricate a vote; what it can do is leave one member holding a verdict
    // the rest of the fleet cannot yet see. Store-and-forward is the answer to
    // that, not a different tally.
    assert_eq!(finals[0].supporters, 4);
    assert!(
        finals[0].endorsed,
        "zagluszony trzyma cztery prawdziwe podpisy"
    );
}

#[test]
fn adversary_replayed_frames_are_dropped_before_verification() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    let fleet = 5;
    let mut sea = Sea::new("replay").expect("sea");
    let hub = sea.launch_hub(fleet);
    // A replay is not free: it occupies the air, and whichever binding slot it
    // lands on collides. Three attempts, so an honest member whose slot was
    // hit gets through on the next window. What is being checked is that no
    // receiver spends a signature verification on a frame it already had.
    let ships = put_to_sea_with_adversary(
        &mut sea,
        fleet,
        &hub,
        &["--attempts", "3"],
        4,
        &["--adversary", "replay"],
    );
    let finals = await_all(&ships, Duration::from_mins(4));
    let replayed = count_in_log(&ships[4], "\"replayed\"");
    let dropped: usize = ships[..4]
        .iter()
        .map(|s| count_in_log(s, "replay_dropped"))
        .sum();
    note(&format!(
        "  powtarzacz: {replayed} powtorzen, {dropped} odrzucen u uczciwych, poparc {}",
        finals[0].supporters
    ));
    note_anomalies(&ships);

    assert!(replayed >= 1, "powtarzacz nic nie powtorzyl");
    assert!(
        dropped >= replayed,
        "kazde powtorzenie musi zostac odrzucone u kazdego, kto je slyszal"
    );
    for (index, result) in finals.iter().enumerate() {
        assert!(
            result.supporters >= result.threshold,
            "statek {index}: {} poparc przy progu {}",
            result.supporters,
            result.threshold
        );
        assert!(result.endorsed);
    }
}

#[test]
fn adversary_equivocating_opener_is_evidence_not_a_split() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The opener opens the round, then opens it again a second later under a
    // fresh label. Nobody re-anchors. Every member keeps the second opening as
    // evidence -- and since one disagreeing member is below the threshold, no
    // member declares a split or leaves its slots. A lone liar changes nothing.
    let fleet = 5;
    let mut sea = Sea::new("equiv").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea_with_adversary(
        &mut sea,
        fleet,
        &hub,
        &[],
        0,
        &["--adversary", "equivocate"],
    );
    let finals = await_all(&ships, Duration::from_mins(3));
    let equivocated = count_in_log(&ships[0], "\"equivocated\"");
    let evidence = ships[1..]
        .iter()
        .filter(|s| count_in_log(s, "\"evidence\"") >= 1)
        .count();
    let splits = ships
        .iter()
        .filter(|s| count_in_log(s, "\"split\"") >= 1)
        .count();
    note(&format!(
        "  dwa wyzwalacze: {equivocated} drugie otwarcie, {evidence}/4 zachowalo dowod, {splits} oglosilo split, poparc {}",
        finals[1].supporters
    ));
    note_anomalies(&ships);

    assert_eq!(equivocated, 1);
    assert!(evidence >= 3, "dowod musi zostac zachowany");
    assert_eq!(
        splits, 0,
        "jeden klamca nie moze zdegradowac floty do losowania"
    );
    for (index, result) in finals.iter().enumerate() {
        assert_eq!(result.supporters, 5, "statek {index} policzyl inaczej");
        assert!(result.endorsed);
    }
}

#[test]
fn a_vessel_killed_inside_the_round_finishes_it_after_refloating() {
    let _serial = one_fleet_at_a_time();
    if !docker_available() {
        note("POMINIETE: docker niedostepny");
        return;
    }
    // The sharpest test of the journal. The vessel is killed after it has cast
    // its vote and heard the members before it in slot order, and brought back
    // while the round is still going. Those earlier members are done
    // transmitting; repair only asks after members heard since. If what the
    // vessel admitted before dying is not on disk, it cannot finish the round
    // it comes back into. It must: its own vote from the outbox, the votes it
    // witnessed from the journal, the rest from the air.
    let fleet = 5;
    let mut sea = Sea::new("midround").expect("sea");
    let hub = sea.launch_hub(fleet);
    let ships = put_to_sea_with(&mut sea, fleet, &hub, &["--attempts", "3"]);
    let casualty = &ships[2];

    assert!(
        casualty.await_log("\"sent\",\"stage\":\"binding\"", Duration::from_mins(2)),
        "statek 2 nie oddal glosu"
    );
    let reports_before = casualty.reports();
    let starts_before = casualty.starts();
    casualty.sink();
    casualty.refloat();

    let recovered = casualty
        .await_restart(starts_before, Duration::from_mins(1))
        .expect("statek 2 nie wstal ponownie");
    let after = casualty
        .await_report(reports_before, Duration::from_mins(4))
        .unwrap_or_else(|| {
            for line in casualty
                .logs()
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                note(&format!("    statek 2: {line}"));
            }
            panic!("statek 2 nie dokonczyl rundy, do ktorej wrocil");
        });
    let restored = casualty
        .logs()
        .lines()
        .rev()
        .find(|l| l.contains("witnessed_restored"))
        .map_or(0, |l| field(l, "\"votes\":"));
    let others = await_all(&ships[..2], Duration::from_mins(4))
        .into_iter()
        .chain(await_all(&ships[3..], Duration::from_mins(4)))
        .collect::<Vec<_>>();
    note(&format!(
        "  zabity w trakcie: odzyskany_glos={recovered}, odtworzono {restored} przyjetych glosow, po powrocie poparc {} / prog {} -> {}",
        after.supporters,
        after.threshold,
        if after.endorsed {
            "ZATWIERDZONE"
        } else {
            "zablokowane"
        }
    ));
    note_anomalies(&ships);

    if after.supporters < fleet {
        let logs = casualty.logs();
        let tail: Vec<&str> = logs.lines().rev().take(30).collect();
        for line in tail.into_iter().rev() {
            note(&format!("    statek 2: {line}"));
        }
    }
    assert!(
        recovered,
        "glos oddany przed smiercia musi zostac odzyskany"
    );
    assert!(restored >= 1, "przyjete glosy musza przezyc restart");
    assert!(after.endorsed, "wrocil do zywej rundy i musi ja dokonczyc");
    assert_eq!(after.supporters, fleet, "po powrocie ma komplet glosow");
    for result in &others {
        assert!(result.endorsed);
        assert!(result.supporters >= result.threshold);
    }
}
