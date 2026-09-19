//! The bus on a Linux single-board computer: `spidev` and the GPIO character
//! device, under the same `lora-phy` driver the virtual chip runs.
//!
//! A Raspberry Pi with an SX1262 HAT is the deployment shape; this is what
//! `VirtualSpi` and `VirtualIv` are swapped for. Nothing above it changes.
//! Untested against hardware until there is hardware (D16, D21): what is
//! pinned here is that it compiles, that the transaction shape matches the
//! chip's NSS framing, and that BUSY and DIO1 are read the way the datasheet
//! says.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use embedded_hal::spi::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::spi::SpiDevice;
use gpio_cdev::{Chip as GpioChip, LineHandle, LineRequestFlags};
use lora_phy::mod_params::RadioError;
use lora_phy::mod_traits::InterfaceVariant;
use spidev::{SpiModeFlags, Spidev, SpidevOptions, SpidevTransfer};

/// The SPI clock the `SX126x` is driven at: well under its 16 MHz ceiling.
pub const SPI_SPEED_HZ: u32 = 2_000_000;
/// How long BUSY may stay high before the chip is declared stuck.
const BUSY_TIMEOUT: Duration = Duration::from_millis(100);
/// How often a line is polled while waiting on it.
const POLL: Duration = Duration::from_micros(200);

/// A SPI failure on the host bus.
#[derive(Debug)]
pub struct LinuxSpiError(pub io::Error);

impl embedded_hal::spi::Error for LinuxSpiError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// `/dev/spidevX.Y` as the driver's SPI device.
pub struct LinuxSpi {
    spi: Spidev,
}

impl LinuxSpi {
    /// Open and configure the device: mode 0, 8 bits, [`SPI_SPEED_HZ`].
    ///
    /// # Errors
    ///
    /// The kernel's, if the device cannot be opened or configured.
    pub fn open(path: &str) -> io::Result<Self> {
        let mut spi = Spidev::open(path)?;
        let options = SpidevOptions::new()
            .bits_per_word(8)
            .max_speed_hz(SPI_SPEED_HZ)
            .mode(SpiModeFlags::SPI_MODE_0)
            .build();
        spi.configure(&options)?;
        Ok(Self { spi })
    }
}

impl ErrorType for LinuxSpi {
    type Error = LinuxSpiError;
}

// The trait's functions are `async`; the kernel answers synchronously.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl SpiDevice for LinuxSpi {
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), LinuxSpiError> {
        // One ioctl carries the whole transaction, so NSS stays asserted
        // between the operations -- the chip's command framing. spidev has no
        // in-place transfer, so an in-place operation clocks a copy of its
        // bytes out while it reads; the copies live for the transaction.
        let copies: Vec<Vec<u8>> = operations
            .iter()
            .map(|operation| match operation {
                Operation::TransferInPlace(buffer) => buffer.to_vec(),
                _ => Vec::new(),
            })
            .collect();
        let mut transfers: Vec<SpidevTransfer<'_, '_>> = operations
            .iter_mut()
            .zip(copies.iter())
            .map(|(operation, copy)| match operation {
                Operation::Write(bytes) => SpidevTransfer::write(bytes),
                Operation::Read(buffer) => SpidevTransfer::read(buffer),
                Operation::Transfer(read, write) => SpidevTransfer::read_write(write, read),
                Operation::TransferInPlace(buffer) => SpidevTransfer::read_write(copy, buffer),
                Operation::DelayNs(ns) => {
                    SpidevTransfer::delay(u16::try_from(*ns / 1_000).unwrap_or(u16::MAX))
                }
            })
            .collect();
        self.spi
            .transfer_multiple(&mut transfers)
            .map_err(LinuxSpiError)
    }
}

/// Which GPIO lines the chip's control pins are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// The GPIO character device, `/dev/gpiochip0` on a Raspberry Pi.
    pub chip: String,
    /// BUSY, an input.
    pub busy: u32,
    /// DIO1, an input: the IRQ line.
    pub dio1: u32,
    /// NRESET, an output.
    pub reset: u32,
}

/// BUSY, DIO1 and NRESET on the GPIO character device.
///
/// DIO1 is shared with the driver thread's watch, which polls it between
/// commands; a line can be requested once, so both hold the one handle.
pub struct LinuxIv {
    busy: LineHandle,
    dio1: Arc<Mutex<LineHandle>>,
    reset: LineHandle,
    /// Set by the host to break out of an IRQ wait when it has work.
    wake: Arc<AtomicBool>,
}

impl LinuxIv {
    /// Request the three lines; the DIO1 handle comes back shared, for the
    /// driver thread's watch.
    ///
    /// # Errors
    ///
    /// The kernel's, if a line is missing or busy.
    pub fn open(
        pins: &Pins,
        wake: Arc<AtomicBool>,
    ) -> Result<(Self, Arc<Mutex<LineHandle>>), gpio_cdev::Error> {
        let mut chip = GpioChip::new(&pins.chip)?;
        let busy = chip
            .get_line(pins.busy)?
            .request(LineRequestFlags::INPUT, 0, "lcq-busy")?;
        let dio1 = Arc::new(Mutex::new(chip.get_line(pins.dio1)?.request(
            LineRequestFlags::INPUT,
            0,
            "lcq-dio1",
        )?));
        let reset = chip
            .get_line(pins.reset)?
            .request(LineRequestFlags::OUTPUT, 1, "lcq-reset")?;
        Ok((
            Self {
                busy,
                dio1: Arc::clone(&dio1),
                reset,
                wake,
            },
            dio1,
        ))
    }

    fn dio1_high(&self) -> Result<bool, gpio_cdev::Error> {
        Ok(self
            .dio1
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_value()?
            != 0)
    }
}

#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl InterfaceVariant for LinuxIv {
    async fn reset(&mut self, _delay: &mut impl DelayNs) -> Result<(), RadioError> {
        // DS.SX1261-2 8.1: NRESET low for at least 100 µs, then wait for the
        // chip to come out of reset.
        self.reset.set_value(0).map_err(|_| RadioError::Reset)?;
        thread::sleep(Duration::from_millis(1));
        self.reset.set_value(1).map_err(|_| RadioError::Reset)?;
        thread::sleep(Duration::from_millis(5));
        Ok(())
    }

    async fn wait_on_busy(&mut self) -> Result<(), RadioError> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            if self.busy.get_value().map_err(|_| RadioError::Busy)? == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(RadioError::Busy);
            }
            thread::sleep(POLL);
        }
    }

    async fn await_irq(&mut self) -> Result<(), RadioError> {
        loop {
            if self.dio1_high().map_err(|_| RadioError::DIO1)? {
                return Ok(());
            }
            if self.wake.swap(false, Ordering::AcqRel) {
                return Ok(());
            }
            thread::sleep(POLL);
        }
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
