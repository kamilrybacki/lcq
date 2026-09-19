//! The `RNode` adapter against a fake `RNode` on the other end of a pseudo-terminal
//! pair: the same bytes a real one sends, without the hardware.

use std::io;
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use lcq::application::{PhyProfile, Radio, RadioError, RadioEvent};
use lcq::infrastructure::rnode::{
    DETECT_REQUEST, DETECT_RESPONSE, Port, RNodeLink, RNodeRadio, command, kiss,
};
use serial2::SerialPort;

/// A serial port as the adapter wants it: short read timeouts, whole writes.
struct Tty(SerialPort);

impl Port for Tty {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }
}

/// The device end: answers like `RNode` firmware 1.86 does on the wire.
fn fake_rnode(port: &SerialPort, radio_on: bool, transmitted: &Sender<Vec<u8>>) {
    let mut deframer = kiss::Deframer::new(600);
    let mut buffer = [0u8; 512];
    loop {
        let n = match port.read(&mut buffer) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
            Err(_) => return,
        };
        let mut frames = Vec::new();
        deframer.push(&buffer[..n], &mut frames);
        for frame in frames {
            let Some((&cmd, payload)) = frame.split_first() else {
                continue;
            };
            let reply = match cmd {
                command::DETECT if payload == [DETECT_REQUEST] => {
                    kiss::encode(command::DETECT, &[DETECT_RESPONSE])
                }
                command::FW_VERSION => kiss::encode(command::FW_VERSION, &[1, 86]),
                command::PLATFORM => kiss::encode(command::PLATFORM, &[0x80]),
                command::MCU => kiss::encode(command::MCU, &[0x81]),
                command::RADIO_STATE => kiss::encode(
                    command::RADIO_STATE,
                    &[u8::from(radio_on && payload == [1])],
                ),
                command::DATA => {
                    let _ = transmitted.send(payload.to_vec());
                    kiss::encode(command::READY, &[])
                }
                // Configuration is echoed back as confirmation.
                other => kiss::encode(other, payload),
            };
            if port.write_all(&reply).is_err() {
                return;
            }
        }
    }
}

fn pair() -> (SerialPort, SerialPort) {
    let (mut host, mut device) = SerialPort::pair().expect("a pseudo-terminal pair");
    host.set_read_timeout(Duration::from_millis(20))
        .expect("timeout");
    device
        .set_read_timeout(Duration::from_millis(20))
        .expect("timeout");
    (host, device)
}

#[test]
fn the_host_protocol_brings_a_device_up_from_the_captured_sequence() {
    let mut link = RNodeLink::new(PhyProfile::eu868_sf10());
    link.start();
    let out = link.take_outbound();
    // Detect first, then probes, then the profile, then the radio on.
    assert!(out.starts_with(&kiss::encode(command::DETECT, &[DETECT_REQUEST])));
    assert!(out.ends_with(&kiss::encode(command::RADIO_STATE, &[1])));
    let frequency = kiss::encode(command::FREQUENCY, &868_100_000u32.to_be_bytes());
    assert!(
        out.windows(frequency.len())
            .any(|w| w == frequency.as_slice())
    );
    assert!(!link.online());
    link.on_serial(&kiss::encode(command::DETECT, &[DETECT_RESPONSE]));
    link.on_serial(&kiss::encode(command::FW_VERSION, &[1, 86]));
    link.on_serial(&kiss::encode(command::RADIO_STATE, &[1]));
    assert!(link.detected());
    assert_eq!(link.firmware(), Some((1, 86)));
    assert!(link.online());
}

#[test]
fn a_received_packet_arrives_with_its_signal_report() {
    let mut link = RNodeLink::new(PhyProfile::eu868_sf10());
    link.on_serial(&kiss::encode(command::RADIO_STATE, &[1]));
    let mut bytes = kiss::encode(command::STAT_RSSI, &[57]);
    bytes.extend(kiss::encode(command::STAT_SNR, &[0xE4]));
    bytes.extend(kiss::encode(command::DATA, &[0xC0, 0xDB, 1, 2, 3]));
    // Chopped up any which way.
    for chunk in bytes.chunks(3) {
        link.on_serial(chunk);
    }
    let _ = link.poll(); // the radio-online note
    match link.poll() {
        Some(RadioEvent::Received(received)) => {
            assert_eq!(received.bytes, vec![0xC0, 0xDB, 1, 2, 3], "escapes undone");
            assert_eq!(received.rssi_dbm, 57 - 157);
            assert_eq!(received.snr_db, -7);
        }
        other => panic!("expected a reception, got {other:?}"),
    }
}

#[test]
fn transmitting_needs_the_radio_on_and_fits_one_packet() {
    let mut link = RNodeLink::new(PhyProfile::eu868_sf10());
    assert_eq!(link.transmit(&[1, 2, 3]), Err(RadioError::Offline));
    link.on_serial(&kiss::encode(command::RADIO_STATE, &[1]));
    assert!(matches!(
        link.transmit(&[0; 300]),
        Err(RadioError::TooLong { .. })
    ));
    assert!(link.transmit(&[9, 8, 7]).is_ok());
    assert_eq!(
        link.take_outbound(),
        kiss::encode(command::DATA, &[9, 8, 7])
    );
}

#[test]
fn an_rnode_on_a_pseudo_terminal_comes_up_and_moves_packets_both_ways() {
    let (host, device) = pair();
    let (transmitted, on_air) = channel();
    let device_writer = device.try_clone().expect("clone");
    thread::spawn(move || fake_rnode(&device, true, &transmitted));

    let mut radio =
        RNodeRadio::open(Tty(host), PhyProfile::eu868_sf10()).expect("the RNode comes up");
    radio.transmit(&[0xAA, 0xBB, 0xCC]).expect("transmit");
    let sent = on_air
        .recv_timeout(Duration::from_secs(2))
        .expect("the device got the packet");
    assert_eq!(sent, vec![0xAA, 0xBB, 0xCC]);

    // The device hears a packet: RSSI, SNR, bytes.
    let mut inbound = kiss::encode(command::STAT_RSSI, &[67]);
    inbound.extend(kiss::encode(command::STAT_SNR, &[0x14]));
    inbound.extend(kiss::encode(command::DATA, &[1, 2, 3, 4]));
    device_writer.write_all(&inbound).expect("device writes");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match radio.poll() {
            Some(RadioEvent::Received(received)) => {
                assert_eq!(received.bytes, vec![1, 2, 3, 4]);
                assert_eq!(received.rssi_dbm, -90);
                assert_eq!(received.snr_db, 5);
                break;
            }
            Some(_) => {}
            None => {
                assert!(Instant::now() < deadline, "nothing received");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn a_device_that_keeps_its_radio_off_is_reported_not_guessed() {
    let (host, device) = pair();
    let (transmitted, _on_air) = channel();
    thread::spawn(move || fake_rnode(&device, false, &transmitted));
    let outcome = RNodeRadio::open(Tty(host), PhyProfile::eu868_sf10());
    assert!(
        matches!(
            outcome,
            Err(lcq::infrastructure::rnode::RNodeError::RadioOff)
        ),
        "an unverified firmware refuses to turn its radio on, and says so"
    );
}
