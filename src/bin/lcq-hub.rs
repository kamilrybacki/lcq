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
//! `lcq-hub --port 0 --fleet 10 --scale 100 [--loss 0.0] [--partition] [--quiet]`

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lcq::sim::airtime_ms;

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
    ends_at: Instant,
}

fn main() {
    let options = Options::from_args();
    let listener = TcpListener::bind(("127.0.0.1", options.port)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    // Printed first and flushed, so a harness can read the port before the
    // nodes it is about to start need it.
    println!("{{\"event\":\"listening\",\"port\":{port}}}");
    let _ = std::io::stdout().flush();

    let writers: Arc<Mutex<HashMap<usize, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
    let (sender, receiver) = channel::<Incoming>();

    let accepting = Arc::clone(&writers);
    let fleet = options.fleet;
    thread::spawn(move || accept_nodes(&listener, fleet, &accepting, &sender));

    relay(&receiver, &writers, &options);
}

/// Take `fleet` connections, each announcing its index, and read from each.
fn accept_nodes(
    listener: &TcpListener,
    fleet: usize,
    writers: &Arc<Mutex<HashMap<usize, TcpStream>>>,
    sender: &Sender<Incoming>,
) {
    for _ in 0..fleet {
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
    let mut in_flight: Option<InFlight> = None;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;

    loop {
        // Wait only as long as the frame in flight still has to run.
        let wait = in_flight.as_ref().map_or(Duration::from_millis(250), |f| {
            f.ends_at.saturating_duration_since(Instant::now())
        });

        match receiver.recv_timeout(wait) {
            Ok(incoming) => {
                let air = Duration::from_millis(
                    airtime_ms(incoming.bytes.len()) / u64::from(options.scale).max(1),
                );
                let ends_at = incoming.at + air;
                if let Some(current) = in_flight.take() {
                    if current.ends_at > incoming.at {
                        // Overlap. Neither survives: without a difference in
                        // received strength there is nothing to capture with,
                        // and two processes on one loopback have none.
                        options.log(&format!(
                            "{{\"event\":\"collision\",\"a\":{},\"b\":{}}}",
                            current.from, incoming.from
                        ));
                        in_flight = Some(InFlight {
                            from: usize::MAX,
                            bytes: Vec::new(),
                            ends_at: ends_at.max(current.ends_at),
                        });
                        continue;
                    }
                    deliver(&current, writers, options, &mut seed);
                }
                in_flight = Some(InFlight {
                    from: incoming.from,
                    bytes: incoming.bytes,
                    ends_at,
                });
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(current) = in_flight.take() {
                    deliver(&current, writers, options, &mut seed);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Hand a finished frame to everyone else who can hear it.
fn deliver(
    frame: &InFlight,
    writers: &Arc<Mutex<HashMap<usize, TcpStream>>>,
    options: &Options,
    seed: &mut u64,
) {
    if frame.from == usize::MAX {
        // The wreckage of a collision. Nothing to deliver.
        return;
    }
    let mut guard = writers.lock().expect("lock");
    let targets: Vec<usize> = guard.keys().copied().collect();
    for target in targets {
        if target == frame.from || !options.reaches(frame.from, target) {
            continue;
        }
        if options.loss > 0.0 && next_f64(seed) < options.loss {
            options.log(&format!(
                "{{\"event\":\"lost\",\"from\":{},\"to\":{target}}}",
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
        }
    }
    options.log(&format!(
        "{{\"event\":\"delivered\",\"from\":{},\"bytes\":{}}}",
        frame.from,
        frame.bytes.len()
    ));
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
    port: u16,
    fleet: usize,
    scale: u32,
    loss: f64,
    partition: bool,
    quiet: bool,
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
            port: value("--port").and_then(|v| v.parse().ok()).unwrap_or(0),
            fleet: value("--fleet").and_then(|v| v.parse().ok()).unwrap_or(5),
            scale: value("--scale").and_then(|v| v.parse().ok()).unwrap_or(100),
            loss: value("--loss").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            partition: args.iter().any(|a| a == "--partition"),
            quiet: args.iter().any(|a| a == "--quiet"),
        }
    }

    fn reaches(&self, from: usize, to: usize) -> bool {
        !self.partition || (from < self.fleet / 2) == (to < self.fleet / 2)
    }

    fn log(&self, line: &str) {
        if !self.quiet {
            println!("{line}");
            let _ = std::io::stdout().flush();
        }
    }
}
