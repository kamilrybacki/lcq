//! The channel emulator's socket, and the frame it delivers.
//!
//! Node to hub is the raw frame behind a little-endian `u32` length. Hub to
//! node is the same length prefix around a [`Delivery`]: what arrived, how
//! strong it was, how long it occupied the channel, and whether it survived
//! intact. Both radios that attach to the hub -- the plain socket and the
//! virtual SX1262 -- read the same delivery, so the medium's verdict is written
//! down once and interpreted twice.

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

/// The largest frame the socket will read; anything bigger is a broken peer.
pub const MAX_SOCKET_FRAME_BYTES: usize = 4096;
/// First byte of every delivery: the format version.
pub const DELIVERY_TAG: u8 = 0x02;
/// Bytes before the payload in an encoded delivery.
pub const DELIVERY_HEADER_BYTES: usize = 9;

/// How often, and how long, a node retries the hub before giving up.
const CONNECT_ATTEMPTS: u32 = 300;
const CONNECT_PAUSE: Duration = Duration::from_millis(100);

/// What a receiver made of a frame it locked onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Intact.
    Decoded,
    /// The header failed its CRC: nothing was received, only heard.
    HeaderError,
    /// The payload failed its CRC: the bytes are noise.
    CrcError,
}

impl Verdict {
    const fn byte(self) -> u8 {
        match self {
            Self::Decoded => 0,
            Self::HeaderError => 1,
            Self::CrcError => 2,
        }
    }

    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Decoded),
            1 => Some(Self::HeaderError),
            2 => Some(Self::CrcError),
            _ => None,
        }
    }
}

/// What the medium handed one receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The payload as this receiver got it -- garbled unless the verdict is
    /// `Decoded`, empty for a header error.
    pub bytes: Vec<u8>,
    /// Received power in dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio in dB.
    pub snr_db: i8,
    /// How long the frame occupied the channel, in (scaled) milliseconds.
    pub airtime_ms: u32,
    /// What the receiver made of it.
    pub verdict: Verdict,
}

impl Delivery {
    /// The wire form: tag, RSSI, SNR, airtime, verdict, payload.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DELIVERY_HEADER_BYTES + self.bytes.len());
        out.push(DELIVERY_TAG);
        out.extend_from_slice(&self.rssi_dbm.to_le_bytes());
        out.push(self.snr_db.to_le_bytes()[0]);
        out.extend_from_slice(&self.airtime_ms.to_le_bytes());
        out.push(self.verdict.byte());
        out.extend_from_slice(&self.bytes);
        out
    }

    /// Parse the wire form; `None` for anything that is not one.
    #[must_use]
    pub fn decode(encoded: &[u8]) -> Option<Self> {
        if encoded.len() < DELIVERY_HEADER_BYTES || encoded[0] != DELIVERY_TAG {
            return None;
        }
        let rssi_dbm = i16::from_le_bytes([encoded[1], encoded[2]]);
        let snr_db = i8::from_le_bytes([encoded[3]]);
        let airtime_ms = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]);
        let verdict = Verdict::from_byte(encoded[8])?;
        Some(Self {
            bytes: encoded[DELIVERY_HEADER_BYTES..].to_vec(),
            rssi_dbm,
            snr_db,
            airtime_ms,
            verdict,
        })
    }
}

/// Failure to join the emulated channel.
#[derive(Debug)]
pub enum HubError {
    /// No connection after every retry.
    Unreachable(String),
    /// Connected, but the index handshake did not go through.
    Handshake(std::io::Error),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(address) => write!(f, "hub at {address} never became reachable"),
            Self::Handshake(error) => write!(f, "hub handshake failed: {error}"),
        }
    }
}

impl std::error::Error for HubError {}

/// The writing half of a node's connection.
pub struct HubWriter {
    stream: TcpStream,
}

impl HubWriter {
    /// Send one frame to the medium. A write that fails is a channel that is
    /// gone; the node keeps its schedule and whoever watches sees the silence.
    pub fn send(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes();
        let _ = self.stream.write_all(&length);
        let _ = self.stream.write_all(bytes);
        let _ = self.stream.flush();
    }
}

/// Write one delivery to a node, the way the hub does.
///
/// # Errors
///
/// Whatever the socket reports; the caller decides whether a receiver that
/// cannot be written to is one that has left.
pub fn write_delivery(stream: &mut TcpStream, delivery: &Delivery) -> std::io::Result<()> {
    let encoded = delivery.encode();
    let length = u32::try_from(encoded.len()).unwrap_or(0).to_le_bytes();
    stream.write_all(&length)?;
    stream.write_all(&encoded)?;
    stream.flush()
}

/// A connected node's end of the emulated channel, with its reader on a
/// thread of its own.
pub struct HubSocket {
    writer: HubWriter,
    inbox: Receiver<Delivery>,
}

impl HubSocket {
    /// Join the channel as member `index`.
    ///
    /// A node may well be powered up before whatever carries its traffic is
    /// reachable, so connecting is retried for thirty seconds rather than
    /// failing at once.
    ///
    /// # Errors
    ///
    /// [`HubError::Unreachable`] after the last retry, [`HubError::Handshake`]
    /// if the index could not be sent.
    pub fn connect(address: &str, index: usize) -> Result<Self, HubError> {
        let mut stream = None;
        for _ in 0..CONNECT_ATTEMPTS {
            if let Ok(socket) = TcpStream::connect(address) {
                stream = Some(socket);
                break;
            }
            thread::sleep(CONNECT_PAUSE);
        }
        let mut stream = stream.ok_or_else(|| HubError::Unreachable(address.to_string()))?;
        stream
            .write_all(&u16::try_from(index).unwrap_or(0).to_le_bytes())
            .and_then(|()| stream.flush())
            .map_err(HubError::Handshake)?;

        let reader = stream.try_clone().map_err(HubError::Handshake)?;
        let (sender, inbox) = channel();
        thread::spawn(move || read_deliveries(reader, &sender));
        Ok(Self {
            writer: HubWriter { stream },
            inbox,
        })
    }

    /// Send one frame to the medium.
    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.send(bytes);
    }

    /// The next delivery, if one has arrived. Never blocks.
    pub fn poll(&mut self) -> Option<Delivery> {
        self.inbox.try_recv().ok()
    }

    /// Split into the writer and the stream of deliveries, for adapters that
    /// run each on a thread of its own.
    #[must_use]
    pub fn into_parts(self) -> (HubWriter, Receiver<Delivery>) {
        (self.writer, self.inbox)
    }
}

/// Read length-prefixed deliveries until the socket closes or misbehaves.
fn read_deliveries(mut reader: TcpStream, sender: &Sender<Delivery>) {
    let mut length = [0u8; 4];
    while reader.read_exact(&mut length).is_ok() {
        let size = u32::from_le_bytes(length) as usize;
        if size == 0 || size > MAX_SOCKET_FRAME_BYTES {
            return;
        }
        let mut encoded = vec![0u8; size];
        if reader.read_exact(&mut encoded).is_err() {
            return;
        }
        let Some(delivery) = Delivery::decode(&encoded) else {
            // A peer speaking another format is not one worth listening to.
            return;
        };
        if sender.send(delivery).is_err() {
            return;
        }
    }
}
