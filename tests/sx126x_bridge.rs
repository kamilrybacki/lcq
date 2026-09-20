//! The bridge adapter against the reference device on a pseudo-terminal
//! pair: the unmodified driver on this side, the protocol on the wire, the
//! virtual chip on the far side. What the firmware must match is pinned
//! here; only the wires are not.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use embedded_hal::spi::Operation;
use lcq::application::{PhyProfile, Radio, RadioEvent};
use lcq::infrastructure::hub::{Delivery, Verdict};
use lcq::infrastructure::rnode::kiss;
use lcq::infrastructure::sx126x::bridge::protocol::{
    self, BOARD_VIRTUAL, EVENT_DIO1, EVENT_ERROR, Hello, OP_READ, OP_TRANSFER, OP_WRITE, REPLY,
    STATUS_OK, STATUS_UNKNOWN_COMMAND,
};
use lcq::infrastructure::sx126x::bridge::{BridgeError, BridgeOptions, BridgeRadio, Link, Port};
use lcq::infrastructure::sx126x::bridge::{device, diagnostic};
use lcq::infrastructure::sx126x::{Chip, ChipMode};
use serial2::SerialPort;

/// Time runs this much faster on the virtual chip.
const SCALE: u32 = 100;
/// Time on air of a 32-byte SF10/125 kHz frame with CR 4/5 and an
/// eight-symbol preamble, at scale.
const AIRTIME_32_BYTES_MS: u32 = 452 / SCALE;

fn pair() -> (Arc<SerialPort>, Arc<SerialPort>) {
    let (mut host, mut dev) = SerialPort::pair().expect("a pseudo-terminal pair");
    host.set_read_timeout(Duration::from_millis(20))
        .expect("read timeout");
    dev.set_read_timeout(Duration::from_millis(20))
        .expect("read timeout");
    (Arc::new(host), Arc::new(dev))
}

/// A chip on the far end of a pty, served by the reference device. What it
/// transmits arrives on `on_air`.
struct Bench {
    host: Arc<SerialPort>,
    chip: Chip,
    on_air: Receiver<Vec<u8>>,
}

fn bench() -> Bench {
    let (host, dev) = pair();
    let (outbound, on_air) = channel();
    let (events, _) = channel();
    let chip = Chip::new(SCALE, 6, outbound, events);
    device::serve(dev, chip.clone()).expect("the device serves");
    Bench { host, chip, on_air }
}

fn options() -> BridgeOptions {
    BridgeOptions {
        tcxo: None,
        use_dcdc: false,
        rx_boost: false,
        patience: Duration::from_secs(2),
    }
}

fn wait_for<T>(mut poll: impl FnMut() -> Option<T>, patience: Duration) -> T {
    let deadline = Instant::now() + patience;
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "nothing happened in {patience:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// The next event of interest from a radio, the notes it says on the way
/// printed for the record.
fn next_event(radio: &mut BridgeRadio, patience: Duration) -> RadioEvent {
    wait_for(
        || match radio.poll() {
            Some(RadioEvent::Note(note)) => {
                println!("  note: {note}");
                None
            }
            other => other,
        },
        patience,
    )
}

#[test]
fn operations_encode_as_the_protocol_says() {
    let mut read = [0u8; 3];
    let mut in_place = [0xAA, 0xBB];
    let mut short = [0u8; 1];
    let ops = [
        Operation::Write(&[0x12]),
        Operation::Read(&mut read),
        Operation::TransferInPlace(&mut in_place),
        Operation::Transfer(&mut short, &[0x01, 0x02, 0x03]),
        Operation::DelayNs(2_500),
    ];
    let payload = protocol::encode_operations(&ops);
    assert_eq!(
        payload,
        vec![
            OP_WRITE,
            1,
            0,
            0x12, //
            OP_READ,
            3,
            0, //
            OP_TRANSFER,
            2,
            0,
            0xAA,
            0xBB, //
            OP_TRANSFER,
            3,
            0,
            0x01,
            0x02,
            0x03, //
            protocol::OP_DELAY,
            3,
            0,
        ]
    );
    let decoded = protocol::decode_operations(&payload).expect("well formed");
    assert_eq!(decoded.len(), 5);
    assert_eq!(decoded[0].bytes, &[0x12]);
    assert_eq!(decoded[1].length, 3);
    assert_eq!(decoded[3].bytes, &[0x01, 0x02, 0x03]);
    assert_eq!(decoded[4].length, 3);
    assert!(protocol::decode_operations(&[OP_WRITE, 5, 0, 1]).is_none());
    assert!(protocol::decode_operations(&[9, 0, 0]).is_none());
}

#[test]
fn hello_round_trips_and_its_version_is_readable_on_its_own() {
    let hello = Hello {
        protocol: protocol::VERSION,
        firmware: (0, 2),
        board: BOARD_VIRTUAL,
        session: 0xDEAD_BEEF,
        busy: false,
        dio1: true,
    };
    assert_eq!(Hello::parse(&hello.encode()), Some(hello));
    // A reply from another version is short, but its version still reads:
    // that is how a mismatch is told from a truncated reply.
    assert_eq!(Hello::parse(&[1, 0, 1, 1, 0, 0]), None);
    assert_eq!(Hello::version(&[1, 0, 1, 1, 0, 0]), Some(1));
    assert_eq!(Hello::version(&[]), None);
}

/// A device the test drives: it answers `HELLO` with whatever session is set
/// on it, ignores everything else, and can be told to push an event.
struct Fake {
    session: Arc<AtomicU32>,
    to_host: Mutex<VecDeque<u8>>,
}

impl Fake {
    fn new(session: &Arc<AtomicU32>) -> Arc<Self> {
        Arc::new(Self {
            session: Arc::clone(session),
            to_host: Mutex::new(VecDeque::new()),
        })
    }

    fn push(&self, command: u8, payload: &[u8]) {
        self.to_host
            .lock()
            .expect("lock")
            .extend(kiss::encode(command, payload));
    }
}

impl Port for Fake {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut out = self.to_host.lock().expect("lock");
        if out.is_empty() {
            drop(out);
            thread::sleep(Duration::from_millis(5));
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let n = out.len().min(buffer.len());
        for slot in buffer.iter_mut().take(n) {
            *slot = out.pop_front().expect("byte");
        }
        Ok(n)
    }

    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut frames = Vec::new();
        kiss::Deframer::new(1_100).push(bytes, &mut frames);
        for frame in frames {
            if frame.first() == Some(&protocol::HELLO) {
                let hello = Hello {
                    protocol: protocol::VERSION,
                    firmware: (0, 2),
                    board: BOARD_VIRTUAL,
                    session: self.session.load(Ordering::Acquire),
                    busy: false,
                    dio1: false,
                };
                self.push(protocol::HELLO | REPLY, &hello.encode());
            }
        }
        Ok(())
    }
}

#[test]
fn a_notice_that_the_line_has_fallen_is_not_an_interrupt() {
    let session = Arc::new(AtomicU32::new(1));
    let fake = Fake::new(&session);
    let link = Link::open(Arc::clone(&fake) as Arc<dyn Port>).expect("link");

    // The chip withdrew the interrupt before the notice crossed the cable.
    fake.push(EVENT_DIO1, &[0]);
    let activity = link.wait(Duration::from_millis(200));
    assert!(
        !activity.irq,
        "a notice carrying a low level must not wake the driver"
    );

    // One that still says high is a hint worth acting on.
    fake.push(EVENT_DIO1, &[1]);
    assert!(link.wait(Duration::from_millis(500)).irq);
}

#[test]
fn an_error_event_about_another_command_is_not_this_call_s_answer() {
    let session = Arc::new(AtomicU32::new(1));
    let fake = Fake::new(&session);
    let link = Link::open(Arc::clone(&fake) as Arc<dyn Port>).expect("link");
    fake.push(EVENT_ERROR, &[STATUS_UNKNOWN_COMMAND, protocol::RF]);
    // The call is for PINS, which the fake never answers; the error names RF.
    match link.call(protocol::PINS, &[], Duration::from_millis(300)) {
        Err(BridgeError::Timeout(protocol::PINS)) => {}
        other => panic!("expected the stale error to be ignored, got {other:?}"),
    }
}

#[test]
fn a_reply_said_before_the_host_arrived_is_not_its_answer() {
    let session = Arc::new(AtomicU32::new(0x3333_3333));
    let fake = Fake::new(&session);
    let link = Link::open(Arc::clone(&fake) as Arc<dyn Port>).expect("link");

    // A reply to somebody else's question, already on the wire: a host that
    // died mid-call, or a board talking to nobody.
    let stale = Hello {
        protocol: protocol::VERSION,
        firmware: (0, 2),
        board: BOARD_VIRTUAL,
        session: 0x9999_9999,
        busy: false,
        dio1: false,
    };
    fake.push(protocol::HELLO | REPLY, &stale.encode());
    thread::sleep(Duration::from_millis(60));

    let hello = link.hello(Duration::from_secs(1)).expect("a hello");
    assert_eq!(
        hello.session, 0x3333_3333,
        "the handshake must answer to this host, not to the last one"
    );
}

#[test]
fn a_device_that_restarted_poisons_the_link_instead_of_carrying_on() {
    let session = Arc::new(AtomicU32::new(0x1111_1111));
    let fake = Fake::new(&session);
    let link = Link::open(Arc::clone(&fake) as Arc<dyn Port>).expect("link");
    assert_eq!(
        link.hello(Duration::from_secs(1)).expect("a hello").session,
        0x1111_1111
    );

    // The board reboots: it answers HELLO again, with a new session.
    session.store(0x2222_2222, Ordering::Release);

    // Anything that goes unanswered now asks who is there.
    match link.dio1_level() {
        Err(BridgeError::Reset { met, now }) => {
            assert_eq!((met, now), (0x1111_1111, 0x2222_2222));
        }
        other => panic!("expected a reset, got {other:?}"),
    }
    assert!(link.poisoned());
    // And the link stays refused: the chip is not the one the driver set up.
    assert!(matches!(
        link.hello(Duration::from_secs(1)),
        Err(BridgeError::Reset { .. })
    ));
}

#[test]
fn a_device_speaking_another_protocol_version_is_named_as_such() {
    let session = Arc::new(AtomicU32::new(1));
    let fake = Fake::new(&session);
    let link = Link::open(Arc::clone(&fake) as Arc<dyn Port>).expect("link");
    // A version-1 reply: six bytes, version first.
    fake.push(protocol::HELLO | REPLY, &[1, 0, 1, BOARD_VIRTUAL, 0, 0]);
    match link.hello(Duration::from_secs(1)) {
        Err(BridgeError::Protocol(why)) => {
            assert!(why.contains("version 1"), "{why}");
        }
        other => panic!("expected a version mismatch, got {other:?}"),
    }
}

#[test]
fn the_reference_device_answers_hello_and_refuses_what_it_does_not_know() {
    let bench = bench();
    let link = Link::open(bench.host).expect("link");
    let hello = link.hello(Duration::from_secs(2)).expect("a hello");
    assert_eq!(hello.protocol, protocol::VERSION);
    assert_eq!(hello.board, BOARD_VIRTUAL);
    assert_ne!(hello.session, 0, "a device draws a session at every boot");
    assert!(!hello.dio1, "nothing is pending on a fresh chip");

    let pins = link
        .call(protocol::PINS, &[], Duration::from_secs(1))
        .expect("pins");
    assert_eq!(pins, vec![0, 0]);

    match link.call(0x42, &[], Duration::from_secs(1)) {
        Err(BridgeError::Device {
            command: 0x42,
            code,
        }) => {
            assert_eq!(code, STATUS_UNKNOWN_COMMAND);
        }
        other => panic!("expected an error event, got {other:?}"),
    }
}

#[test]
fn an_spi_transaction_reaches_the_chip_and_its_answer_comes_back() {
    let bench = bench();
    let link = Link::open(bench.host).expect("link");
    link.hello(Duration::from_secs(2)).expect("a hello");
    // GetStatus (0xC0): one status byte, read against a NOP.
    let mut answer = [0u8; 1];
    let ops = [Operation::Write(&[0xC0]), Operation::Read(&mut answer)];
    let reply = link
        .call(
            protocol::SPI,
            &protocol::encode_operations(&ops),
            Duration::from_secs(1),
        )
        .expect("spi");
    assert_eq!(reply[0], STATUS_OK);
    assert_eq!(reply.len(), 2, "one status byte read back");
    // DS.SX1261-2 13.5.1: chip mode in bits 6:4, `0x2` is standby on the RC
    // oscillator.
    assert_eq!(
        reply[1] & 0x70,
        0x20,
        "a fresh chip is in standby on the RC oscillator"
    );
}

#[test]
fn nobody_on_the_far_end_is_a_timeout() {
    let (host, _dev) = pair();
    let link = Link::open(host).expect("link");
    let started = Instant::now();
    match link.hello(Duration::from_millis(400)) {
        Err(BridgeError::Timeout(cmd)) => assert_eq!(cmd, protocol::HELLO),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_millis(400));
}

#[test]
fn the_pre_flight_answers_every_step_against_the_reference_device() {
    let bench = bench();
    let link = Link::open(bench.host).expect("link");
    let report = diagnostic::run(&link);
    for step in &report.steps {
        println!("  {:<18} {:?} {:?}", step.name, step.took, step.outcome);
    }
    assert!(
        report.passed(),
        "the pre-flight failed at {:?}",
        report.first_failure()
    );
    let named: Vec<&str> = report.steps.iter().map(|step| step.name).collect();
    assert_eq!(
        named,
        vec![
            "hello",
            "reset",
            "busy low",
            "get status",
            "get device errors",
            "get irq status",
            "dio1 reads low",
            "rf switch",
            "round trip",
        ]
    );
}

#[test]
fn the_pre_flight_says_which_rung_failed_when_nobody_answers() {
    let (host, _dev) = pair();
    let link = Link::open(host).expect("link");
    let report = diagnostic::run(&link);
    assert!(!report.passed());
    assert_eq!(
        report.first_failure().expect("a failure").name,
        "hello",
        "the lowest rung is the one to report"
    );
}

#[test]
fn a_bridged_driver_brings_the_chip_up_and_listens() {
    let bench = bench();
    let mut radio = BridgeRadio::open(bench.host, &options(), PhyProfile::eu868_sf10())
        .expect("the radio comes up");
    assert_eq!(radio.hello().board, BOARD_VIRTUAL);
    let up = wait_for(
        || match radio.poll() {
            Some(RadioEvent::Note(note)) if note.contains("chip_up") => Some(note),
            Some(RadioEvent::Note(note)) => {
                println!("  note: {note}");
                None
            }
            _ => None,
        },
        Duration::from_secs(5),
    );
    assert!(up.contains("eu868-sf10-v1"), "{up}");
    assert_eq!(bench.chip.snapshot().mode, ChipMode::Receive);
}

#[test]
fn a_bridged_chip_receives_a_frame_with_its_signal_report_and_a_crc_failure_as_such() {
    let bench = bench();
    let mut radio = BridgeRadio::open(bench.host, &options(), PhyProfile::eu868_sf10())
        .expect("the radio comes up");
    wait_for(
        || (bench.chip.snapshot().mode == ChipMode::Receive).then_some(()),
        Duration::from_secs(5),
    );
    let frame: Vec<u8> = (0..32).map(|i| i * 3).collect();
    bench.chip.deliver(&Delivery {
        bytes: frame.clone(),
        rssi_dbm: -71,
        snr_db: 6,
        airtime_ms: AIRTIME_32_BYTES_MS,
        verdict: Verdict::Decoded,
    });
    match next_event(&mut radio, Duration::from_secs(5)) {
        RadioEvent::Received(received) => {
            assert_eq!(received.bytes, frame);
            assert_eq!(received.rssi_dbm, -71);
            assert_eq!(received.snr_db, 6);
        }
        other => panic!("expected a reception, got {other:?}"),
    }

    // A frame that failed its CRC: the flags come over the wire through
    // GetIrqStatus, which the Linux bus cannot do (D21).
    bench.chip.deliver(&Delivery {
        bytes: vec![0xFF; 32],
        rssi_dbm: -90,
        snr_db: -3,
        airtime_ms: AIRTIME_32_BYTES_MS,
        verdict: Verdict::CrcError,
    });
    match next_event(&mut radio, Duration::from_secs(5)) {
        RadioEvent::CrcError { rssi_dbm } => assert_eq!(rssi_dbm, -90),
        other => panic!("expected a CRC failure, got {other:?}"),
    }
}

#[test]
fn two_bridged_radios_exchange_a_frame_over_a_medium() {
    let alice = bench();
    let bob = bench();
    let mut alice_radio = BridgeRadio::open(alice.host, &options(), PhyProfile::eu868_sf10())
        .expect("alice comes up");
    let mut bob_radio =
        BridgeRadio::open(bob.host, &options(), PhyProfile::eu868_sf10()).expect("bob comes up");
    wait_for(
        || {
            (bob.chip.snapshot().mode == ChipMode::Receive
                && alice.chip.snapshot().mode == ChipMode::Receive)
                .then_some(())
        },
        Duration::from_secs(5),
    );

    // The medium: what alice's chip puts on the air ends at bob's chip
    // after its airtime, intact.
    let bob_chip = bob.chip.clone();
    thread::spawn(move || {
        for bytes in alice.on_air {
            thread::sleep(Duration::from_millis(u64::from(AIRTIME_32_BYTES_MS)));
            bob_chip.deliver(&Delivery {
                bytes,
                rssi_dbm: -60,
                snr_db: 9,
                airtime_ms: AIRTIME_32_BYTES_MS,
                verdict: Verdict::Decoded,
            });
        }
    });

    let frame: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(7)).collect();
    alice_radio.transmit(&frame).expect("queued");
    match next_event(&mut bob_radio, Duration::from_secs(5)) {
        RadioEvent::Received(received) => {
            assert_eq!(received.bytes, frame);
            assert_eq!(received.rssi_dbm, -60);
        }
        other => panic!("expected bob to receive, got {other:?}"),
    }
    // Bob's copy ends a hair before alice's chip finishes its airtime; the
    // driver then puts her back to listening.
    wait_for(
        || (alice.chip.snapshot().mode == ChipMode::Receive).then_some(()),
        Duration::from_secs(2),
    );
    assert_eq!(alice.chip.snapshot().counters.transmitted, 1);
}
