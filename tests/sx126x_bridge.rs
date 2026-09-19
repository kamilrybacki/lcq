//! The bridge adapter against the reference device on a pseudo-terminal
//! pair: the unmodified driver on this side, the protocol on the wire, the
//! virtual chip on the far side. What the firmware must match is pinned
//! here; only the wires are not.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use embedded_hal::spi::Operation;
use lcq::application::{PhyProfile, Radio, RadioEvent};
use lcq::infrastructure::hub::{Delivery, Verdict};
use lcq::infrastructure::sx126x::bridge::device;
use lcq::infrastructure::sx126x::bridge::protocol::{
    self, BOARD_VIRTUAL, Hello, OP_READ, OP_TRANSFER, OP_WRITE, STATUS_OK, STATUS_UNKNOWN_COMMAND,
};
use lcq::infrastructure::sx126x::bridge::{BridgeError, BridgeOptions, BridgeRadio, Link};
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
fn hello_round_trips() {
    let hello = Hello {
        protocol: 1,
        firmware: (0, 1),
        board: BOARD_VIRTUAL,
        busy: false,
        dio1: true,
    };
    assert_eq!(Hello::parse(&hello.encode()), Some(hello));
    assert_eq!(Hello::parse(&[1, 0]), None);
}

#[test]
fn the_reference_device_answers_hello_and_refuses_what_it_does_not_know() {
    let bench = bench();
    let link = Link::open(bench.host).expect("link");
    let hello = link.hello(Duration::from_secs(2)).expect("a hello");
    assert_eq!(hello.protocol, protocol::VERSION);
    assert_eq!(hello.board, BOARD_VIRTUAL);
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
