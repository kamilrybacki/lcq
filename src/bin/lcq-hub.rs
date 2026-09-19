//! A channel emulator: one shared medium, over TCP.
//!
//! Nodes are separate processes and cannot hear each other directly, so this
//! stands in for the air between them. It is not a message bus. It enforces the
//! two things that make a radio a radio: **one frame at a time**, and **frames
//! take time**.
//!
//! A transmission occupies the channel for its airtime. Anything that starts
//! while another is in flight destroys both, which is what a collision is. Only
//! after a frame's airtime has elapsed is it handed to the other nodes.
//!
//! Usage:
//! `lcq-hub [--bind 0.0.0.0] --port 0 --fleet 10 --scale 100 [--loss 0.0]
//! [--partition] [--quiet]`

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lcq::sim::{Link, SENSITIVITY_DBM, TX_POWER_DBM, airtime_ms, capture_wins, rssi_dbm};

/// One frame arriving from a node.
struct Incoming {
    from: usize,
    bytes: Vec<u8>,
    at: Instant,
}

/// A frame holding the channel until it lands.
struct InFlight {
    from: usize,
    bytes: Vec<u8>,
    starts_at: Instant,
    ends_at: Instant,
    /// Settled: every receiver has been told, or not, and only the overlap
    /// record remains.
    done: bool,
}

/// Received power when no geometry is configured: the same for every pair, so
/// nothing can ever capture over anything and every overlap destroys both.
const NOMINAL_RSSI_DBM: f64 = -80.0;

fn main() {
    let options = Options::from_args();
    let listener = TcpListener::bind((options.bind.as_str(), options.port)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    // Printed first and flushed, so a harness can read the port before the
    // nodes it is about to start need it.
    println!("{{\"event\":\"listening\",\"port\":{port}}}");
    let _ = std::io::stdout().flush();

    let writers: Arc<Mutex<HashMap<usize, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
    let (sender, receiver) = channel::<Incoming>();

    let accepting = Arc::clone(&writers);
    thread::spawn(move || accept_nodes(&listener, &accepting, &sender));

    relay(&receiver, &writers, &options);
}

/// Take `fleet` connections, each announcing its index, and read from each.
/// Accept members for as long as the emulator runs.
///
/// Not "the first `fleet` of them": a member that restarts connects again, and
/// an emulator that stopped listening once the fleet was complete would leave
/// it with nothing to come back to. The first version did exactly that, and a
/// refloated vessel spent thirty seconds failing to connect and died -- while
/// the test watching it saw its start line, logged before the connection, and
/// passed. A reconnecting member replaces its own dead socket.
fn accept_nodes(
    listener: &TcpListener,
    writers: &Arc<Mutex<HashMap<usize, TcpStream>>>,
    sender: &Sender<Incoming>,
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = stream.try_clone().expect("clone");
        let mut index_bytes = [0u8; 2];
        if reader.read_exact(&mut index_bytes).is_err() {
            continue;
        }
        let index = usize::from(u16::from_le_bytes(index_bytes));
        writers.lock().expect("lock").insert(index, stream);

        let sender = sender.clone();
        thread::spawn(move || {
            let mut length = [0u8; 4];
            while reader.read_exact(&mut length).is_ok() {
                let size = u32::from_le_bytes(length) as usize;
                if size == 0 || size > 4096 {
                    return;
                }
                let mut bytes = vec![0u8; size];
                if reader.read_exact(&mut bytes).is_err() {
                    return;
                }
                if sender
                    .send(Incoming {
                        from: index,
                        bytes,
                        at: Instant::now(),
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

/// Hold one frame at a time, destroy overlaps, deliver what survives.
fn relay(
    receiver: &Receiver<Incoming>,
    writers: &Arc<Mutex<HashMap<usize, TcpStream>>>,
    options: &Options,
) {
    // Every frame in flight, plus frames recently ended, because a frame is
    // judged against everything that overlapped it -- and a frame that ends
    // later may have started before this one ended.
    let mut frames: Vec<InFlight> = Vec::new();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;

    loop {
        let now = Instant::now();
        let wait = frames
            .iter()
            .filter(|f| !f.done)
            .map(|f| f.ends_at.saturating_duration_since(now))
            .min()
            .unwrap_or(Duration::from_millis(250));

        match receiver.recv_timeout(wait) {
            Ok(incoming) => {
                let air = Duration::from_millis(
                    airtime_ms(incoming.bytes.len()) / u64::from(options.scale).max(1),
                );
                frames.push(InFlight {
                    from: incoming.from,
                    bytes: incoming.bytes,
                    starts_at: incoming.at,
                    ends_at: incoming.at + air,
                    done: false,
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        // Settle every frame that has finished. A frame is decided when it
        // ends, per receiver, against everything that overlapped it.
        let now = Instant::now();
        let ended: Vec<usize> = frames
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.done && f.ends_at <= now)
            .map(|(i, _)| i)
            .collect();
        for index in ended {
            settle(index, &frames, writers, options, &mut seed);
            frames[index].done = true;
        }
        // Keep settled frames a little longer than any frame can last, so a
        // frame that started during one of them still finds it.
        frames.retain(|f| !f.done || f.ends_at + Duration::from_secs(5) > now);
    }
}

/// Decide a finished frame's fate at every receiver, and deliver where it won.
///
/// Three ways to lose it. Too weak: the receiver is beyond range. Collided:
/// another frame overlapped it in time and this one was not enough stronger
/// to be demodulated over it -- the capture effect, which needs a difference
/// in received power and so never happens without geometry. Lost: the
/// configured residual loss. Each is per receiver, because reception is.
fn settle(
    index: usize,
    frames: &[InFlight],
    writers: &Arc<Mutex<HashMap<usize, TcpStream>>>,
    options: &Options,
    seed: &mut u64,
) {
    let frame = &frames[index];
    let overlappers: Vec<&InFlight> = frames
        .iter()
        .enumerate()
        .filter(|(i, o)| *i != index && o.starts_at < frame.ends_at && frame.starts_at < o.ends_at)
        .map(|(_, o)| o)
        .collect();

    let mut guard = writers.lock().expect("lock");
    let targets: Vec<usize> = guard.keys().copied().collect();
    let mut delivered_to = 0usize;
    for target in targets {
        if target == frame.from || !options.reaches(frame.from, target) {
            continue;
        }
        let power = options.rssi(frame.from, target);
        if power < SENSITIVITY_DBM {
            options.log(&format!(
                "{{\"event\":\"lost\",\"from\":{},\"to\":{target},\"why\":\"weak\",\"rssi\":{power:.1}}}",
                frame.from
            ));
            continue;
        }
        if let Some(other) = overlappers.iter().find(|o| {
            options.reaches(o.from, target) && !capture_wins(power, options.rssi(o.from, target))
        }) {
            options.log(&format!(
                "{{\"event\":\"collision\",\"a\":{},\"b\":{},\"at\":{target}}}",
                frame.from, other.from
            ));
            continue;
        }
        if options.loss > 0.0 && next_f64(seed) < options.loss {
            options.log(&format!(
                "{{\"event\":\"lost\",\"from\":{},\"to\":{target},\"why\":\"loss\"}}",
                frame.from
            ));
            continue;
        }
        if let Some(stream) = guard.get_mut(&target) {
            let length = u32::try_from(frame.bytes.len()).unwrap_or(0).to_le_bytes();
            if stream.write_all(&length).is_err() || stream.write_all(&frame.bytes).is_err() {
                continue;
            }
            let _ = stream.flush();
            delivered_to += 1;
        }
    }
    if delivered_to > 0 {
        options.log(&format!(
            "{{\"event\":\"delivered\",\"from\":{},\"bytes\":{},\"to\":{delivered_to}}}",
            frame.from,
            frame.bytes.len()
        ));
    }
}

fn next_f64(seed: &mut u64) -> f64 {
    *seed ^= *seed >> 12;
    *seed ^= *seed << 25;
    *seed ^= *seed >> 27;
    let value = seed.wrapping_mul(2_685_821_657_736_338_717);
    #[allow(clippy::cast_precision_loss)]
    {
        (value >> 11) as f64 / 9_007_199_254_740_992.0
    }
}

/// How the emulator was configured.
struct Options {
    bind: String,
    port: u16,
    fleet: usize,
    scale: u32,
    loss: f64,
    partition: bool,
    quiet: bool,
    /// Milliseconds after the FIRST frame during which the halves cannot hear
    /// each other. Then the channel is whole. This is how a fleet ends up with
    /// two openings for one subject: the half that could not hear the opening
    /// opens its own, and by the time the halves hear each other both are
    /// counting slots from different instants. Measured from the first frame
    /// rather than from start so it covers the opening whenever it happens.
    isolate_ms: u64,
    first_frame_at: std::sync::Mutex<Option<Instant>>,
    /// Members strung out in a line this far apart, in metres. Turns on the
    /// two-ray path-loss model from `lcq::sim::phy`: far members fall below
    /// sensitivity, and a near member can be demodulated over a distant one.
    /// Without it every pair is equally loud and there is no geometry to
    /// speak of.
    spacing_m: Option<f64>,
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
            // Loopback by default, because a hub reachable from the network is
            // a hub anyone can inject frames into.
            bind: value("--bind").unwrap_or_else(|| "127.0.0.1".to_string()),
            port: value("--port").and_then(|v| v.parse().ok()).unwrap_or(0),
            fleet: value("--fleet").and_then(|v| v.parse().ok()).unwrap_or(5),
            scale: value("--scale").and_then(|v| v.parse().ok()).unwrap_or(100),
            loss: value("--loss").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            partition: args.iter().any(|a| a == "--partition"),
            quiet: args.iter().any(|a| a == "--quiet"),
            isolate_ms: value("--isolate-for-ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            first_frame_at: std::sync::Mutex::new(None),
            spacing_m: value("--spacing-m").and_then(|v| v.parse().ok()),
        }
    }

    /// Received power at `to` of a frame from `from`, in dBm.
    fn rssi(&self, from: usize, to: usize) -> f64 {
        match self.spacing_m {
            None => NOMINAL_RSSI_DBM,
            Some(spacing) => {
                #[allow(clippy::cast_precision_loss)]
                let distance = spacing * (from.abs_diff(to) as f64);
                rssi_dbm(&Link::new(distance), TX_POWER_DBM)
            }
        }
    }

    fn reaches(&self, from: usize, to: usize) -> bool {
        let same_half = (from < self.fleet / 2) == (to < self.fleet / 2);
        let mut first = self.first_frame_at.lock().expect("lock");
        let since_first = first.get_or_insert_with(Instant::now).elapsed();
        let isolated = since_first < Duration::from_millis(self.isolate_ms);
        (!self.partition && !isolated) || same_half
    }

    fn log(&self, line: &str) {
        if !self.quiet {
            println!("{line}");
            let _ = std::io::stdout().flush();
        }
    }
}
