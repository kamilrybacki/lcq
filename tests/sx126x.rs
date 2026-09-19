//! The virtual SX1262 under the unmodified `lora-phy` driver, with no hub:
//! the medium is a pair of channels, so what the chip does with time and mode
//! can be checked to the frame.

use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use lcq::application::RadioEvent;
use lcq::infrastructure::hub::Delivery;
use lcq::infrastructure::sx126x::{
    Chip, ChipMode, HostDelay, IRQ_CRC_ERR, IRQ_RX_DONE, VirtualIv, VirtualSpi, block_on,
};
use lora_modulation::{Bandwidth, CodingRate, SpreadingFactor};
use lora_phy::LoRa;
use lora_phy::RxMode;
use lora_phy::mod_params::{ModulationParams, PacketParams};
use lora_phy::mod_traits::IrqState;
use lora_phy::sx126x::{Config, Sx126x, Sx1262};

type Driver = LoRa<Sx126x<VirtualSpi, VirtualIv, Sx1262>, HostDelay>;

const FREQUENCY_HZ: u32 = 868_100_000;
const PREAMBLE: u16 = 8;
/// One SF10 symbol at 125 kHz.
const SYMBOL_MS: f64 = 8.192;
/// Time on air of a 32-byte SF10/125 kHz frame with CR 4/5, eight preamble
/// symbols, explicit header and CRC: 55.25 symbols of 8.192 ms.
const AIRTIME_32_BYTES: Duration = Duration::from_millis(452);

struct Bench {
    chip: Chip,
    on_air: Receiver<Vec<u8>>,
    events: Receiver<RadioEvent>,
    driver: Driver,
    modulation: ModulationParams,
    tx_params: PacketParams,
    rx_params: PacketParams,
    listening_since: Option<Instant>,
}

/// A chip on its bench: brought up by the real driver, medium in hand.
fn bench(scale: u32) -> Bench {
    let (outbound, on_air) = channel();
    let (events_in, events) = channel();
    let chip = Chip::new(scale, 6, outbound, events_in);
    let kind = Sx126x::new(
        VirtualSpi::new(chip.clone()),
        VirtualIv::new(chip.clone()),
        Config {
            chip: Sx1262,
            tcxo_ctrl: None,
            use_dcdc: false,
            rx_boost: false,
        },
    );
    let mut driver = block_on(LoRa::with_syncword(kind, 0x12, HostDelay)).expect("driver init");
    let modulation = driver
        .create_modulation_params(
            SpreadingFactor::_10,
            Bandwidth::_125KHz,
            CodingRate::_4_5,
            FREQUENCY_HZ,
        )
        .expect("modulation");
    let tx_params = driver
        .create_tx_packet_params(PREAMBLE, false, true, false, &modulation)
        .expect("tx params");
    let rx_params = driver
        .create_rx_packet_params(PREAMBLE, false, 255, true, false, &modulation)
        .expect("rx params");
    Bench {
        chip,
        on_air,
        events,
        driver,
        modulation,
        tx_params,
        rx_params,
        listening_since: None,
    }
}

impl Bench {
    fn listen(&mut self) {
        block_on(
            self.driver
                .prepare_for_rx(RxMode::Continuous, &self.modulation, &self.rx_params),
        )
        .expect("prepare rx");
        block_on(self.driver.start_rx()).expect("start rx");
        self.listening_since = Some(Instant::now());
    }

    /// Listen with a preamble of another length.
    fn listen_with_preamble(&mut self, preamble: u16) {
        self.rx_params = self
            .driver
            .create_rx_packet_params(preamble, false, 255, true, false, &self.modulation)
            .expect("rx params");
        self.listen();
    }

    /// A frame ending now that began the given number of symbols before the
    /// chip started listening. Whether it is received is the late-listener
    /// rule, and nothing else.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn deliver_started_before_listening(&mut self, symbols: f64) {
        let since = self
            .listening_since
            .expect("listening")
            .elapsed()
            .as_secs_f64()
            * 1_000.0;
        let airtime_ms = (since + symbols * SYMBOL_MS).round() as u32;
        self.chip
            .deliver(&delivery(frame(0x42, 20), airtime_ms, true));
    }

    fn transmit(&mut self, bytes: &[u8]) {
        block_on(
            self.driver
                .prepare_for_tx(&self.modulation, &mut self.tx_params, 14, bytes),
        )
        .expect("prepare tx");
        block_on(self.driver.tx()).expect("tx");
    }

    /// Service one DIO1 event the way the radio thread does, returning what
    /// the driver handed up.
    fn service(&mut self) -> Option<(Vec<u8>, i16, i16)> {
        assert!(
            self.chip.irq_pending(),
            "nothing on DIO1: {:?}",
            self.chip.snapshot()
        );
        let outcome = match block_on(self.driver.process_irq_event()).expect("irq") {
            Some(IrqState::Done) => {
                let mut buffer = [0u8; 255];
                let (length, status) =
                    block_on(self.driver.get_rx_result(&self.rx_params, &mut buffer))
                        .expect("rx result");
                Some((
                    buffer[..usize::from(length)].to_vec(),
                    status.rssi,
                    status.snr,
                ))
            }
            _ => None,
        };
        block_on(self.driver.clear_irq_status()).expect("clear");
        outcome
    }
}

fn frame(byte: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| byte.wrapping_add(u8::try_from(i).unwrap_or(0)))
        .collect()
}

fn delivery(bytes: Vec<u8>, airtime_ms: u32, crc_ok: bool) -> Delivery {
    Delivery {
        bytes,
        rssi_dbm: -90,
        snr_db: -7,
        airtime_ms,
        crc_ok,
    }
}

#[test]
fn the_driver_brings_the_chip_up_into_standby() {
    let bench = bench(1);
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.mode, ChipMode::StandbyRc);
    assert_eq!(snapshot.counters.transmitted, 0);
}

#[test]
fn a_transmission_reaches_the_medium_at_once_and_ends_after_its_airtime() {
    let mut bench = bench(1);
    let payload = frame(0x40, 32);
    let started = Instant::now();
    bench.transmit(&payload);
    let took = started.elapsed();

    let on_air = bench
        .on_air
        .recv_timeout(Duration::from_millis(10))
        .expect("the medium got the frame");
    assert_eq!(on_air, payload, "the bytes on the air are the bytes given");
    assert!(
        took >= AIRTIME_32_BYTES.mul_f64(0.9),
        "TxDone came after {took:?}, before the airtime of {AIRTIME_32_BYTES:?}"
    );
    assert!(
        took < AIRTIME_32_BYTES * 3,
        "TxDone came after {took:?}, far past the airtime of {AIRTIME_32_BYTES:?}"
    );
    let snapshot = bench.chip.snapshot();
    assert_eq!(
        snapshot.mode,
        ChipMode::StandbyRc,
        "back to standby after TxDone"
    );
    assert_eq!(snapshot.counters.transmitted, 1);
}

#[test]
fn time_scale_compresses_the_airtime() {
    let mut bench = bench(100);
    let started = Instant::now();
    bench.transmit(&frame(0x11, 32));
    let took = started.elapsed();
    assert!(
        took < AIRTIME_32_BYTES / 10,
        "at scale 100 the 452 ms frame took {took:?}"
    );
}

#[test]
fn a_listening_chip_receives_a_frame_with_its_signal_report() {
    let mut bench = bench(1);
    bench.listen();
    assert_eq!(bench.chip.snapshot().mode, ChipMode::Receive);
    // The frame started after the chip began listening: it ends now, and it
    // was short enough to have started after SetRx.
    thread::sleep(Duration::from_millis(20));
    let payload = frame(0x80, 40);
    bench.chip.deliver(&delivery(payload.clone(), 15, true));

    let (bytes, rssi, snr) = bench.service().expect("RxDone");
    assert_eq!(bytes, payload);
    assert_eq!(rssi, -90, "RssiPkt decoded the way the driver decodes it");
    assert_eq!(snr, -7, "SnrPkt decoded the way the driver decodes it");
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.counters.received, 1);
    assert_eq!(
        snapshot.mode,
        ChipMode::Receive,
        "continuous receive keeps listening"
    );
}

#[test]
fn a_chip_in_standby_hears_nothing() {
    let mut bench = bench(1);
    bench.chip.deliver(&delivery(frame(0x01, 20), 10, true));
    assert!(!bench.chip.irq_pending());
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.counters.received, 0);
    assert_eq!(snapshot.counters.missed_idle, 1);
    match bench.events.try_recv() {
        Ok(RadioEvent::Note(body)) => assert!(body.contains("\"why\":\"idle\""), "{body}"),
        other => panic!("expected a note about the missed frame, got {other:?}"),
    }
    // And once it listens, it hears.
    bench.listen();
    thread::sleep(Duration::from_millis(20));
    bench.chip.deliver(&delivery(frame(0x02, 20), 10, true));
    assert!(bench.service().is_some());
}

#[test]
fn a_frame_that_began_before_the_chip_listened_is_missed() {
    let mut bench = bench(1);
    bench.listen();
    // Ends now, but it was 900 ms on the air: it began long before SetRx and
    // its preamble is gone.
    bench.chip.deliver(&delivery(frame(0x03, 20), 900, true));
    assert!(!bench.chip.irq_pending());
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.counters.missed_late, 1);
    assert_eq!(snapshot.counters.received, 0);
}

#[test]
fn a_transmitting_chip_is_deaf() {
    let (outbound, _on_air) = channel();
    let (events_in, _events) = channel();
    let chip = Chip::new(1, 6, outbound, events_in);
    let kind = Sx126x::new(
        VirtualSpi::new(chip.clone()),
        VirtualIv::new(chip.clone()),
        Config {
            chip: Sx1262,
            tcxo_ctrl: None,
            use_dcdc: false,
            rx_boost: false,
        },
    );
    let mut driver = block_on(LoRa::with_syncword(kind, 0x12, HostDelay)).expect("driver init");
    let modulation = driver
        .create_modulation_params(
            SpreadingFactor::_10,
            Bandwidth::_125KHz,
            CodingRate::_4_5,
            FREQUENCY_HZ,
        )
        .expect("modulation");
    let mut tx_params = driver
        .create_tx_packet_params(PREAMBLE, false, true, false, &modulation)
        .expect("tx params");
    block_on(driver.prepare_for_tx(&modulation, &mut tx_params, 14, &frame(0x55, 32)))
        .expect("prepare tx");

    // Deliver from another thread while the driver is inside tx().
    let listener = chip.clone();
    let intruder = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        assert_eq!(listener.snapshot().mode, ChipMode::Transmit);
        listener.deliver(&delivery(frame(0x66, 20), 10, true));
    });
    block_on(driver.tx()).expect("tx");
    intruder.join().expect("intruder");

    let snapshot = chip.snapshot();
    assert_eq!(snapshot.counters.missed_transmitting, 1);
    assert_eq!(snapshot.counters.received, 0);
    assert_eq!(snapshot.mode, ChipMode::StandbyRc);
}

#[test]
fn a_collided_frame_arrives_as_a_crc_failure() {
    let mut bench = bench(1);
    bench.listen();
    thread::sleep(Duration::from_millis(20));
    bench.chip.deliver(&delivery(frame(0x99, 24), 10, false));

    let irq = bench.chip.snapshot().irq_status;
    assert_ne!(irq & IRQ_RX_DONE, 0, "RxDone is raised for a bad frame too");
    assert_ne!(irq & IRQ_CRC_ERR, 0, "and CrcErr says why");
    // lora-phy hands a CRC-failed frame up as a reception: the payload is
    // whatever arrived. The protocol's seal is what rejects it. This is the
    // driver's behaviour, pinned so a change upstream is noticed.
    let (bytes, _, _) = bench.service().expect("the driver reports Done");
    assert_eq!(bytes.len(), 24);
    assert_eq!(bench.chip.snapshot().counters.crc_errors, 1);
}

#[test]
fn any_command_wakes_a_sleeping_chip() {
    let mut bench = bench(1);
    block_on(bench.driver.sleep(false)).expect("sleep");
    assert_eq!(bench.chip.snapshot().mode, ChipMode::Sleep);
    bench.listen();
    assert_eq!(bench.chip.snapshot().mode, ChipMode::Receive);
}

#[test]
fn an_eight_symbol_preamble_forgives_a_listener_one_and_a_half_symbols_late() {
    let mut bench = bench(1);
    bench.listen();
    bench.deliver_started_before_listening(1.5);
    assert!(bench.service().is_some(), "{:?}", bench.chip.snapshot());
}

#[test]
fn an_eight_symbol_preamble_does_not_forgive_two_and_a_half_symbols() {
    let mut bench = bench(1);
    bench.listen();
    bench.deliver_started_before_listening(2.5);
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.counters.missed_late, 1, "{snapshot:?}");
    assert!(!bench.chip.irq_pending());
}

#[test]
fn a_twelve_symbol_preamble_forgives_five_and_a_half_symbols_but_not_six_and_a_half() {
    let mut bench = bench(1);
    bench.listen_with_preamble(12);
    bench.deliver_started_before_listening(5.5);
    assert!(bench.service().is_some(), "{:?}", bench.chip.snapshot());

    bench.deliver_started_before_listening(6.5);
    let snapshot = bench.chip.snapshot();
    assert_eq!(snapshot.counters.missed_late, 1, "{snapshot:?}");
    assert_eq!(snapshot.counters.received, 1);
}
