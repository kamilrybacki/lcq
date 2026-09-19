//! A real `SX1262` on a Linux single-board computer, behind the [`Radio`] seam.
//!
//! The same driver thread as the virtual chip's ([`super::radio`]), with the
//! bus on `spidev` and the GPIO character device ([`super::linux`]) and the
//! air for a medium. Nothing here has met hardware yet (D21): it compiles,
//! the sequences it sends are the driver's, and the first board will say the
//! rest. Without the virtual chip's view of the IRQ status, a CRC-failed
//! frame is handed up as a reception -- the driver's behaviour, pinned in the
//! bench -- and the protocol's seal rejects it; the upstream change that
//! surfaces CRC and header errors closes that gap.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use gpio_cdev::LineHandle;
use lora_phy::sx126x::{Config, Sx126x, Sx1262, TcxoCtrlVoltage};

use super::chip::Activity;
use super::linux::{LinuxIv, LinuxSpi, Pins};
use super::radio::{DriverHandle, Watch, drive};
use crate::application::{PhyProfile, Radio, RadioError, RadioEvent};

/// How often DIO1 is polled while the driver thread waits for something to
/// happen; interrupt latency at `LoRa` timescales is generous.
const POLL: Duration = Duration::from_micros(500);

/// Why the board could not be brought up.
#[derive(Debug)]
pub enum HardwareError {
    /// The SPI device.
    Spi(std::io::Error),
    /// A GPIO line.
    Gpio(gpio_cdev::Error),
    /// A thread could not be spawned.
    Thread(std::io::Error),
}

impl fmt::Display for HardwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spi(error) => write!(f, "spi: {error}"),
            Self::Gpio(error) => write!(f, "gpio: {error}"),
            Self::Thread(error) => write!(f, "radio thread: {error}"),
        }
    }
}

impl std::error::Error for HardwareError {}

/// What the driver thread waits on for a real board: DIO1, and the host.
pub struct LinuxWatch {
    dio1: Arc<Mutex<LineHandle>>,
    wake: Arc<AtomicBool>,
}

impl Watch for LinuxWatch {
    fn wait_for_activity(&self, timeout: Duration) -> Activity {
        let deadline = Instant::now() + timeout;
        loop {
            let irq = self
                .dio1
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_value()
                .is_ok_and(|value| value != 0);
            let host = self.wake.swap(false, Ordering::AcqRel);
            if irq || host || Instant::now() >= deadline {
                return Activity { irq, host };
            }
            thread::sleep(POLL);
        }
    }

    fn wake_host(&self) {
        self.wake.store(true, Ordering::Release);
    }

    fn irq_flags(&self) -> Option<u16> {
        // The driver clears the chip's IRQ status as it goes; nothing here
        // can read it without another SPI transaction the driver owns.
        None
    }
}

/// An `SX1262` on SPI and GPIO.
pub struct HardwareRadio {
    handle: DriverHandle,
}

impl HardwareRadio {
    /// Bring the board up for `profile`.
    ///
    /// `tcxo` says whether the module has a temperature-compensated
    /// oscillator on DIO3 (most modules do; a Waveshare HAT does at 1.8 V).
    ///
    /// # Errors
    ///
    /// See [`HardwareError`].
    pub fn open(
        spi_path: &str,
        pins: &Pins,
        tcxo: Option<TcxoCtrlVoltage>,
        profile: PhyProfile,
    ) -> Result<Self, HardwareError> {
        let spi = LinuxSpi::open(spi_path).map_err(HardwareError::Spi)?;
        let wake = Arc::new(AtomicBool::new(false));
        let (iv, dio1) = LinuxIv::open(pins, Arc::clone(&wake)).map_err(HardwareError::Gpio)?;
        let watch = LinuxWatch { dio1, wake };
        let kind = Sx126x::new(
            spi,
            iv,
            Config {
                chip: Sx1262,
                tcxo_ctrl: tcxo,
                use_dcdc: true,
                rx_boost: false,
            },
        );
        let (commands, inbox) = channel();
        let (events_in, events) = channel::<RadioEvent>();
        let handle = DriverHandle::new(commands, events, Arc::new(watch));
        let watch = handle.watch();
        thread::Builder::new()
            .name("sx126x-hardware".into())
            .spawn(move || drive(watch.as_ref(), kind, profile, &inbox, &events_in))
            .map_err(HardwareError::Thread)?;
        Ok(Self { handle })
    }
}

impl Radio for HardwareRadio {
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        self.handle.transmit(bytes)
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        self.handle.poll()
    }
}
