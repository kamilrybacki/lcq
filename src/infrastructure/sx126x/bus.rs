//! The wires between the driver and the chip model: SPI, BUSY, DIO1, reset.
//!
//! The driver only ever sees `embedded-hal` traits. A real board implements
//! them with a SPI peripheral and GPIO lines; this implements them with the
//! model, and the driver cannot tell the difference -- which is the whole
//! point of putting the seam here rather than above the driver.

use std::thread;
use std::time::Duration;

use embedded_hal::spi::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::spi::SpiDevice;
use lora_phy::mod_params::RadioError;
use lora_phy::mod_traits::InterfaceVariant;

use super::chip::Chip;

/// The SPI error type the trait demands. The virtual bus never fails, so no
/// value of this is ever produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiFault;

impl embedded_hal::spi::Error for SpiFault {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// A SPI device wired to the chip model.
///
/// One transaction is one command: every write phase is command bytes, every
/// read phase is filled from the command's reply, in order. That is how the
/// chip's NSS-framed transfers work, and it is exactly how the driver's
/// `read_with_status` lays out its operations.
pub struct VirtualSpi {
    chip: Chip,
}

impl VirtualSpi {
    /// Wire a bus to the chip.
    #[must_use]
    pub const fn new(chip: Chip) -> Self {
        Self { chip }
    }
}

impl ErrorType for VirtualSpi {
    type Error = SpiFault;
}

// The trait's functions are `async`; the model answers at once, so nothing
// in them awaits. Clippy 1.98 flags that; it is the point of the virtual bus.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl SpiDevice for VirtualSpi {
    async fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), SpiFault> {
        let mut command: Vec<u8> = Vec::new();
        for operation in operations.iter() {
            match operation {
                Operation::Write(bytes) | Operation::Transfer(_, bytes) => {
                    command.extend_from_slice(bytes);
                }
                Operation::TransferInPlace(bytes) => command.extend_from_slice(bytes),
                Operation::Read(_) | Operation::DelayNs(_) => {}
            }
        }
        let reply = match command.split_first() {
            Some((&opcode, params)) => self.chip.execute(opcode, params),
            None => Vec::new(),
        };
        let mut reply = reply.into_iter();
        for operation in operations.iter_mut() {
            match operation {
                Operation::Read(buffer)
                | Operation::TransferInPlace(buffer)
                | Operation::Transfer(buffer, _) => {
                    for byte in buffer.iter_mut() {
                        *byte = reply.next().unwrap_or(0);
                    }
                }
                Operation::Write(_) | Operation::DelayNs(_) => {}
            }
        }
        Ok(())
    }
}

/// The BUSY, DIO1, reset and RF-switch lines, wired to the same model.
pub struct VirtualIv {
    chip: Chip,
}

impl VirtualIv {
    /// Wire the control lines to the chip.
    #[must_use]
    pub const fn new(chip: Chip) -> Self {
        Self { chip }
    }
}

#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl InterfaceVariant for VirtualIv {
    async fn reset(&mut self, _delay: &mut impl DelayNs) -> Result<(), RadioError> {
        self.chip.reset();
        Ok(())
    }

    async fn wait_on_busy(&mut self) -> Result<(), RadioError> {
        self.chip.wait_on_busy();
        Ok(())
    }

    async fn await_irq(&mut self) -> Result<(), RadioError> {
        self.chip.await_irq();
        Ok(())
    }

    async fn enable_rf_switch_rx(&mut self) -> Result<(), RadioError> {
        Ok(())
    }

    async fn enable_rf_switch_tx(&mut self) -> Result<(), RadioError> {
        Ok(())
    }

    async fn disable_rf_switch(&mut self) -> Result<(), RadioError> {
        Ok(())
    }
}

/// Delays on the host: a sleep. The driver asks for a couple of milliseconds
/// around reset and sleep, which no time scale needs to compress.
pub struct HostDelay;

#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl DelayNs for HostDelay {
    async fn delay_ns(&mut self, ns: u32) {
        thread::sleep(Duration::from_nanos(u64::from(ns)));
    }
}
