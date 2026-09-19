//! The behavioural model of an `SX126x`, with a clock.
//!
//! What is modelled: the command set the `LoRa` driver uses, the 256-byte data
//! buffer, the IRQ status register behind its mask and its DIO1 routing, the
//! chip modes, BUSY after a command, and time -- a transmission lasts its
//! airtime and ends in `TxDone`, a receive timeout runs, and a frame is
//! received only when the chip was in receive mode for the whole of it. What
//! is not: the register map beyond storage (writes are kept and read back,
//! nothing acts on them), calibration, the front end, and anything the
//! datasheet leaves to measurement.
//!
//! Frames come from the medium as [`Delivery`] values at the moment they end,
//! which is when a chip raises `RxDone` too. Frames leave through a channel to
//! whatever carries them at the moment `SetTx` executes; the medium models
//! their airtime from there, and so does the chip, independently, for its own
//! `TxDone`.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use lora_modulation::{Bandwidth, BaseBandModulationParams, CodingRate, SpreadingFactor};

use crate::application::RadioEvent;
use crate::infrastructure::hub::Delivery;

// The opcodes the model acts on or answers. DS.SX1261-2 table 11-1; the values
// are the datasheet's, not the driver's, so the model does not lean on the
// crate it is meant to exercise. Anything else is accepted without effect.
const OP_WRITE_REGISTER: u8 = 0x0D;
const OP_READ_REGISTER: u8 = 0x1D;
const OP_WRITE_BUFFER: u8 = 0x0E;
const OP_READ_BUFFER: u8 = 0x1E;
const OP_SET_SLEEP: u8 = 0x84;
const OP_SET_STANDBY: u8 = 0x80;
const OP_SET_FS: u8 = 0xC1;
const OP_SET_TX: u8 = 0x83;
const OP_SET_RX: u8 = 0x82;
const OP_SET_RX_DUTY_CYCLE: u8 = 0x94;
const OP_SET_CAD: u8 = 0xC5;
const OP_GET_PACKET_TYPE: u8 = 0x11;
const OP_SET_RF_FREQUENCY: u8 = 0x86;
const OP_SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
const OP_SET_MODULATION_PARAMS: u8 = 0x8B;
const OP_SET_PACKET_PARAMS: u8 = 0x8C;
const OP_GET_RX_BUFFER_STATUS: u8 = 0x13;
const OP_GET_PACKET_STATUS: u8 = 0x14;
const OP_GET_RSSI_INST: u8 = 0x15;
const OP_GET_STATS: u8 = 0x10;
const OP_CFG_DIO_IRQ: u8 = 0x08;
const OP_GET_IRQ_STATUS: u8 = 0x12;
const OP_CLR_IRQ_STATUS: u8 = 0x02;
const OP_GET_STATUS: u8 = 0xC0;
const OP_GET_DEVICE_ERRORS: u8 = 0x17;
const OP_CLEAR_DEVICE_ERRORS: u8 = 0x07;

/// `TxDone`: the frame has left.
pub const IRQ_TX_DONE: u16 = 0x0001;
/// `RxDone`: a frame has arrived, intact or not.
pub const IRQ_RX_DONE: u16 = 0x0002;
const IRQ_PREAMBLE_DETECTED: u16 = 0x0004;
const IRQ_SYNCWORD_VALID: u16 = 0x0008;
const IRQ_HEADER_VALID: u16 = 0x0010;
/// `CrcErr`: the frame that arrived failed its CRC.
pub const IRQ_CRC_ERR: u16 = 0x0040;
/// `CadDone`: channel activity detection finished.
pub const IRQ_CAD_DONE: u16 = 0x0080;
/// `Timeout`: a timed receive or transmit ran out.
pub const IRQ_TIMEOUT: u16 = 0x0200;

/// The packet type byte for `LoRa`, `SetPacketType(0x01)`.
const PACKET_TYPE_LORA: u8 = 0x01;
/// The `SetRx` timeout that means "until told otherwise".
const RX_CONTINUOUS_TICKS: u32 = 0x00FF_FFFF;
/// One `SetRx` timeout tick.
const TICK: Duration = Duration::from_nanos(15_625);
/// How long BUSY stays high after a command: the order of what the datasheet
/// gives for most commands, unscaled because it is the chip's own time.
const BUSY: Duration = Duration::from_micros(100);
/// The least a receiver may have missed of a preamble and still lock on, as
/// wall time. Two symbols is the physical rule; the floor covers scheduling
/// jitter when a test compresses time a hundredfold.
const LATE_TOLERANCE_FLOOR: Duration = Duration::from_millis(5);
/// What the instantaneous RSSI reports when nothing is on the air.
const IDLE_RSSI_DBM: i16 = -117;
/// The data buffer.
const BUFFER_BYTES: usize = 256;

/// Where the chip is, as its status byte reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipMode {
    /// Asleep; any command wakes it into `StandbyRc`.
    Sleep,
    /// Standby on the RC oscillator, where every command sequence starts.
    StandbyRc,
    /// Standby on the crystal.
    StandbyXosc,
    /// Frequency synthesis.
    FrequencySynthesis,
    /// Transmitting: deaf until `TxDone`.
    Transmit,
    /// Receiving.
    Receive,
}

impl ChipMode {
    /// Bits 6:4 of the status byte, DS.SX1261-2 table 13-76.
    const fn status_bits(self) -> u8 {
        match self {
            Self::Sleep => 0x00,
            Self::StandbyRc => 0x20,
            Self::StandbyXosc => 0x30,
            Self::FrequencySynthesis => 0x40,
            Self::Receive => 0x50,
            Self::Transmit => 0x60,
        }
    }
}

/// What the model has counted, for whoever wants to check it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames handed to the medium.
    pub transmitted: u32,
    /// Frames received intact.
    pub received: u32,
    /// Frames received with a CRC failure.
    pub crc_errors: u32,
    /// Frames that ended while the chip was transmitting.
    pub missed_transmitting: u32,
    /// Frames that ended while the chip was in standby or asleep.
    pub missed_idle: u32,
    /// Frames the chip started listening to after their preamble had passed.
    pub missed_late: u32,
    /// Timed receives that ran out.
    pub timeouts: u32,
}

/// A view of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipSnapshot {
    /// Current mode.
    pub mode: ChipMode,
    /// The IRQ status register.
    pub irq_status: u16,
    /// What has been counted so far.
    pub counters: Counters,
}

/// What woke a waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activity {
    /// An IRQ routed to DIO1 is pending.
    pub irq: bool,
    /// The host asked for attention.
    pub host: bool,
}

#[derive(Debug, Clone, Copy)]
struct Packet {
    preamble_symbols: u16,
    implicit_header: bool,
    payload_length: u8,
}

#[derive(Debug, Clone, Copy)]
enum Timer {
    TxDone { after: Duration, generation: u64 },
    RxTimeout { after: Duration, generation: u64 },
}

struct Model {
    mode: ChipMode,
    registers: HashMap<u16, u8>,
    buffer: [u8; BUFFER_BYTES],
    tx_base: u8,
    rx_base: u8,
    modulation: Option<BaseBandModulationParams>,
    packet: Packet,
    frequency_raw: u32,
    irq_status: u16,
    irq_mask: u16,
    dio1_mask: u16,
    rx_len: u8,
    rx_offset: u8,
    rssi_raw: u8,
    snr_raw: u8,
    /// When the current receive began; the continuity rule compares it with a
    /// frame's start.
    rx_since: Option<Instant>,
    rx_continuous: bool,
    /// Bumped on every mode change, so a timer armed for an earlier mode finds
    /// itself stale and does nothing.
    generation: u64,
    busy_until: Option<Instant>,
    host_wake: bool,
    counters: Counters,
}

impl Model {
    fn fresh() -> Self {
        Self {
            mode: ChipMode::StandbyRc,
            registers: HashMap::new(),
            buffer: [0; BUFFER_BYTES],
            tx_base: 0,
            rx_base: 0,
            modulation: None,
            packet: Packet {
                preamble_symbols: 8,
                implicit_header: false,
                payload_length: 0,
            },
            frequency_raw: 0,
            irq_status: 0,
            irq_mask: 0,
            dio1_mask: 0,
            rx_len: 0,
            rx_offset: 0,
            rssi_raw: 0,
            snr_raw: 0,
            rx_since: None,
            rx_continuous: false,
            generation: 0,
            busy_until: None,
            host_wake: false,
            counters: Counters::default(),
        }
    }

    fn register(&self, address: u16) -> u8 {
        self.registers.get(&address).copied().unwrap_or(0)
    }

    /// Set only what the IRQ mask lets through: a masked source is not stored.
    fn raise(&mut self, bits: u16) {
        self.irq_status |= bits & self.irq_mask;
    }

    fn irq_on_dio1(&self) -> bool {
        self.irq_status & self.dio1_mask != 0
    }

    fn set_mode(&mut self, mode: ChipMode) {
        self.mode = mode;
        if mode != ChipMode::Receive {
            self.rx_since = None;
        }
        self.generation += 1;
    }

    /// Apply a command that changes state and answers nothing.
    fn configure(&mut self, opcode: u8, params: &[u8]) {
        let at = |i: usize| params.get(i).copied().unwrap_or(0);
        let be16 = |i: usize| u16::from_be_bytes([at(i), at(i + 1)]);
        match opcode {
            OP_WRITE_REGISTER => {
                let address = be16(0);
                for (offset, byte) in params.iter().skip(2).enumerate() {
                    let at = address.wrapping_add(u16::try_from(offset).unwrap_or(u16::MAX));
                    self.registers.insert(at, *byte);
                }
            }
            OP_WRITE_BUFFER => {
                let base = usize::from(at(0));
                for (offset, byte) in params.iter().skip(1).enumerate() {
                    self.buffer[(base + offset) % BUFFER_BYTES] = *byte;
                }
            }
            OP_SET_SLEEP => {
                self.set_mode(ChipMode::Sleep);
            }
            OP_SET_STANDBY => {
                self.set_mode(if at(0) == 1 {
                    ChipMode::StandbyXosc
                } else {
                    ChipMode::StandbyRc
                });
            }
            OP_SET_FS => {
                self.set_mode(ChipMode::FrequencySynthesis);
            }
            OP_SET_RF_FREQUENCY => {
                self.frequency_raw = u32::from_be_bytes([at(0), at(1), at(2), at(3)]);
            }
            OP_SET_BUFFER_BASE_ADDRESS => {
                self.tx_base = at(0);
                self.rx_base = at(1);
            }
            OP_SET_MODULATION_PARAMS => {
                if let (Some(sf), Some(bw), Some(cr)) = (
                    spreading_factor_from_raw(at(0)),
                    bandwidth_from_raw(at(1)),
                    coding_rate_from_raw(at(2)),
                ) {
                    self.modulation = Some(BaseBandModulationParams::new(sf, bw, cr));
                }
            }
            OP_SET_PACKET_PARAMS => {
                self.packet = Packet {
                    preamble_symbols: be16(0),
                    implicit_header: at(2) == 1,
                    payload_length: at(3),
                };
            }
            OP_CFG_DIO_IRQ => {
                self.irq_mask = be16(0);
                self.dio1_mask = be16(2);
            }
            OP_CLR_IRQ_STATUS => {
                self.irq_status &= !be16(0);
            }
            _ => {}
        }
    }

    /// The bytes a command that only reads clocks out: status byte first where
    /// the chip sends one, none for the buffer and register reads, which the
    /// driver reads past a NOP instead.
    fn answer(&self, opcode: u8, params: &[u8]) -> Vec<u8> {
        let at = |i: usize| params.get(i).copied().unwrap_or(0);
        let status = self.mode.status_bits();
        match opcode {
            OP_READ_REGISTER => {
                let address = u16::from_be_bytes([at(0), at(1)]);
                (0..=u8::MAX)
                    .map(|offset| self.register(address.wrapping_add(u16::from(offset))))
                    .collect()
            }
            OP_READ_BUFFER => self.buffer[usize::from(at(0))..].to_vec(),
            OP_GET_IRQ_STATUS => {
                let [high, low] = self.irq_status.to_be_bytes();
                vec![status, high, low]
            }
            OP_GET_STATUS => vec![status],
            OP_GET_RX_BUFFER_STATUS => vec![status, self.rx_len, self.rx_offset],
            OP_GET_PACKET_STATUS => vec![status, self.rssi_raw, self.snr_raw, self.rssi_raw],
            OP_GET_RSSI_INST => vec![status, rssi_raw(IDLE_RSSI_DBM)],
            OP_GET_PACKET_TYPE => vec![status, PACKET_TYPE_LORA],
            OP_GET_DEVICE_ERRORS | OP_CLEAR_DEVICE_ERRORS => vec![status, 0, 0],
            OP_GET_STATS => vec![status, 0, 0, 0, 0, 0, 0],
            _ => Vec::new(),
        }
    }
}

struct Inner {
    model: Mutex<Model>,
    wake: Condvar,
    outbound: Sender<Vec<u8>>,
    events: Sender<RadioEvent>,
    scale: u32,
}

/// One virtual chip, shared by its bus, its control lines, its medium and its
/// timers. Cloning shares it.
#[derive(Clone)]
pub struct Chip {
    inner: Arc<Inner>,
}

impl Chip {
    /// A chip in standby. Frames it transmits go to `outbound`; what it has to
    /// say about frames it missed goes to `events`. `scale` compresses its
    /// airtime and timeouts the way the rest of a test compresses time.
    #[must_use]
    pub fn new(scale: u32, outbound: Sender<Vec<u8>>, events: Sender<RadioEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                model: Mutex::new(Model::fresh()),
                wake: Condvar::new(),
                outbound,
                events,
                scale: scale.max(1),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Model> {
        self.inner
            .model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn scaled(&self, duration: Duration) -> Duration {
        duration / self.inner.scale
    }

    /// The time scale the chip runs at.
    #[must_use]
    pub fn scale(&self) -> u32 {
        self.inner.scale
    }

    /// The reset line: everything back to power-on, in standby.
    pub fn reset(&self) {
        {
            let mut model = self.lock();
            let counters = model.counters;
            *model = Model::fresh();
            model.counters = counters;
        }
        self.inner.wake.notify_all();
    }

    /// A view of the model.
    #[must_use]
    pub fn snapshot(&self) -> ChipSnapshot {
        let model = self.lock();
        ChipSnapshot {
            mode: model.mode,
            irq_status: model.irq_status,
            counters: model.counters,
        }
    }

    /// Block while BUSY is high.
    pub fn wait_on_busy(&self) {
        let until = self.lock().busy_until;
        if let Some(until) = until {
            let now = Instant::now();
            if until > now {
                thread::sleep(until - now);
            }
        }
    }

    /// Whether an IRQ routed to DIO1 is pending.
    #[must_use]
    pub fn irq_pending(&self) -> bool {
        self.lock().irq_on_dio1()
    }

    /// Wake whoever waits on DIO1, without an IRQ: the host has something to do.
    pub fn wake_host(&self) {
        self.lock().host_wake = true;
        self.inner.wake.notify_all();
    }

    /// Block until an IRQ reaches DIO1 or the host asks for attention.
    pub fn await_irq(&self) {
        let mut model = self.lock();
        loop {
            if model.irq_on_dio1() || model.host_wake {
                model.host_wake = false;
                return;
            }
            model = self
                .inner
                .wake
                .wait(model)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Block until an IRQ reaches DIO1, the host asks for attention, or the
    /// timeout passes, and say which.
    pub fn wait_for_activity(&self, timeout: Duration) -> Activity {
        let deadline = Instant::now() + timeout;
        let mut model = self.lock();
        loop {
            let activity = Activity {
                irq: model.irq_on_dio1(),
                host: model.host_wake,
            };
            let now = Instant::now();
            if activity.irq || activity.host || now >= deadline {
                model.host_wake = false;
                return activity;
            }
            let (guard, _) = self
                .inner
                .wake
                .wait_timeout(model, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            model = guard;
        }
    }

    /// Execute one command; `params` excludes the opcode. Returns the bytes the
    /// chip clocks out for commands the driver reads from, status byte first
    /// where the chip sends one.
    #[must_use]
    pub fn execute(&self, opcode: u8, params: &[u8]) -> Vec<u8> {
        let at = |i: usize| params.get(i).copied().unwrap_or(0);
        let mut model = self.lock();
        if model.mode == ChipMode::Sleep {
            // NSS going low wakes the chip; the command executes in standby.
            model.set_mode(ChipMode::StandbyRc);
        }
        let mut timer = None;
        let reply = match opcode {
            OP_READ_REGISTER
            | OP_READ_BUFFER
            | OP_GET_IRQ_STATUS
            | OP_GET_STATUS
            | OP_GET_RX_BUFFER_STATUS
            | OP_GET_PACKET_STATUS
            | OP_GET_RSSI_INST
            | OP_GET_PACKET_TYPE
            | OP_GET_DEVICE_ERRORS
            | OP_CLEAR_DEVICE_ERRORS
            | OP_GET_STATS => model.answer(opcode, params),
            OP_SET_TX => {
                timer = Some(self.begin_transmit(&mut model));
                Vec::new()
            }
            OP_SET_RX | OP_SET_RX_DUTY_CYCLE => {
                timer = self.begin_receive(&mut model, [at(0), at(1), at(2)]);
                Vec::new()
            }
            OP_SET_CAD => {
                // Exit mode 0: back to standby once done. The medium does not
                // yet say whether anyone is on the air, so nothing is detected.
                model.set_mode(ChipMode::StandbyRc);
                model.raise(IRQ_CAD_DONE);
                Vec::new()
            }
            _ => {
                model.configure(opcode, params);
                Vec::new()
            }
        };
        if opcode != OP_SET_SLEEP {
            model.busy_until = Some(Instant::now() + BUSY);
        }
        drop(model);
        if let Some(timer) = timer {
            self.arm(timer);
        }
        self.inner.wake.notify_all();
        reply
    }

    /// `SetTx`: the buffer goes to the medium now, `TxDone` comes after the
    /// airtime the chip's own parameters imply.
    fn begin_transmit(&self, model: &mut Model) -> Timer {
        let base = usize::from(model.tx_base);
        let payload: Vec<u8> = (0..usize::from(model.packet.payload_length))
            .map(|offset| model.buffer[(base + offset) % BUFFER_BYTES])
            .collect();
        let airtime = self.airtime(model);
        let _ = self.inner.outbound.send(payload);
        model.set_mode(ChipMode::Transmit);
        model.counters.transmitted += 1;
        Timer::TxDone {
            after: airtime,
            generation: model.generation,
        }
    }

    /// `SetRx`: listen, forever or for the timeout in 15.625 µs ticks.
    fn begin_receive(&self, model: &mut Model, timeout: [u8; 3]) -> Option<Timer> {
        let ticks = u32::from_be_bytes([0, timeout[0], timeout[1], timeout[2]]);
        model.set_mode(ChipMode::Receive);
        model.rx_since = Some(Instant::now());
        model.rx_continuous = ticks == RX_CONTINUOUS_TICKS;
        if ticks == 0 || model.rx_continuous {
            None
        } else {
            Some(Timer::RxTimeout {
                after: self.scaled(TICK * ticks),
                generation: model.generation,
            })
        }
    }

    fn arm(&self, timer: Timer) {
        let chip = self.clone();
        let after = match timer {
            Timer::TxDone { after, .. } | Timer::RxTimeout { after, .. } => after,
        };
        thread::spawn(move || {
            thread::sleep(after);
            chip.fire(timer);
        });
    }

    fn fire(&self, timer: Timer) {
        {
            let mut model = self.lock();
            match timer {
                Timer::TxDone { generation, .. }
                    if model.mode == ChipMode::Transmit && model.generation == generation =>
                {
                    model.set_mode(ChipMode::StandbyRc);
                    model.raise(IRQ_TX_DONE);
                }
                Timer::RxTimeout { generation, .. }
                    if model.mode == ChipMode::Receive && model.generation == generation =>
                {
                    model.set_mode(ChipMode::StandbyRc);
                    model.raise(IRQ_TIMEOUT);
                    model.counters.timeouts += 1;
                }
                Timer::TxDone { .. } | Timer::RxTimeout { .. } => {}
            }
        }
        self.inner.wake.notify_all();
    }

    /// Time on the air for the buffer under the current parameters, scaled.
    fn airtime(&self, model: &Model) -> Duration {
        let Some(params) = model.modulation else {
            return self.scaled(Duration::from_secs(1));
        };
        let preamble = u8::try_from(model.packet.preamble_symbols).unwrap_or(u8::MAX);
        let micros = params.time_on_air_us(
            Some(preamble),
            !model.packet.implicit_header,
            model.packet.payload_length,
        );
        self.scaled(Duration::from_micros(u64::from(micros)))
    }

    /// How late a receiver may start listening and still catch a frame.
    fn late_tolerance(&self, model: &Model) -> Duration {
        let symbol = model.modulation.map_or(Duration::from_millis(8), |params| {
            Duration::from_micros(u64::from(params.symbol_duration_us()))
        });
        (self.scaled(symbol) * 2).max(LATE_TOLERANCE_FLOOR)
    }

    /// A frame has just ended at this receiver. It is received if the chip was
    /// listening from (nearly) its start; otherwise it is gone, and the host is
    /// told why, because a protocol that misses frames while it transmits should
    /// know how often.
    pub fn deliver(&self, delivery: &Delivery) {
        let mut model = self.lock();
        let now = Instant::now();
        let airtime = Duration::from_millis(u64::from(delivery.airtime_ms));
        let started = now.checked_sub(airtime).unwrap_or(now);
        let tolerance = self.late_tolerance(&model);
        let listening = model.mode == ChipMode::Receive
            && model
                .rx_since
                .is_some_and(|since| since <= started + tolerance);
        if !listening {
            let why = match model.mode {
                ChipMode::Transmit => {
                    model.counters.missed_transmitting += 1;
                    "transmitting"
                }
                ChipMode::Receive => {
                    model.counters.missed_late += 1;
                    "late"
                }
                ChipMode::Sleep
                | ChipMode::StandbyRc
                | ChipMode::StandbyXosc
                | ChipMode::FrequencySynthesis => {
                    model.counters.missed_idle += 1;
                    "idle"
                }
            };
            drop(model);
            let _ = self.inner.events.send(RadioEvent::Note(format!(
                "\"event\":\"chip_missed\",\"why\":\"{why}\",\"bytes\":{}",
                delivery.bytes.len()
            )));
            return;
        }
        let length = u8::try_from(delivery.bytes.len().min(u8::MAX as usize)).unwrap_or(u8::MAX);
        let base = usize::from(model.rx_base);
        for (offset, byte) in delivery.bytes.iter().take(usize::from(length)).enumerate() {
            model.buffer[(base + offset) % BUFFER_BYTES] = *byte;
        }
        model.rx_len = length;
        model.rx_offset = model.rx_base;
        model.rssi_raw = rssi_raw(delivery.rssi_dbm);
        model.snr_raw = snr_raw(delivery.snr_db);
        let mut flags = IRQ_PREAMBLE_DETECTED | IRQ_SYNCWORD_VALID | IRQ_HEADER_VALID | IRQ_RX_DONE;
        if delivery.crc_ok {
            model.counters.received += 1;
        } else {
            flags |= IRQ_CRC_ERR;
            model.counters.crc_errors += 1;
        }
        model.raise(flags);
        if !model.rx_continuous {
            model.set_mode(ChipMode::StandbyRc);
        }
        drop(model);
        self.inner.wake.notify_all();
    }
}

/// `RssiPkt` as the chip encodes it: minus twice the dBm value.
fn rssi_raw(rssi_dbm: i16) -> u8 {
    u8::try_from((-i32::from(rssi_dbm) * 2).clamp(0, 255)).unwrap_or(u8::MAX)
}

/// `SnrPkt` as the chip encodes it: four times the dB value, two's complement.
fn snr_raw(snr_db: i8) -> u8 {
    let quarters = (i32::from(snr_db) * 4).clamp(i32::from(i8::MIN), i32::from(i8::MAX));
    i8::try_from(quarters).unwrap_or(0).to_le_bytes()[0]
}

fn spreading_factor_from_raw(value: u8) -> Option<SpreadingFactor> {
    Some(match value {
        0x05 => SpreadingFactor::_5,
        0x06 => SpreadingFactor::_6,
        0x07 => SpreadingFactor::_7,
        0x08 => SpreadingFactor::_8,
        0x09 => SpreadingFactor::_9,
        0x0A => SpreadingFactor::_10,
        0x0B => SpreadingFactor::_11,
        0x0C => SpreadingFactor::_12,
        _ => return None,
    })
}

fn bandwidth_from_raw(value: u8) -> Option<Bandwidth> {
    Some(match value {
        0x00 => Bandwidth::_7KHz,
        0x08 => Bandwidth::_10KHz,
        0x01 => Bandwidth::_15KHz,
        0x09 => Bandwidth::_20KHz,
        0x02 => Bandwidth::_31KHz,
        0x0A => Bandwidth::_41KHz,
        0x03 => Bandwidth::_62KHz,
        0x04 => Bandwidth::_125KHz,
        0x05 => Bandwidth::_250KHz,
        0x06 => Bandwidth::_500KHz,
        _ => return None,
    })
}

fn coding_rate_from_raw(value: u8) -> Option<CodingRate> {
    Some(match value {
        0x01 => CodingRate::_4_5,
        0x02 => CodingRate::_4_6,
        0x03 => CodingRate::_4_7,
        0x04 => CodingRate::_4_8,
        _ => return None,
    })
}
