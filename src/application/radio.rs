//! The radio as the protocol sees it.
//!
//! A node hands bytes to whatever carries them and gets bytes back, together
//! with what the medium said about each frame. Nothing above this seam knows
//! whether the medium is the channel emulator's socket, a virtual chip under a
//! real driver, or hardware. That is the point (D16): the container suite and
//! the vessel run the same node against different adapters of this trait.

use core::fmt;

/// The physical-layer profile a radio is configured with.
///
/// One profile describes one channel: everybody on it must agree on every
/// field, and the airtime and sensitivity figures in `sim` describe this one.
/// It is versioned by `name` so that an emulator run and a hardware run are
/// only ever compared on the same profile, never on somebody's defaults; the
/// contract vectors in `tests/phy_profile.rs` pin every field.
// A configuration record: every flag is a chip parameter bit, and an enum per
// bit would only rename true and false.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhyProfile {
    /// The profile's name and version, logged by every process that uses it.
    pub name: &'static str,
    /// `LoRa` spreading factor, 5 to 12.
    pub spreading_factor: u8,
    /// Channel bandwidth in hertz.
    pub bandwidth_hz: u32,
    /// Coding rate as the denominator of 4/x, 5 to 8.
    pub coding_rate_denominator: u8,
    /// Preamble length in symbols.
    pub preamble_symbols: u16,
    /// Explicit header: the frame carries its own length, coding rate and
    /// CRC presence.
    pub explicit_header: bool,
    /// Payload CRC on the air.
    pub crc_on: bool,
    /// Inverted IQ, as gateways use for downlink; a fleet of peers does not.
    pub iq_inverted: bool,
    /// Low-data-rate optimisation. The chip needs it once a symbol passes
    /// about 16 ms, which at 125 kHz means SF11 and SF12 and nothing lower.
    pub low_data_rate_optimize: bool,
    /// Carrier frequency in hertz.
    pub frequency_hz: u32,
    /// Transmit power in dBm.
    pub tx_power_dbm: i8,
    /// `LoRa` sync word in the legacy single-byte form; `0x12` is the private
    /// network value every `SX127x` and `SX126x` understands.
    pub sync_word: u8,
    /// Listen continuously rather than with a receive timeout.
    pub rx_continuous: bool,
    /// Channel activity detection: symbols to examine.
    pub cad_symbols: u8,
    /// Channel activity detection: peak detection threshold, `SX126x` units.
    pub cad_detect_peak: u8,
    /// Channel activity detection: minimum detection threshold, `SX126x` units.
    pub cad_detect_min: u8,
    /// How many preamble symbols a receiver must hear to lock on. A model
    /// parameter, conservative against `LoRaSim`'s five, until hardware says
    /// otherwise (D16).
    pub preamble_symbols_to_lock: u16,
    /// How much stronger a frame must be than an overlapping one to be
    /// demodulated over it, in dB. A model parameter (D14, D16).
    pub capture_threshold_db: u8,
}

impl PhyProfile {
    /// The profile the rest of the crate assumes: EU 868 MHz, SF10, 125 kHz,
    /// CR 4/5, eight preamble symbols, explicit header, CRC, 14 dBm, private
    /// sync word, continuous receive.
    #[must_use]
    pub const fn eu868_sf10() -> Self {
        Self {
            name: "eu868-sf10-v1",
            spreading_factor: 10,
            bandwidth_hz: 125_000,
            coding_rate_denominator: 5,
            preamble_symbols: 8,
            explicit_header: true,
            crc_on: true,
            iq_inverted: false,
            low_data_rate_optimize: false,
            frequency_hz: 868_100_000,
            tx_power_dbm: 14,
            sync_word: 0x12,
            rx_continuous: true,
            cad_symbols: 8,
            cad_detect_peak: 23,
            cad_detect_min: 10,
            preamble_symbols_to_lock: 6,
            capture_threshold_db: 6,
        }
    }
}

/// A frame the radio received intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The payload, exactly as it was on the air.
    pub bytes: Vec<u8>,
    /// Received signal strength in dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio in dB.
    pub snr_db: i8,
}

/// Something the radio has to tell the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioEvent {
    /// A frame arrived and passed its integrity check.
    Received(Received),
    /// A frame arrived but failed its CRC: somebody spoke, and what they said
    /// is gone.
    CrcError {
        /// Received signal strength in dBm of the garbled frame.
        rssi_dbm: i16,
    },
    /// A diagnostic from the adapter: the body of a JSON object, for the
    /// node's log.
    Note(String),
}

/// Why a frame could not be handed to the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioError {
    /// Longer than the radio's buffer.
    TooLong {
        /// Bytes offered.
        len: usize,
        /// Bytes the radio takes at most.
        max: usize,
    },
    /// The radio is gone: its driver stopped or its medium closed.
    Offline,
}

impl fmt::Display for RadioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { len, max } => {
                write!(f, "frame of {len} bytes exceeds the radio's {max}")
            }
            Self::Offline => f.write_str("radio offline"),
        }
    }
}

impl std::error::Error for RadioError {}

/// What a node needs from a radio.
pub trait Radio {
    /// Put a frame on the air. Returns once the radio has accepted it; the
    /// airtime is spent by the radio, not waited for here.
    ///
    /// # Errors
    ///
    /// [`RadioError::TooLong`] if the frame exceeds the radio's buffer,
    /// [`RadioError::Offline`] if the radio can no longer be reached.
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError>;

    /// The next thing the radio has to say, if anything. Never blocks.
    fn poll(&mut self) -> Option<RadioEvent>;
}
