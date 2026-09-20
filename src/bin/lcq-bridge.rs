//! The bridge pre-flight: ask a board what it is, before a protocol is put
//! on top of it.
//!
//! When a node cannot bring a radio up, the log says `chip_failed` and
//! nothing more: the fault could be the bridge firmware, the USB link, the
//! pin map, the module's power, the `lora-phy` driver or LCQ itself. This
//! walks up that stack one rung at a time and stops being ambiguous. Run it
//! on a new board before anything else (`docs/HARDWARE-BRINGUP.md`).
//!
//! ```sh
//! lcq-bridge --port /dev/ttyACM0            # the bridge alone
//! lcq-bridge --port /dev/ttyACM0 --radio    # and then bring the chip up
//! ```

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lcq::application::{PhyProfile, Radio, RadioEvent};
use lcq::infrastructure::sx126x::bridge::{
    BridgeOptions, BridgeRadio, Link, Port, diagnostic, serial_port,
};

/// How long to wait for the driver to report that the chip is up.
const BRING_UP: Duration = Duration::from_secs(20);

struct Options {
    port: String,
    radio: bool,
    tcxo_millivolts: Option<u32>,
    use_dcdc: bool,
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    Options {
        port: value("--port").unwrap_or_else(|| "/dev/ttyACM0".to_string()),
        radio: flag("--radio"),
        tcxo_millivolts: match value("--tcxo").as_deref() {
            Some("none") => None,
            Some(volts) => Some(millivolts(volts)),
            None => Some(1_800),
        },
        use_dcdc: !flag("--ldo"),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn millivolts(volts: &str) -> u32 {
    let volts: f64 = volts
        .parse()
        .unwrap_or_else(|_| panic!("tcxo voltage {volts:?} is not a number"));
    (volts.clamp(0.0, 5.0) * 1_000.0).round() as u32
}

fn main() -> ExitCode {
    let options = options();
    let port: Arc<dyn Port> = match serial_port(&options.port) {
        Ok(port) => Arc::new(port),
        Err(error) => {
            eprintln!("{}: {error}", options.port);
            return ExitCode::from(2);
        }
    };

    println!("bridge on {}", options.port);
    let report = {
        let link = match Link::open(Arc::clone(&port)) {
            Ok(link) => link,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        diagnostic::run(&link)
    };
    for step in &report.steps {
        let (mark, said) = match &step.outcome {
            Ok(said) => ("ok  ", said.as_str()),
            Err(why) => ("FAIL", why.as_str()),
        };
        println!("  {mark} {:<18} {:>9.1?}  {said}", step.name, step.took);
    }
    if let Some(failed) = report.first_failure() {
        println!("\nthe bridge did not answer `{}`.", failed.name);
        println!("Nothing above this rung can be trusted; see docs/HARDWARE-BRINGUP.md.");
        return ExitCode::FAILURE;
    }
    println!("\nthe bridge answers everything asked of it.");
    if !options.radio {
        return ExitCode::SUCCESS;
    }

    println!("\nbringing the chip up under the driver");
    let settings = BridgeOptions {
        tcxo: options
            .tcxo_millivolts
            .map(lcq::infrastructure::sx126x::tcxo_control),
        use_dcdc: options.use_dcdc,
        ..BridgeOptions::default()
    };
    let mut radio = match BridgeRadio::open(port, &settings, PhyProfile::eu868_sf10()) {
        Ok(radio) => radio,
        Err(error) => {
            eprintln!("  FAIL {error}");
            return ExitCode::FAILURE;
        }
    };
    let deadline = Instant::now() + BRING_UP;
    while Instant::now() < deadline {
        match radio.poll() {
            Some(RadioEvent::Note(note)) if note.contains("chip_up") => {
                println!("  ok   {note}");
                println!("\nthe radio is listening. This board is ready for a node.");
                return ExitCode::SUCCESS;
            }
            Some(RadioEvent::Note(note)) if note.contains("chip_failed") => {
                println!("  FAIL {note}");
                println!(
                    "\nThe bridge is sound and the driver is not: suspect the module's power\n\
                     (try --ldo), its oscillator (try --tcxo none) or the pin map."
                );
                return ExitCode::FAILURE;
            }
            Some(RadioEvent::Note(note)) => println!("  ..   {note}"),
            Some(_) | None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    println!("\nthe driver never said the chip was up.");
    ExitCode::FAILURE
}
