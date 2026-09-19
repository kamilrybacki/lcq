//! An `RNode` as a radio: a `LoRa` modem on the other end of a serial port.
//!
//! `RNode` firmware (Mark Qvist; the community fork for more boards) turns an
//! SX1262 or SX1276 development board into a modem the host drives with
//! KISS-framed commands over USB. It is the cheapest way to put this node on
//! real hardware: nothing to flash but a released firmware, nothing to wire.
//! The host protocol is small and observed on the wire (D20): a command byte
//! at the head of each KISS frame, configuration echoed back as confirmation,
//! `RADIO_STATE` echoed as `1` once the radio is on, and for every received
//! packet a triplet -- RSSI, SNR, then the bytes.
//!
//! The protocol lives in [`RNodeLink`], sans I/O, so the same code runs on a
//! pseudo-terminal in a test and on `/dev/ttyACM0` at sea; [`RNodeRadio`]
//! puts a serial port under it.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::application::{PhyProfile, Radio, RadioError, RadioEvent, Received};

/// KISS framing: a frame is delimited by `FEND`, and the two special bytes
/// are escaped inside it.
pub mod kiss {
    /// Frame end.
    pub const FEND: u8 = 0xC0;
    /// Frame escape.
    pub const FESC: u8 = 0xDB;
    /// Transposed frame end.
    pub const TFEND: u8 = 0xDC;
    /// Transposed frame escape.
    pub const TFESC: u8 = 0xDD;

    /// One frame: the command byte, then the payload, escaped and delimited.
    #[must_use]
    pub fn encode(command: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 4);
        out.push(FEND);
        out.push(command);
        for &byte in payload {
            match byte {
                FEND => out.extend_from_slice(&[FESC, TFEND]),
                FESC => out.extend_from_slice(&[FESC, TFESC]),
                other => out.push(other),
            }
        }
        out.push(FEND);
        out
    }

    /// Reassembles frames from a byte stream, however it is chopped up.
    #[derive(Debug, Default)]
    pub struct Deframer {
        frame: Vec<u8>,
        in_frame: bool,
        escaped: bool,
        /// The most bytes a frame may hold before it is discarded as noise.
        limit: usize,
    }

    impl Deframer {
        /// A deframer that refuses frames longer than `limit` bytes.
        #[must_use]
        pub const fn new(limit: usize) -> Self {
            Self {
                frame: Vec::new(),
                in_frame: false,
                escaped: false,
                limit,
            }
        }

        /// Feed bytes; every completed frame (command byte first) is pushed
        /// to `out`.
        pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Vec<u8>>) {
            for &byte in bytes {
                if byte == FEND {
                    if self.in_frame && !self.frame.is_empty() {
                        out.push(std::mem::take(&mut self.frame));
                    }
                    self.frame.clear();
                    self.in_frame = true;
                    self.escaped = false;
                    continue;
                }
                if !self.in_frame {
                    continue;
                }
                let decoded = if self.escaped {
                    self.escaped = false;
                    match byte {
                        TFEND => FEND,
                        TFESC => FESC,
                        other => other,
                    }
                } else if byte == FESC {
                    self.escaped = true;
                    continue;
                } else {
                    byte
                };
                if self.frame.len() >= self.limit {
                    // Noise, or a firmware speaking something else: drop it.
                    self.frame.clear();
                    self.in_frame = false;
                    continue;
                }
                self.frame.push(decoded);
            }
        }
    }
}

/// The command bytes of the `RNode` host protocol, as observed on the wire.
pub mod command {
    /// A packet, to or from the air.
    pub const DATA: u8 = 0x00;
    /// Carrier frequency, `u32` big-endian hertz.
    pub const FREQUENCY: u8 = 0x01;
    /// Bandwidth, `u32` big-endian hertz.
    pub const BANDWIDTH: u8 = 0x02;
    /// Transmit power, dBm.
    pub const TXPOWER: u8 = 0x03;
    /// Spreading factor.
    pub const SPREADING_FACTOR: u8 = 0x04;
    /// Coding rate, the denominator of 4/x.
    pub const CODING_RATE: u8 = 0x05;
    /// Radio on (1) or off (0); echoed once applied.
    pub const RADIO_STATE: u8 = 0x06;
    /// Detect: request `0x73`, response `0x46`.
    pub const DETECT: u8 = 0x08;
    /// The device can take another packet.
    pub const READY: u8 = 0x0F;
    /// RSSI of the packet that follows, `dBm + 157`.
    pub const STAT_RSSI: u8 = 0x23;
    /// SNR of the packet that follows, `dB * 4` as `i8`.
    pub const STAT_SNR: u8 = 0x24;
    /// Platform probe.
    pub const PLATFORM: u8 = 0x48;
    /// MCU probe.
    pub const MCU: u8 = 0x49;
    /// Firmware version, major then minor.
    pub const FW_VERSION: u8 = 0x50;
    /// An error code from the device.
    pub const ERROR: u8 = 0x90;
}

/// The detect request byte.
pub const DETECT_REQUEST: u8 = 0x73;
/// The detect response byte.
pub const DETECT_RESPONSE: u8 = 0x46;
/// RSSI on the wire is offset by this: `dBm = raw - 157`.
pub const RSSI_OFFSET: i16 = 157;
/// The most an `RNode` carries in one packet before it splits it.
pub const MAX_PACKET_BYTES: usize = 254;
/// The most a host frame may hold: a packet plus room for escapes.
const FRAME_LIMIT: usize = 600;
/// Baud rate of every `RNode`'s USB serial port.
pub const BAUD: u32 = 115_200;
/// How long to wait for the device to answer the detect probe and come up.
pub const BRING_UP_TIMEOUT: Duration = Duration::from_secs(5);

/// The `RNode` host protocol, sans I/O: feed it bytes from the port, write out
/// what it queues, poll it for events.
#[derive(Debug)]
pub struct RNodeLink {
    profile: PhyProfile,
    deframer: kiss::Deframer,
    outbound: Vec<u8>,
    events: VecDeque<RadioEvent>,
    detected: bool,
    online: bool,
    firmware: Option<(u8, u8)>,
    pending_rssi: Option<i16>,
    pending_snr: Option<i8>,
}

impl RNodeLink {
    /// A link that will configure the device for `profile`.
    #[must_use]
    pub fn new(profile: PhyProfile) -> Self {
        Self {
            profile,
            deframer: kiss::Deframer::new(FRAME_LIMIT),
            outbound: Vec::new(),
            events: VecDeque::new(),
            detected: false,
            online: false,
            firmware: None,
            pending_rssi: None,
            pending_snr: None,
        }
    }

    fn queue(&mut self, command: u8, payload: &[u8]) {
        self.outbound
            .extend_from_slice(&kiss::encode(command, payload));
    }

    /// Queue the whole bring-up: detect, probes, configuration, radio on.
    pub fn start(&mut self) {
        self.queue(command::DETECT, &[DETECT_REQUEST]);
        self.queue(command::FW_VERSION, &[0x00]);
        self.queue(command::PLATFORM, &[0x00]);
        self.queue(command::MCU, &[0x00]);
        let profile = self.profile;
        self.queue(command::FREQUENCY, &profile.frequency_hz.to_be_bytes());
        self.queue(command::BANDWIDTH, &profile.bandwidth_hz.to_be_bytes());
        self.queue(command::TXPOWER, &[profile.tx_power_dbm.to_le_bytes()[0]]);
        self.queue(command::SPREADING_FACTOR, &[profile.spreading_factor]);
        self.queue(command::CODING_RATE, &[profile.coding_rate_denominator]);
        self.queue(command::RADIO_STATE, &[0x01]);
    }

    /// Bytes waiting for the port. Empties the queue.
    pub fn take_outbound(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outbound)
    }

    /// Whether the device answered the detect probe.
    #[must_use]
    pub const fn detected(&self) -> bool {
        self.detected
    }

    /// Whether the device reported its radio on.
    #[must_use]
    pub const fn online(&self) -> bool {
        self.online
    }

    /// Firmware version, once probed.
    #[must_use]
    pub const fn firmware(&self) -> Option<(u8, u8)> {
        self.firmware
    }

    /// Bytes read from the port.
    pub fn on_serial(&mut self, bytes: &[u8]) {
        let mut frames = Vec::new();
        self.deframer.push(bytes, &mut frames);
        for frame in frames {
            self.on_frame(&frame);
        }
    }

    fn on_frame(&mut self, frame: &[u8]) {
        let Some((&command, payload)) = frame.split_first() else {
            return;
        };
        match command {
            command::DETECT => {
                if payload.first() == Some(&DETECT_RESPONSE) {
                    self.detected = true;
                }
            }
            command::FW_VERSION => {
                if let [major, minor, ..] = payload {
                    self.firmware = Some((*major, *minor));
                    // Logged so an operator can pin what firmware the fleet
                    // runs; the device is in the trusted computing base.
                    self.note(format!(
                        "\"event\":\"rnode_firmware\",\"major\":{major},\"minor\":{minor}"
                    ));
                }
            }
            command::RADIO_STATE => {
                let was = self.online;
                self.online = payload.first() == Some(&1);
                if self.online != was {
                    self.note(format!(
                        "\"event\":\"rnode_radio\",\"online\":{}",
                        self.online
                    ));
                }
            }
            command::STAT_RSSI => {
                if let Some(&raw) = payload.first() {
                    self.pending_rssi = Some(i16::from(raw) - RSSI_OFFSET);
                }
            }
            command::STAT_SNR => {
                if let Some(&raw) = payload.first() {
                    self.pending_snr = Some(i8::from_le_bytes([raw]) / 4);
                }
            }
            command::DATA => {
                let rssi_dbm = self.pending_rssi.take().unwrap_or(0);
                let snr_db = self.pending_snr.take().unwrap_or(0);
                self.events.push_back(RadioEvent::Received(Received {
                    bytes: payload.to_vec(),
                    rssi_dbm,
                    snr_db,
                }));
            }
            command::ERROR => {
                let code = payload.first().copied().unwrap_or(0);
                self.note(format!("\"event\":\"rnode_error\",\"code\":{code}"));
            }
            // Echoed configuration, READY, channel and battery statistics:
            // nothing the protocol needs.
            _ => {}
        }
    }

    fn note(&mut self, body: String) {
        self.events.push_back(RadioEvent::Note(body));
    }

    /// Queue a packet for the air.
    ///
    /// # Errors
    ///
    /// [`RadioError::TooLong`] past what one packet carries,
    /// [`RadioError::Offline`] while the radio has not reported itself on.
    pub fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(RadioError::TooLong {
                len: bytes.len(),
                max: MAX_PACKET_BYTES,
            });
        }
        if !self.online {
            return Err(RadioError::Offline);
        }
        self.queue(command::DATA, bytes);
        Ok(())
    }

    /// The next event, if any.
    pub fn poll(&mut self) -> Option<RadioEvent> {
        self.events.pop_front()
    }
}

/// Why the `RNode` could not be brought up.
#[derive(Debug)]
pub enum RNodeError {
    /// The port could not be opened or configured.
    Port(io::Error),
    /// The device never answered the detect probe.
    NotDetected,
    /// The device answered but never reported its radio on -- an unverified
    /// firmware refuses to, and says so with `RADIO_STATE 0`.
    RadioOff,
}

impl fmt::Display for RNodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port(error) => write!(f, "serial port: {error}"),
            Self::NotDetected => f.write_str("no RNode answered the detect probe"),
            Self::RadioOff => f.write_str("the RNode did not turn its radio on"),
        }
    }
}

impl std::error::Error for RNodeError {}

/// A byte pipe to the device: what a serial port is to this adapter.
pub trait Port: Send + 'static {
    /// Read whatever is available, blocking at most briefly; `Ok(0)` on
    /// nothing yet.
    ///
    /// # Errors
    ///
    /// The port's own.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
    /// Write all of it.
    ///
    /// # Errors
    ///
    /// The port's own.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// An `RNode` behind the [`Radio`] seam.
pub struct RNodeRadio {
    link: Arc<Mutex<RNodeLink>>,
    to_port: Sender<Vec<u8>>,
}

impl RNodeRadio {
    /// Bring an `RNode` up on `port` for `profile`: detect it, configure it,
    /// turn its radio on, and wait for it to say so.
    ///
    /// # Errors
    ///
    /// See [`RNodeError`].
    pub fn open<P: Port>(port: P, profile: PhyProfile) -> Result<Self, RNodeError> {
        let link = Arc::new(Mutex::new(RNodeLink::new(profile)));
        let (to_port, from_host) = channel::<Vec<u8>>();
        let pump = Arc::clone(&link);
        thread::Builder::new()
            .name("rnode-port".into())
            .spawn(move || pump_port(port, &pump, &from_host))
            .map_err(RNodeError::Port)?;
        {
            let mut guard = lock(&link);
            guard.start();
            let bytes = guard.take_outbound();
            let _ = to_port.send(bytes);
        }
        let deadline = Instant::now() + BRING_UP_TIMEOUT;
        loop {
            let (detected, online) = {
                let guard = lock(&link);
                (guard.detected(), guard.online())
            };
            if online {
                return Ok(Self { link, to_port });
            }
            if Instant::now() >= deadline {
                return Err(if detected {
                    RNodeError::RadioOff
                } else {
                    RNodeError::NotDetected
                });
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The link, for whoever wants to look at it.
    #[must_use]
    pub fn link(&self) -> Arc<Mutex<RNodeLink>> {
        Arc::clone(&self.link)
    }
}

fn lock(link: &Mutex<RNodeLink>) -> std::sync::MutexGuard<'_, RNodeLink> {
    link.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The port thread: bytes in go to the link, bytes queued go out.
fn pump_port<P: Port>(mut port: P, link: &Mutex<RNodeLink>, from_host: &Receiver<Vec<u8>>) {
    let mut buffer = [0u8; 1024];
    loop {
        while let Ok(bytes) = from_host.try_recv() {
            if port.write_all(&bytes).is_err() {
                return;
            }
        }
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(n) => {
                let mut guard = lock(link);
                guard.on_serial(&buffer[..n]);
                let pending = guard.take_outbound();
                drop(guard);
                if !pending.is_empty() && port.write_all(&pending).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

impl Radio for RNodeRadio {
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        let mut guard = lock(&self.link);
        guard.transmit(bytes)?;
        let out = guard.take_outbound();
        drop(guard);
        self.to_port.send(out).map_err(|_| RadioError::Offline)
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        lock(&self.link).poll()
    }
}

#[cfg(feature = "hardware")]
impl Port for serial2::SerialPort {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        serial2::SerialPort::read(self, buffer)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        serial2::SerialPort::write_all(self, bytes)
    }
}

#[cfg(feature = "hardware")]
impl RNodeRadio {
    /// Open the serial port at `path` at the `RNode` baud rate and bring the
    /// device up.
    ///
    /// # Errors
    ///
    /// See [`RNodeError`].
    pub fn open_path(path: &str, profile: PhyProfile) -> Result<Self, RNodeError> {
        let mut port = serial2::SerialPort::open(path, BAUD).map_err(RNodeError::Port)?;
        port.set_read_timeout(Duration::from_millis(20))
            .map_err(RNodeError::Port)?;
        Self::open(port, profile)
    }
}
