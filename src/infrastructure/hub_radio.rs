//! The channel emulator's socket as a radio: what the container suite ran on
//! before D16, behind the seam it now shares with the virtual SX1262.

use crate::application::{Radio, RadioError, RadioEvent, Received};
use crate::infrastructure::hub::{HubError, HubSocket, MAX_SOCKET_FRAME_BYTES};

/// A node attached straight to `lcq-hub`.
///
/// The hub models the channel: airtime, collisions, reach, loss. This adapter
/// adds nothing -- a frame is on the air the moment it is written, and a
/// delivery is a reception. Half-duplex is not enforced here; the hub's
/// collision rule approximates it, and the virtual SX1262 enforces it.
pub struct HubRadio {
    socket: HubSocket,
}

impl HubRadio {
    /// Join the channel as member `index`.
    ///
    /// # Errors
    ///
    /// See [`HubSocket::connect`].
    pub fn connect(address: &str, index: usize) -> Result<Self, HubError> {
        Ok(Self {
            socket: HubSocket::connect(address, index)?,
        })
    }
}

impl Radio for HubRadio {
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        if bytes.len() > MAX_SOCKET_FRAME_BYTES {
            return Err(RadioError::TooLong {
                len: bytes.len(),
                max: MAX_SOCKET_FRAME_BYTES,
            });
        }
        self.socket.send(bytes);
        Ok(())
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        let delivery = self.socket.poll()?;
        Some(if delivery.crc_ok {
            RadioEvent::Received(Received {
                bytes: delivery.bytes,
                rssi_dbm: delivery.rssi_dbm,
                snr_db: delivery.snr_db,
            })
        } else {
            RadioEvent::CrcError {
                rssi_dbm: delivery.rssi_dbm,
            }
        })
    }
}
