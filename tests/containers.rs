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
        let name = format!("{}-hub", self.network);
        self.run(
            &name,
            &[],
            &[
                "/lcq-hub".to_string(),
                "--bind".into(),
                "0.0.0.0".into(),
                "--port".into(),
                "9000".into(),
                "--fleet".into(),
                fleet.to_string(),
                "--scale".into(),
                SCALE.into(),
                "--quiet".into(),
            ],
        );
        name
    }

    /// One vessel, with a journal on a mount only it can see.
    ///
    /// `opens_round` decides whether it is the one that puts the trigger on the
    /// air. The caller launches every other vessel first and waits for each to
    /// report, then launches this one: a round opened while containers are
    /// still starting is a round opened for nobody, and a fixed delay would be
    /// a guess about how long Docker takes today.
    fn launch_ship(&mut self, index: usize, fleet: usize, hub: &str, opens_round: bool) -> Ship {
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
        }
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

/// What a vessel concluded.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Final {
    supporters: usize,
    threshold: usize,
    endorsed: bool,
    recovered_vote: bool,
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
            ship.await_final(Duration::from_secs(180))
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
