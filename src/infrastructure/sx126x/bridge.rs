//! A real `SX1262` on the far end of a USB cable, behind the [`Radio`] seam.
//!
//! The board is a microcontroller with the module wired to it (first, the
//! Seeed XIAO ESP32-S3 with a Wio-SX1262; D23), running the firmware under
//! `firmware/bridge`. That firmware makes the board a remote SPI device: the
//! host sends SPI transactions and pin operations over a USB CDC serial port,
//! and the unmodified `lora-phy` driver runs here, exactly as it runs on the
//! virtual chip. No modem firmware sits between the protocol and the radio,
//! so the timing the node sees is the chip's own -- which is why this exists
//! rather than an `RNode`, whose CSMA delays every frame by up to seconds at
//! SF10 and forces its own preamble.
//!
//! # The protocol
//!
//! KISS framing, as `RNode`'s ([`kiss`]): `FEND` delimits a frame, `FESC`
//! escapes. The first byte of a frame is the command. The host sends
//! requests; the device answers each with the same command byte plus `0x80`,
//! and sends events on its own initiative. One request is in flight at a
//! time.
//!
//! | Request | Payload | Reply payload |
//! |---|---|---|
//! | `0x01` `HELLO` | -- | protocol version, firmware major, minor, board id, session id (`u32` little-endian), BUSY level, DIO1 level; turns DIO1 events on |
//! | `0x02` `RESET` | -- | status, once NRESET has been pulsed and BUSY seen low |
//! | `0x03` `BUSY` | timeout in ms, `u16` little-endian | status: `0` once BUSY is low, `1` on timeout |
//! | `0x04` `PINS` | -- | BUSY level, DIO1 level |
//! | `0x05` `RF` | mode: `0` off, `1` receive, `2` transmit | status |
//! | `0x06` `SPI` | operations, below | status, then every byte read, in order |
//! | `0x07` `EVENTS` | `0` off, `1` on | DIO1 level |
//!
//! An `SPI` request is a list of operations executed with NSS low across all
//! of them, after BUSY has been seen low: each is a kind byte, a `u16`
//! little-endian length, and for kinds `0` (write) and `2` (transfer) that
//! many bytes to clock out. Kind `1` (read) clocks out zeros and returns the
//! bytes read, kind `2` returns them too, and kind `3` (delay) pauses for
//! `length` microseconds. Statuses: `0` ok, `1` timeout, `2` bad request,
//! `3` too long, `4` unknown command.
//!
//! Events: `0xE1` DIO1 rose (payload: the level *as the device read it
//! after the edge*), `0xEE` error (payload: status, offending command). A
//! rising edge the device latched before the host asked for events is not
//! reported; the level in the `HELLO` reply is.
//!
//! **An event is a hint, never a fact.** A DIO1 notice may cross a USB cable
//! that is slower than the driver, so by the time it lands the chip's IRQ may
//! already be cleared. The host therefore drops a notice whose level says the
//! line has fallen, and confirms every other one by reading the line before
//! it wakes the driver -- because the driver's preamble path clears the IRQ
//! status, and a spurious wake there would discard a reception that arrived
//! in between.
//!
//! **The session id says the board is the one the host met.** It is drawn
//! afresh at every boot. A host that stops hearing replies asks again: a
//! different id means the microcontroller restarted mid-operation, the chip
//! is no longer configured, and the link poisons itself rather than carrying
//! on with a radio that is not listening.
//!
//! Board ids: `0x01` XIAO ESP32-S3 + Wio-SX1262 kit, `0xFE` the reference
//! device on the virtual chip ([`device`]), which is the specification the
//! firmware must match and what the tests run against.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use embedded_hal::spi::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::spi::SpiDevice;
use lora_modulation::BaseBandModulationParams;
use lora_phy::mod_params::RadioError as DriverError;
use lora_phy::mod_traits::InterfaceVariant;
use lora_phy::sx126x::{Config, Sx126x, Sx1262, TcxoCtrlVoltage};

use super::chip::Activity;
use super::radio::{DriverHandle, Watch, bandwidth, coding_rate, drive, spreading_factor};
use crate::application::{PhyProfile, Radio, RadioError, RadioEvent};
use crate::infrastructure::rnode::kiss;

/// How long any reply may take. The device answers an SPI transaction in
/// under a millisecond; USB and a busy host add a few more.
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
/// How long one `HELLO` attempt waits before the next.
const HELLO_TIMEOUT: Duration = Duration::from_millis(300);
/// What the bridge, the host and the driver may add to a chip's own timing
/// before a wait is called a failure.
const BRIDGE_MARGIN: Duration = Duration::from_secs(1);
/// The deadline used when a profile's timing cannot be worked out.
const FALLBACK_IRQ_TIMEOUT: Duration = Duration::from_secs(12);
/// While waiting for DIO1, how often the level is read in case an edge went
/// missing.
const IRQ_SLICE: Duration = Duration::from_millis(100);
/// How often the reader thread checks whether it is still wanted.
const READ_SLICE: Duration = Duration::from_millis(20);
/// The nominal rate of a USB CDC port; the device ignores it.
const BAUD: u32 = 115_200;
/// `GetIrqStatus`: a status byte, then the sixteen IRQ bits.
const OP_GET_IRQ_STATUS: u8 = 0x12;
/// The chip's data buffer: the longest frame a profile can put on the air.
const MAX_PAYLOAD_BYTES: u8 = 255;

/// How long the driver may wait on DIO1 before the wait is a failure.
///
/// The one place the driver waits on the line is inside `tx()`, for
/// `TxDone`, so the bound is the time on air of the longest frame this
/// profile can carry plus what the bridge and the host may add. A fixed
/// constant would be either too short for a slower profile or too slow to
/// notice a board that has stopped answering.
fn irq_timeout(profile: PhyProfile) -> Duration {
    let (Some(sf), Some(bw), Some(cr)) = (
        spreading_factor(profile.spreading_factor),
        bandwidth(profile.bandwidth_hz),
        coding_rate(profile.coding_rate_denominator),
    ) else {
        return FALLBACK_IRQ_TIMEOUT;
    };
    let preamble = u8::try_from(profile.preamble_symbols).unwrap_or(u8::MAX);
    let micros = BaseBandModulationParams::new(sf, bw, cr).time_on_air_us(
        Some(preamble),
        profile.explicit_header,
        MAX_PAYLOAD_BYTES,
    );
    Duration::from_micros(u64::from(micros)) + BRIDGE_MARGIN
}

/// The bytes of the protocol, shared by the adapter, the reference device
/// and -- by hand -- the firmware.
pub mod protocol {
    use embedded_hal::spi::Operation;

    /// The protocol version `HELLO` must answer with. Version 2 added the
    /// session id.
    pub const VERSION: u8 = 2;

    /// Who is there; turns DIO1 events on.
    pub const HELLO: u8 = 0x01;
    /// Pulse NRESET, wait for BUSY.
    pub const RESET: u8 = 0x02;
    /// Wait for BUSY to go low.
    pub const BUSY: u8 = 0x03;
    /// Read BUSY and DIO1.
    pub const PINS: u8 = 0x04;
    /// Steer the antenna switch.
    pub const RF: u8 = 0x05;
    /// One SPI transaction.
    pub const SPI: u8 = 0x06;
    /// DIO1 events on or off.
    pub const EVENTS: u8 = 0x07;
    /// Set on a request byte to make its reply.
    pub const REPLY: u8 = 0x80;
    /// DIO1 rose.
    pub const EVENT_DIO1: u8 = 0xE1;
    /// The device could not do what it was asked.
    pub const EVENT_ERROR: u8 = 0xEE;

    /// Done.
    pub const STATUS_OK: u8 = 0;
    /// BUSY stayed high.
    pub const STATUS_TIMEOUT: u8 = 1;
    /// The payload was malformed.
    pub const STATUS_BAD_REQUEST: u8 = 2;
    /// The reply would not fit a frame.
    pub const STATUS_TOO_LONG: u8 = 3;
    /// The command byte means nothing to the device.
    pub const STATUS_UNKNOWN_COMMAND: u8 = 4;

    /// Clock bytes out.
    pub const OP_WRITE: u8 = 0;
    /// Clock zeros out, return what came back.
    pub const OP_READ: u8 = 1;
    /// Clock bytes out, return what came back.
    pub const OP_TRANSFER: u8 = 2;
    /// Pause, NSS held low.
    pub const OP_DELAY: u8 = 3;

    /// No antenna.
    pub const RF_OFF: u8 = 0;
    /// The receive path.
    pub const RF_RX: u8 = 1;
    /// The transmit path.
    pub const RF_TX: u8 = 2;

    /// Seeed XIAO ESP32-S3 + Wio-SX1262 kit.
    pub const BOARD_XIAO_S3_WIO_SX1262: u8 = 0x01;
    /// The reference device on the virtual chip.
    pub const BOARD_VIRTUAL: u8 = 0xFE;

    /// The most a decoded frame may hold, either way.
    pub const FRAME_LIMIT: usize = 1_100;
    /// How long the device lets BUSY stay high before giving up.
    pub const BUSY_LIMIT_MS: u16 = 100;

    /// A driver transaction as the payload of an `SPI` request.
    ///
    /// A transfer whose read side is longer than its write side clocks
    /// zeros out for the difference, as the `embedded-hal` contract says.
    #[must_use]
    pub fn encode_operations(operations: &[Operation<'_, u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut push = |kind: u8, length: usize, bytes: &[u8]| {
            let length = u16::try_from(length).unwrap_or(u16::MAX);
            out.push(kind);
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(bytes);
        };
        for operation in operations {
            match operation {
                Operation::Write(bytes) => push(OP_WRITE, bytes.len(), bytes),
                Operation::Read(buffer) => push(OP_READ, buffer.len(), &[]),
                Operation::Transfer(read, write) => {
                    // A transfer clocks out exactly `length` bytes: the
                    // write side, then zeros for a longer read side.
                    let length = read.len().max(write.len());
                    let mut bytes = write.to_vec();
                    bytes.resize(length, 0);
                    push(OP_TRANSFER, length, &bytes);
                }
                Operation::TransferInPlace(buffer) => push(OP_TRANSFER, buffer.len(), buffer),
                Operation::DelayNs(ns) => {
                    push(
                        OP_DELAY,
                        usize::try_from(ns.div_ceil(1_000)).unwrap_or(0),
                        &[],
                    );
                }
            }
        }
        out
    }

    /// One parsed operation of an `SPI` request, as the device sees it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Op<'a> {
        /// `OP_WRITE`, `OP_READ`, `OP_TRANSFER` or `OP_DELAY`.
        pub kind: u8,
        /// Bytes to read back, or microseconds to pause.
        pub length: usize,
        /// The bytes to clock out, for a write or a transfer.
        pub bytes: &'a [u8],
    }

    /// Parse an `SPI` request payload; `None` if it is malformed.
    #[must_use]
    pub fn decode_operations(payload: &[u8]) -> Option<Vec<Op<'_>>> {
        let mut ops = Vec::new();
        let mut rest = payload;
        while !rest.is_empty() {
            let (&[kind, low, high], after) = rest.split_first_chunk::<3>()?;
            let length = usize::from(u16::from_le_bytes([low, high]));
            let (bytes, after) = match kind {
                OP_WRITE | OP_TRANSFER => after.split_at_checked(length)?,
                OP_READ | OP_DELAY => (&[][..], after),
                _ => return None,
            };
            ops.push(Op {
                kind,
                length,
                bytes,
            });
            rest = after;
        }
        Some(ops)
    }

    /// What the device says in reply to `HELLO`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Hello {
        /// The protocol version it speaks.
        pub protocol: u8,
        /// Firmware major and minor version.
        pub firmware: (u8, u8),
        /// Which board it is.
        pub board: u8,
        /// Drawn afresh every time the device boots: a change means it
        /// restarted, and nothing the host configured survived.
        pub session: u32,
        /// BUSY was high.
        pub busy: bool,
        /// DIO1 was high: an IRQ is pending from before the host was here.
        pub dio1: bool,
    }

    impl Hello {
        /// The protocol version alone, which every version of the reply
        /// carries first. Read before [`Hello::parse`], so that a device
        /// speaking another version is told apart from a truncated reply.
        #[must_use]
        pub fn version(body: &[u8]) -> Option<u8> {
            body.first().copied()
        }

        /// Decode the reply payload; `None` if it is short.
        #[must_use]
        pub fn parse(body: &[u8]) -> Option<Self> {
            let &[protocol, major, minor, board, s0, s1, s2, s3, busy, dio1] =
                body.first_chunk::<10>()?;
            Some(Self {
                protocol,
                firmware: (major, minor),
                board,
                session: u32::from_le_bytes([s0, s1, s2, s3]),
                busy: busy != 0,
                dio1: dio1 != 0,
            })
        }

        /// The reply payload for these values.
        #[must_use]
        pub fn encode(&self) -> [u8; 10] {
            let [s0, s1, s2, s3] = self.session.to_le_bytes();
            [
                self.protocol,
                self.firmware.0,
                self.firmware.1,
                self.board,
                s0,
                s1,
                s2,
                s3,
                u8::from(self.busy),
                u8::from(self.dio1),
            ]
        }
    }
}

use protocol::Hello;

/// Why the bridge could not do something.
#[derive(Debug)]
pub enum BridgeError {
    /// The serial port.
    Port(io::Error),
    /// A thread could not be spawned.
    Thread(io::Error),
    /// No reply to this command in time.
    Timeout(u8),
    /// The port closed.
    Gone,
    /// The device answered this command with a failure status.
    Status {
        /// The command.
        command: u8,
        /// Its status byte.
        status: u8,
    },
    /// The device sent an error event while this command was in flight.
    Device {
        /// The command in flight.
        command: u8,
        /// The status the event carried.
        code: u8,
    },
    /// The device said something the protocol does not allow.
    Protocol(String),
    /// The device restarted while the host was using it: everything the
    /// driver configured is gone, so the link refuses to carry on.
    Reset {
        /// The session the host met.
        met: u32,
        /// The session answering now.
        now: u32,
    },
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port(error) => write!(f, "serial port: {error}"),
            Self::Thread(error) => write!(f, "bridge thread: {error}"),
            Self::Timeout(command) => write!(f, "no reply to command {command:#04x}"),
            Self::Gone => f.write_str("the port closed"),
            Self::Status { command, status } => {
                write!(f, "command {command:#04x} failed with status {status}")
            }
            Self::Device { command, code } => {
                write!(
                    f,
                    "the device reported error {code} during command {command:#04x}"
                )
            }
            Self::Protocol(what) => write!(f, "protocol: {what}"),
            Self::Reset { met, now } => write!(
                f,
                "the device restarted mid-operation: session {met:#010x} became {now:#010x}"
            ),
        }
    }
}

impl std::error::Error for BridgeError {}

/// A byte pipe to the device that two threads share: one reads, one writes.
pub trait Port: Send + Sync + 'static {
    /// Read what is available, blocking at most briefly: `TimedOut` (or
    /// `Ok(0)` never, except at the end of the pipe) when nothing came.
    ///
    /// # Errors
    ///
    /// The port's own.
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize>;
    /// Write all of it.
    ///
    /// # Errors
    ///
    /// The port's own.
    fn write_all(&self, bytes: &[u8]) -> io::Result<()>;
}

#[cfg(feature = "hardware")]
impl Port for serial2::SerialPort {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        serial2::SerialPort::read(self, buffer)
    }

    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        serial2::SerialPort::write_all(self, bytes)
    }
}

enum Inbound {
    Frame(Vec<u8>),
    Wake,
    Gone,
}

/// The host's end of the protocol: requests out, replies and events in,
/// one request in flight at a time.
///
/// The driver thread owns every call. The node thread only ever asks for
/// attention ([`Link::wake`]), which lands in the same inbox as the device's
/// events so that one wait serves both.
pub struct Link {
    port: Arc<dyn Port>,
    inbox: Mutex<Receiver<Inbound>>,
    wakes: Sender<Inbound>,
    /// DIO1 rose and nobody has acted on it yet.
    irq: AtomicBool,
    /// The host asked for attention while a call was in progress.
    woken: AtomicBool,
    /// The session the device answered `HELLO` with, once it has.
    session: AtomicU32,
    /// The device restarted under the host: every later call refuses.
    poisoned: AtomicBool,
    /// Cleared when the link is dropped, so the reader thread lets go.
    alive: Arc<AtomicBool>,
}

impl Link {
    /// Start reading `port`.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Thread`] if the reader thread could not be spawned.
    pub fn open(port: Arc<dyn Port>) -> Result<Arc<Self>, BridgeError> {
        let (frames, inbox) = channel();
        let alive = Arc::new(AtomicBool::new(true));
        let reader = Arc::clone(&port);
        let sink = frames.clone();
        let wanted = Arc::clone(&alive);
        thread::Builder::new()
            .name("sx126x-bridge-port".into())
            .spawn(move || read_frames(reader.as_ref(), &sink, &wanted))
            .map_err(BridgeError::Thread)?;
        Ok(Arc::new(Self {
            port,
            inbox: Mutex::new(inbox),
            wakes: frames,
            irq: AtomicBool::new(false),
            woken: AtomicBool::new(false),
            session: AtomicU32::new(0),
            poisoned: AtomicBool::new(false),
            alive,
        }))
    }

    /// Send a request and wait for its reply payload. Events that arrive
    /// meanwhile are kept: a DIO1 edge is latched, an error is the answer.
    ///
    /// # Errors
    ///
    /// See [`BridgeError`].
    pub fn call(
        &self,
        command: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, BridgeError> {
        self.guard()?;
        self.port
            .write_all(&kiss::encode(command, payload))
            .map_err(BridgeError::Port)?;
        let deadline = Instant::now() + timeout;
        let inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(remaining) {
                Ok(Inbound::Frame(frame)) => match frame.split_first() {
                    Some((&head, body)) if head == command | protocol::REPLY => {
                        return Ok(body.to_vec());
                    }
                    Some((&protocol::EVENT_DIO1, level)) => {
                        // A notice whose level says the line has fallen is
                        // stale: the driver cleared the IRQ before the cable
                        // caught up. Latching it would wake the driver into
                        // the path that clears the status, which would eat
                        // the next reception.
                        if level.first().copied().unwrap_or(1) != 0 {
                            self.irq.store(true, Ordering::Release);
                        }
                    }
                    Some((&protocol::EVENT_ERROR, body)) => {
                        // The event names what it is about. One about
                        // something else is stale, not this call's answer.
                        let offending = body.get(1).copied().unwrap_or(command);
                        if offending == command {
                            return Err(BridgeError::Device {
                                command,
                                code: body.first().copied().unwrap_or(0),
                            });
                        }
                    }
                    // A reply to something else: stale, from before the port
                    // was opened. Nothing waits for it.
                    Some(_) | None => {}
                },
                Ok(Inbound::Wake) => self.woken.store(true, Ordering::Release),
                Ok(Inbound::Gone) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(BridgeError::Gone);
                }
                Err(RecvTimeoutError::Timeout) => return Err(BridgeError::Timeout(command)),
            }
        }
    }

    /// Throw away whatever the device has already said.
    fn drain(&self) {
        let inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        while inbox.try_recv().is_ok() {}
    }

    /// Refuse to speak to a device that restarted under us.
    fn guard(&self) -> Result<(), BridgeError> {
        if self.poisoned.load(Ordering::Acquire) {
            let met = self.session.load(Ordering::Acquire);
            return Err(BridgeError::Reset { met, now: met });
        }
        Ok(())
    }

    /// A call that treats silence as a question: if nothing answered, ask the
    /// device who it is. A different session means it rebooted, and the chip
    /// it left behind is not the one the driver configured -- so the link
    /// poisons itself and every later call fails, rather than transmitting
    /// into a radio that is no longer listening.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Reset`] once the device has restarted; otherwise
    /// whatever the call itself produced.
    pub fn call_checked(
        &self,
        command: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, BridgeError> {
        match self.call(command, payload, timeout) {
            Err(BridgeError::Timeout(command)) => {
                self.verify_session()?;
                Err(BridgeError::Timeout(command))
            }
            other => other,
        }
    }

    /// Ask the device who it is and compare it with who it was. Poisons the
    /// link if the answer changed.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Reset`] if the session changed; the call's own error if
    /// the device did not answer at all.
    pub fn verify_session(&self) -> Result<(), BridgeError> {
        self.guard()?;
        let met = self.session.load(Ordering::Acquire);
        let body = self.call(protocol::HELLO, &[], HELLO_TIMEOUT)?;
        let Some(hello) = Hello::parse(&body) else {
            return Err(BridgeError::Protocol("short HELLO reply".into()));
        };
        if hello.session == met {
            return Ok(());
        }
        self.poisoned.store(true, Ordering::Release);
        Err(BridgeError::Reset {
            met,
            now: hello.session,
        })
    }

    /// Whether the device has restarted under the host.
    #[must_use]
    pub fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// A request whose reply is a status byte.
    fn expect_ok(&self, command: u8, payload: &[u8], timeout: Duration) -> Result<(), BridgeError> {
        match self.call_checked(command, payload, timeout)?.first() {
            Some(&protocol::STATUS_OK) => Ok(()),
            Some(&status) => Err(BridgeError::Status { command, status }),
            None => Err(BridgeError::Protocol(format!(
                "empty reply to command {command:#04x}"
            ))),
        }
    }

    /// Say hello until the device answers or `patience` runs out: a board
    /// may be resetting as its port is opened.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Timeout`] once patience runs out; the rest as
    /// [`BridgeError`].
    pub fn hello(&self, patience: Duration) -> Result<Hello, BridgeError> {
        // Anything already on the wire was said to somebody else: a host
        // that died mid-call, or a board that has been talking to nobody.
        // A stale reply must not be mistaken for the answer to the first
        // question this host asks.
        self.drain();
        let deadline = Instant::now() + patience;
        loop {
            match self.call(protocol::HELLO, &[], HELLO_TIMEOUT) {
                Ok(body) => {
                    let version = Hello::version(&body)
                        .ok_or_else(|| BridgeError::Protocol("empty HELLO reply".into()))?;
                    if version != protocol::VERSION {
                        return Err(BridgeError::Protocol(format!(
                            "the device speaks protocol version {version}, this adapter version {}",
                            protocol::VERSION
                        )));
                    }
                    let hello = Hello::parse(&body)
                        .ok_or_else(|| BridgeError::Protocol("short HELLO reply".into()))?;
                    self.session.store(hello.session, Ordering::Release);
                    if hello.dio1 {
                        self.irq.store(true, Ordering::Release);
                    }
                    return Ok(hello);
                }
                Err(BridgeError::Timeout(_)) if Instant::now() < deadline => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// The DIO1 level, read from the device.
    ///
    /// # Errors
    ///
    /// See [`BridgeError`].
    pub fn dio1_level(&self) -> Result<bool, BridgeError> {
        let body = self.call_checked(protocol::PINS, &[], CALL_TIMEOUT)?;
        body.get(1)
            .map(|&level| level != 0)
            .ok_or_else(|| BridgeError::Protocol("short PINS reply".into()))
    }

    /// Block until DIO1 rose, the host asked for attention, or the timeout
    /// passed, and say which.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> Activity {
        let pending = Activity {
            irq: self.irq.swap(false, Ordering::AcqRel),
            host: self.woken.swap(false, Ordering::AcqRel),
        };
        if pending.irq || pending.host {
            return pending;
        }
        let deadline = Instant::now() + timeout;
        let inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(remaining) {
                Ok(Inbound::Frame(frame)) => {
                    // As in `call`: a notice that says the line has fallen
                    // is a stale hint, not an interrupt.
                    if let Some((&protocol::EVENT_DIO1, level)) = frame.split_first()
                        && level.first().copied().unwrap_or(1) != 0
                    {
                        return Activity {
                            irq: true,
                            host: false,
                        };
                    }
                }
                Ok(Inbound::Wake) => {
                    return Activity {
                        irq: false,
                        host: true,
                    };
                }
                Ok(Inbound::Gone) | Err(RecvTimeoutError::Disconnected) => {
                    // Nothing will ever arrive; do not spin on it.
                    thread::sleep(remaining);
                    return Activity {
                        irq: false,
                        host: false,
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Activity {
                        irq: false,
                        host: false,
                    };
                }
            }
        }
    }

    /// Ask the driver thread for attention.
    pub fn wake(&self) {
        let _ = self.wakes.send(Inbound::Wake);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

fn read_frames(port: &dyn Port, frames: &Sender<Inbound>, alive: &AtomicBool) {
    let mut deframer = kiss::Deframer::new(protocol::FRAME_LIMIT);
    let mut buffer = [0u8; 1_024];
    let mut out = Vec::new();
    while alive.load(Ordering::Acquire) {
        match port.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                deframer.push(&buffer[..n], &mut out);
                for frame in out.drain(..) {
                    if frames.send(Inbound::Frame(frame)).is_err() {
                        return;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    let _ = frames.send(Inbound::Gone);
}

/// The SPI error type the trait demands.
#[derive(Debug)]
pub struct BridgeSpiError(pub BridgeError);

impl embedded_hal::spi::Error for BridgeSpiError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// The driver's SPI device: every transaction is one `SPI` request.
pub struct BridgeSpi {
    link: Arc<Link>,
}

impl ErrorType for BridgeSpi {
    type Error = BridgeSpiError;
}

// The trait's functions are `async`; the link answers synchronously.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl SpiDevice for BridgeSpi {
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), BridgeSpiError> {
        let payload = protocol::encode_operations(operations);
        let reply = self
            .link
            .call_checked(protocol::SPI, &payload, CALL_TIMEOUT)
            .map_err(BridgeSpiError)?;
        let Some((&status, mut bytes)) = reply.split_first() else {
            return Err(BridgeSpiError(BridgeError::Protocol(
                "empty SPI reply".into(),
            )));
        };
        if status != protocol::STATUS_OK {
            return Err(BridgeSpiError(BridgeError::Status {
                command: protocol::SPI,
                status,
            }));
        }
        for operation in operations.iter_mut() {
            let (buffer, skip) = match operation {
                Operation::Read(buffer) | Operation::TransferInPlace(buffer) => (&mut **buffer, 0),
                Operation::Transfer(read, write) => {
                    let skip = write.len().saturating_sub(read.len());
                    (&mut **read, skip)
                }
                Operation::Write(_) | Operation::DelayNs(_) => continue,
            };
            let wanted = buffer.len() + skip;
            let Some((chunk, rest)) = bytes.split_at_checked(wanted) else {
                return Err(BridgeSpiError(BridgeError::Protocol(format!(
                    "SPI reply short by {} bytes",
                    wanted - bytes.len()
                ))));
            };
            buffer.copy_from_slice(&chunk[..buffer.len()]);
            bytes = rest;
        }
        Ok(())
    }
}

/// The driver's control lines: reset, BUSY, DIO1 and the antenna switch.
pub struct BridgeIv {
    link: Arc<Link>,
    /// How long `TxDone` may take on this profile.
    irq_timeout: Duration,
}

#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl InterfaceVariant for BridgeIv {
    async fn reset(&mut self, _delay: &mut impl DelayNs) -> Result<(), DriverError> {
        self.link
            .expect_ok(protocol::RESET, &[], CALL_TIMEOUT)
            .map_err(|_| DriverError::Reset)
    }

    async fn wait_on_busy(&mut self) -> Result<(), DriverError> {
        self.link
            .expect_ok(
                protocol::BUSY,
                &protocol::BUSY_LIMIT_MS.to_le_bytes(),
                CALL_TIMEOUT,
            )
            .map_err(|_| DriverError::Busy)
    }

    async fn await_irq(&mut self) -> Result<(), DriverError> {
        // The driver waits here for `TxDone`. A notice only ends the wait
        // early; the line itself decides, because a notice can cross the
        // cable after the chip has moved on. A wake from the host is kept
        // for the next wait rather than cutting the transmission short.
        let deadline = Instant::now() + self.irq_timeout;
        let mut host_wanted = false;
        loop {
            host_wanted |= self.link.wait(IRQ_SLICE).host;
            match self.link.dio1_level() {
                Ok(true) => {
                    if host_wanted {
                        self.link.wake();
                    }
                    return Ok(());
                }
                Ok(false) => {}
                Err(_) => return Err(DriverError::DIO1),
            }
            if Instant::now() >= deadline {
                return Err(DriverError::Irq);
            }
        }
    }

    async fn enable_rf_switch_rx(&mut self) -> Result<(), DriverError> {
        self.link
            .expect_ok(protocol::RF, &[protocol::RF_RX], CALL_TIMEOUT)
            .map_err(|_| DriverError::RfSwitchRx)
    }

    async fn enable_rf_switch_tx(&mut self) -> Result<(), DriverError> {
        self.link
            .expect_ok(protocol::RF, &[protocol::RF_TX], CALL_TIMEOUT)
            .map_err(|_| DriverError::RfSwitchTx)
    }

    async fn disable_rf_switch(&mut self) -> Result<(), DriverError> {
        self.link
            .expect_ok(protocol::RF, &[protocol::RF_OFF], CALL_TIMEOUT)
            .map_err(|_| DriverError::RfSwitchRx)
    }
}

/// What the driver thread waits on: DIO1 events, and the host.
pub struct BridgeWatch {
    link: Arc<Link>,
    /// Where a restart is reported, once.
    events: Sender<RadioEvent>,
    /// Whether that report has been made.
    announced: AtomicBool,
}

impl Watch for BridgeWatch {
    fn wait_for_activity(&self, timeout: Duration) -> Activity {
        let host = self.link.wait(timeout).host;
        // A notice only ends the wait early. Whether one arrived or the wait
        // timed out, the line is what decides: the driver's preamble path
        // clears the chip's IRQ status, so waking it on a notice that the
        // chip has already withdrawn would discard the reception that
        // arrived in between.
        let irq = self.link.dio1_level().unwrap_or(false);
        if self.link.poisoned() && !self.announced.swap(true, Ordering::AcqRel) {
            let _ = self.events.send(RadioEvent::Note(
                "\"event\":\"bridge_reset\",\"why\":\"the device restarted; this radio is offline\""
                    .to_string(),
            ));
        }
        Activity { irq, host }
    }

    fn wake_host(&self) {
        self.link.wake();
    }

    fn irq_flags(&self) -> Option<u16> {
        // Unlike a bus the driver owns outright, this one can be borrowed
        // between the driver's transactions: `GetIrqStatus` reads the flags
        // without clearing them, so a CRC failure is seen on real hardware.
        let mut answer = [0u8; 3];
        let ops = [
            Operation::Write(&[OP_GET_IRQ_STATUS]),
            Operation::Read(&mut answer),
        ];
        let payload = protocol::encode_operations(&ops);
        let reply = self.link.call(protocol::SPI, &payload, CALL_TIMEOUT).ok()?;
        match reply.as_slice() {
            [protocol::STATUS_OK, _status, high, low] => Some(u16::from_be_bytes([*high, *low])),
            _ => None,
        }
    }
}

/// How the module on the far end is built.
#[derive(Clone)]
pub struct BridgeOptions {
    /// The TCXO on DIO3, if the module has one; the Wio-SX1262 does, at
    /// 1.8 V.
    pub tcxo: Option<TcxoCtrlVoltage>,
    /// Whether to run the chip's DC-DC converter; the Wio-SX1262 has the
    /// inductor for it.
    pub use_dcdc: bool,
    /// Whether to boost receive gain at the cost of current.
    pub rx_boost: bool,
    /// How long to keep saying hello before giving up on the device.
    pub patience: Duration,
}

impl Default for BridgeOptions {
    /// The XIAO ESP32-S3 + Wio-SX1262 kit.
    fn default() -> Self {
        Self {
            tcxo: Some(TcxoCtrlVoltage::Ctrl1V8),
            use_dcdc: true,
            rx_boost: false,
            patience: Duration::from_secs(4),
        }
    }
}

/// An `SX1262` behind the bridge firmware.
pub struct BridgeRadio {
    handle: DriverHandle,
    hello: Hello,
}

impl BridgeRadio {
    /// Bring the radio up on `port`: find the device, then start the driver
    /// thread on it for `profile`.
    ///
    /// Driver failures after this returns -- an initialisation that does not
    /// take, a transmit that errors -- arrive as [`RadioEvent::Note`]s.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Timeout`] if nothing answered `HELLO`,
    /// [`BridgeError::Protocol`] if what answered speaks another version;
    /// the rest as [`BridgeError`].
    pub fn open(
        port: Arc<dyn Port>,
        options: &BridgeOptions,
        profile: PhyProfile,
    ) -> Result<Self, BridgeError> {
        let link = Link::open(port)?;
        let hello = link.hello(options.patience)?;
        let kind = Sx126x::new(
            BridgeSpi {
                link: Arc::clone(&link),
            },
            BridgeIv {
                link: Arc::clone(&link),
                irq_timeout: irq_timeout(profile),
            },
            Config {
                chip: Sx1262,
                tcxo_ctrl: options.tcxo,
                use_dcdc: options.use_dcdc,
                rx_boost: options.rx_boost,
            },
        );
        let (commands, inbox) = channel();
        let (events_in, events) = channel::<RadioEvent>();
        let _ = events_in.send(RadioEvent::Note(format!(
            "\"event\":\"bridge_up\",\"protocol\":{},\"firmware\":\"{}.{}\",\"board\":{},\"session\":\"{:#010x}\"",
            hello.protocol, hello.firmware.0, hello.firmware.1, hello.board, hello.session
        )));
        let watch: Arc<dyn Watch> = Arc::new(BridgeWatch {
            link,
            events: events_in.clone(),
            announced: AtomicBool::new(false),
        });
        let handle = DriverHandle::new(commands, events, watch);
        let watch = handle.watch();
        thread::Builder::new()
            .name("sx126x-bridge".into())
            .spawn(move || drive(watch.as_ref(), kind, profile, &inbox, &events_in))
            .map_err(BridgeError::Thread)?;
        Ok(Self { handle, hello })
    }

    /// What the device said about itself.
    #[must_use]
    pub const fn hello(&self) -> &Hello {
        &self.hello
    }
}

/// Open a serial port for the bridge: the device ignores the rate, but the
/// read timeout decides how promptly the reader thread notices a frame.
///
/// # Errors
///
/// The port's own.
#[cfg(feature = "hardware")]
pub fn serial_port(path: &str) -> io::Result<serial2::SerialPort> {
    let mut port = serial2::SerialPort::open(path, BAUD)?;
    port.set_read_timeout(READ_SLICE)?;
    Ok(port)
}

#[cfg(feature = "hardware")]
impl BridgeRadio {
    /// Open the serial port at `path` and bring the radio up on it.
    ///
    /// # Errors
    ///
    /// See [`BridgeRadio::open`], and [`BridgeError::Port`] for the port.
    pub fn open_path(
        path: &str,
        options: &BridgeOptions,
        profile: PhyProfile,
    ) -> Result<Self, BridgeError> {
        let mut port = serial2::SerialPort::open(path, BAUD).map_err(BridgeError::Port)?;
        port.set_read_timeout(READ_SLICE)
            .map_err(BridgeError::Port)?;
        Self::open(Arc::new(port), options, profile)
    }
}

impl Radio for BridgeRadio {
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        self.handle.transmit(bytes)
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        self.handle.poll()
    }
}

/// The pre-flight: what the bridge can be asked before a protocol is put on
/// top of it.
///
/// When a board refuses to come up, `chip_failed` alone cannot say whether
/// the fault is the firmware, the USB link, the pin map, the module or the
/// driver. These steps separate them, in the order that each one depends on
/// the last, and they run against the reference device as readily as against
/// a board -- so the tool itself is tested before it meets hardware.
pub mod diagnostic {
    use std::time::{Duration, Instant};

    use embedded_hal::spi::Operation;

    use super::{CALL_TIMEOUT, Link, protocol};

    /// How many times a probe is repeated to say something about its spread.
    const PROBE_ROUNDS: usize = 20;
    /// `GetStatus`.
    const OP_GET_STATUS: u8 = 0xC0;
    /// `GetDeviceErrors`: a status byte, then the sixteen error bits.
    const OP_GET_DEVICE_ERRORS: u8 = 0x17;
    /// `GetIrqStatus`: a status byte, then the sixteen IRQ bits.
    const OP_GET_IRQ_STATUS: u8 = 0x12;

    /// One step: what it asked, what came back, and how long it took.
    #[derive(Debug, Clone)]
    pub struct Step {
        /// What was asked.
        pub name: &'static str,
        /// What came back, or why nothing did.
        pub outcome: Result<String, String>,
        /// How long the step took.
        pub took: Duration,
    }

    impl Step {
        /// Whether this step answered.
        #[must_use]
        pub const fn passed(&self) -> bool {
            self.outcome.is_ok()
        }
    }

    /// Every step, in the order they ran.
    #[derive(Debug, Clone)]
    pub struct Report {
        /// The steps.
        pub steps: Vec<Step>,
    }

    impl Report {
        /// Whether every step answered.
        #[must_use]
        pub fn passed(&self) -> bool {
            self.steps.iter().all(Step::passed)
        }

        /// The first step that did not, if any.
        #[must_use]
        pub fn first_failure(&self) -> Option<&Step> {
            self.steps.iter().find(|step| !step.passed())
        }
    }

    fn step(name: &'static str, body: impl FnOnce() -> Result<String, String>) -> Step {
        let started = Instant::now();
        let outcome = body();
        Step {
            name,
            outcome,
            took: started.elapsed(),
        }
    }

    /// One SPI command with a fixed-length answer, through the bridge.
    fn spi(link: &Link, opcode: u8, reads: usize) -> Result<Vec<u8>, String> {
        let mut answer = vec![0u8; reads];
        let ops = [
            Operation::Write(&[opcode]),
            Operation::Read(&mut answer[..]),
        ];
        let payload = protocol::encode_operations(&ops);
        let reply = link
            .call_checked(protocol::SPI, &payload, CALL_TIMEOUT)
            .map_err(|error| error.to_string())?;
        match reply.split_first() {
            Some((&protocol::STATUS_OK, bytes)) => Ok(bytes.to_vec()),
            Some((&status, _)) => Err(format!(
                "the device refused the transaction: status {status}"
            )),
            None => Err("empty reply".to_string()),
        }
    }

    /// Run the pre-flight. Every step is attempted; a failure does not stop
    /// the rest, because the later answers say which layer the first one
    /// belongs to.
    #[must_use]
    pub fn run(link: &Link) -> Report {
        let mut steps = speaks(link);
        steps.extend(answers(link));
        steps.extend(switches(link));
        Report { steps }
    }

    /// Is anything there, and does the chip come out of reset: the cable and
    /// the firmware, before any SPI command means anything.
    fn speaks(link: &Link) -> Vec<Step> {
        let mut steps = Vec::new();

        steps.push(step("hello", || {
            let hello = link
                .hello(Duration::from_secs(2))
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "protocol {}, firmware {}.{}, board {:#04x}, session {:#010x}, BUSY {}, DIO1 {}",
                hello.protocol,
                hello.firmware.0,
                hello.firmware.1,
                hello.board,
                hello.session,
                u8::from(hello.busy),
                u8::from(hello.dio1),
            ))
        }));

        steps.push(step("reset", || {
            link.call_checked(protocol::RESET, &[], CALL_TIMEOUT)
                .map_err(|error| error.to_string())
                .and_then(|body| match body.first() {
                    Some(&protocol::STATUS_OK) => Ok("NRESET pulsed, BUSY low".to_string()),
                    Some(&status) => Err(format!("status {status}")),
                    None => Err("empty reply".to_string()),
                })
        }));

        steps.push(step("busy low", || {
            link.call_checked(
                protocol::BUSY,
                &protocol::BUSY_LIMIT_MS.to_le_bytes(),
                CALL_TIMEOUT,
            )
            .map_err(|error| error.to_string())
            .and_then(|body| match body.first() {
                Some(&protocol::STATUS_OK) => Ok("the chip is ready for a command".to_string()),
                Some(&protocol::STATUS_TIMEOUT) => {
                    Err("BUSY stayed high: the module may be unpowered or unseated".to_string())
                }
                Some(&status) => Err(format!("status {status}")),
                None => Err("empty reply".to_string()),
            })
        }));

        steps
    }

    /// Does the chip answer for itself: SPI, the pin map, and what the chip
    /// says about its own health.
    fn answers(link: &Link) -> Vec<Step> {
        let mut steps = Vec::new();

        steps.push(step("get status", || {
            let bytes = spi(link, OP_GET_STATUS, 1)?;
            let status = bytes.first().copied().unwrap_or(0);
            // DS.SX1261-2 13.5.1: chip mode in bits 6:4, command status in 3:1.
            Ok(format!(
                "{status:#04x}: mode {:#x}, command status {:#x}",
                (status >> 4) & 0x7,
                (status >> 1) & 0x7
            ))
        }));

        steps.push(step("get device errors", || {
            let bytes = spi(link, OP_GET_DEVICE_ERRORS, 3)?;
            match bytes.as_slice() {
                [_, high, low] => {
                    let errors = u16::from_be_bytes([*high, *low]);
                    if errors == 0 {
                        Ok("none".to_string())
                    } else {
                        Err(format!("{errors:#06x}: the chip reports a fault"))
                    }
                }
                _ => Err("short reply".to_string()),
            }
        }));

        steps.push(step("get irq status", || {
            let bytes = spi(link, OP_GET_IRQ_STATUS, 3)?;
            match bytes.as_slice() {
                [_, high, low] => Ok(format!("{:#06x}", u16::from_be_bytes([*high, *low]))),
                _ => Err("short reply".to_string()),
            }
        }));

        steps.push(step("dio1 reads low", || {
            if link.dio1_level().map_err(|error| error.to_string())? {
                Err(
                    "DIO1 is high with no IRQ set: the line may be misassigned or stuck"
                        .to_string(),
                )
            } else {
                Ok("no interrupt pending".to_string())
            }
        }));

        steps
    }

    /// The board around the chip, and what the cable costs.
    fn switches(link: &Link) -> Vec<Step> {
        let mut steps = Vec::new();

        steps.push(step("rf switch", || {
            for (name, mode) in [
                ("receive", protocol::RF_RX),
                ("transmit", protocol::RF_TX),
                ("off", protocol::RF_OFF),
            ] {
                let body = link
                    .call_checked(protocol::RF, &[mode], CALL_TIMEOUT)
                    .map_err(|error| format!("{name}: {error}"))?;
                if body.first() != Some(&protocol::STATUS_OK) {
                    return Err(format!("{name}: status {:?}", body.first()));
                }
            }
            Ok("receive, transmit and off all accepted".to_string())
        }));

        steps.push(step("round trip", || {
            let mut took = Vec::with_capacity(PROBE_ROUNDS);
            for _ in 0..PROBE_ROUNDS {
                let started = Instant::now();
                spi(link, OP_GET_STATUS, 1)?;
                took.push(started.elapsed());
            }
            took.sort_unstable();
            let at = |percent: usize| took[(took.len() * percent / 100).min(took.len() - 1)];
            Ok(format!(
                "{PROBE_ROUNDS} status reads: median {:?}, p95 {:?}, max {:?}",
                at(50),
                at(95),
                took[took.len() - 1]
            ))
        }));

        steps
    }
}

/// The device side of the protocol, on the virtual chip.
///
/// This is the reference the firmware mirrors: the same requests, the same
/// replies, the same events, with the chip model where the silicon would be.
/// The tests bring a [`BridgeRadio`] up against it over a pseudo-terminal
/// pair, so what is pinned here is everything but the wires.
pub mod device {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread;
    use std::time::Duration;

    use super::protocol::{self, Hello, Op};
    use super::{BridgeError, Port, READ_SLICE};
    use crate::infrastructure::rnode::kiss;
    use crate::infrastructure::sx126x::Chip;

    /// Firmware version the reference device reports.
    pub const FIRMWARE: (u8, u8) = (0, 2);
    /// How often the edge thread looks at the chip.
    const EDGE_SLICE: Duration = Duration::from_millis(20);

    /// A session id from the host's entropy, never zero: a board draws one
    /// at every boot, so that a host can tell a restart from silence.
    fn session_id() -> u32 {
        use std::hash::{BuildHasher, Hasher};
        let drawn = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        #[allow(clippy::cast_possible_truncation)]
        let drawn = drawn as u32;
        drawn | 1
    }

    struct Device {
        port: Arc<dyn Port>,
        chip: Chip,
        /// Drawn once per served chip, as a board draws one per boot.
        session: u32,
        events_on: AtomicBool,
        /// The pending IRQ has been announced; announce the next one only
        /// after DIO1 has fallen.
        reported: AtomicBool,
        alive: AtomicBool,
        writes: Mutex<()>,
    }

    /// Serve `chip` on `port` until the port goes away. Returns at once;
    /// the device runs on its own two threads.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Thread`] if a thread could not be spawned.
    pub fn serve(port: Arc<dyn Port>, chip: Chip) -> Result<(), BridgeError> {
        let device = Arc::new(Device {
            port,
            chip,
            session: session_id(),
            events_on: AtomicBool::new(false),
            reported: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            writes: Mutex::new(()),
        });
        let pump = Arc::clone(&device);
        thread::Builder::new()
            .name("bridge-device-port".into())
            .spawn(move || pump.pump())
            .map_err(BridgeError::Thread)?;
        thread::Builder::new()
            .name("bridge-device-dio1".into())
            .spawn(move || device.edges())
            .map_err(BridgeError::Thread)?;
        Ok(())
    }

    impl Device {
        fn send(&self, command: u8, payload: &[u8]) {
            let _guard = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = self.port.write_all(&kiss::encode(command, payload));
        }

        fn status(&self, command: u8, status: u8) {
            self.send(command | protocol::REPLY, &[status]);
        }

        fn pump(&self) {
            let mut deframer = kiss::Deframer::new(protocol::FRAME_LIMIT);
            let mut buffer = [0u8; 1_024];
            let mut frames = Vec::new();
            while self.alive.load(Ordering::Acquire) {
                match self.port.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        deframer.push(&buffer[..n], &mut frames);
                        for frame in frames.drain(..) {
                            self.handle(&frame);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
            self.alive.store(false, Ordering::Release);
        }

        fn edges(&self) {
            while self.alive.load(Ordering::Acquire) {
                let _ = self.chip.wait_for_activity(EDGE_SLICE);
                let pending = self.chip.irq_pending();
                if pending {
                    if self.events_on.load(Ordering::Acquire)
                        && !self.reported.swap(true, Ordering::AcqRel)
                    {
                        // The level as it reads now, which the host uses to
                        // drop a notice the chip has already withdrawn.
                        self.send(protocol::EVENT_DIO1, &[u8::from(self.chip.irq_pending())]);
                    }
                    // The chip's wait returns at once while the IRQ stands;
                    // give the host time to service it.
                    thread::sleep(READ_SLICE);
                } else {
                    self.reported.store(false, Ordering::Release);
                }
            }
        }

        fn handle(&self, frame: &[u8]) {
            let Some((&command, payload)) = frame.split_first() else {
                return;
            };
            match command {
                protocol::HELLO => {
                    // Clear first, read second: an edge between the two is
                    // then reported as an event rather than lost, which is
                    // the ordering the firmware must also use.
                    self.events_on.store(true, Ordering::Release);
                    self.reported.store(false, Ordering::Release);
                    let pending = self.chip.irq_pending();
                    self.reported.store(pending, Ordering::Release);
                    let hello = Hello {
                        protocol: protocol::VERSION,
                        firmware: FIRMWARE,
                        board: protocol::BOARD_VIRTUAL,
                        session: self.session,
                        busy: false,
                        dio1: pending,
                    };
                    self.send(protocol::HELLO | protocol::REPLY, &hello.encode());
                }
                protocol::RESET => {
                    self.chip.reset();
                    self.status(protocol::RESET, protocol::STATUS_OK);
                }
                protocol::BUSY => {
                    if payload.len() == 2 {
                        self.chip.wait_on_busy();
                        self.status(protocol::BUSY, protocol::STATUS_OK);
                    } else {
                        self.status(protocol::BUSY, protocol::STATUS_BAD_REQUEST);
                    }
                }
                protocol::PINS => {
                    let level = u8::from(self.chip.irq_pending());
                    self.send(protocol::PINS | protocol::REPLY, &[0, level]);
                }
                protocol::RF => {
                    let status = match payload {
                        [protocol::RF_OFF | protocol::RF_RX | protocol::RF_TX] => {
                            protocol::STATUS_OK
                        }
                        _ => protocol::STATUS_BAD_REQUEST,
                    };
                    self.status(protocol::RF, status);
                }
                protocol::SPI => self.spi(payload),
                protocol::EVENTS => {
                    if let [on] = payload {
                        self.events_on.store(*on != 0, Ordering::Release);
                        self.reported.store(false, Ordering::Release);
                        let pending = self.chip.irq_pending();
                        self.reported.store(pending, Ordering::Release);
                        self.send(protocol::EVENTS | protocol::REPLY, &[u8::from(pending)]);
                    } else {
                        self.status(protocol::EVENTS, protocol::STATUS_BAD_REQUEST);
                    }
                }
                other => self.send(
                    protocol::EVENT_ERROR,
                    &[protocol::STATUS_UNKNOWN_COMMAND, other],
                ),
            }
        }

        /// One transaction: every write is command bytes, every read is
        /// filled from the command's reply, in order -- the virtual bus's
        /// rule, now on the far side of the wire.
        fn spi(&self, payload: &[u8]) {
            let Some(ops) = protocol::decode_operations(payload) else {
                self.status(protocol::SPI, protocol::STATUS_BAD_REQUEST);
                return;
            };
            let wanted: usize = ops
                .iter()
                .filter(|op| matches!(op.kind, protocol::OP_READ | protocol::OP_TRANSFER))
                .map(|op| op.length)
                .sum();
            if wanted + 1 > protocol::FRAME_LIMIT {
                self.status(protocol::SPI, protocol::STATUS_TOO_LONG);
                return;
            }
            let command: Vec<u8> = ops.iter().flat_map(|op| op.bytes.iter().copied()).collect();
            self.chip.wait_on_busy();
            let answer = match command.split_first() {
                Some((&opcode, params)) => self.chip.execute(opcode, params),
                None => Vec::new(),
            };
            let mut answer = answer.into_iter();
            let mut reply = vec![protocol::STATUS_OK];
            for Op { kind, length, .. } in &ops {
                if matches!(*kind, protocol::OP_READ | protocol::OP_TRANSFER) {
                    reply.extend((0..*length).map(|_| answer.next().unwrap_or(0)));
                }
            }
            self.send(protocol::SPI | protocol::REPLY, &reply);
        }
    }
}
