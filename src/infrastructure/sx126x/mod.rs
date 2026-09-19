//! A virtual SX1262 under the real driver.
//!
//! The chip model ([`Chip`]) executes the `SX126x` command set with a clock: a
//! transmission occupies its airtime, the chip hears nothing outside receive
//! mode, and a frame is received only if the chip was listening for all of it.
//! The unmodified `lora-phy` `Sx126x` driver runs on top through a virtual SPI
//! device and virtual BUSY, DIO1 and reset lines ([`VirtualSpi`],
//! [`VirtualIv`]), so the command sequences the node exercises here are the
//! ones a real chip would see. The medium is `lcq-hub`, reached through
//! [`crate::infrastructure::hub`].
//!
//! Register-for-register fidelity is not attempted (D16). What the datasheet
//! specifies and the hub can time is modelled; what only a front end knows is
//! not.

pub mod bridge;
mod bus;
mod chip;
mod executor;
#[cfg(feature = "hardware")]
mod hardware;
#[cfg(feature = "hardware")]
mod linux;
mod radio;

pub use bridge::{BridgeError, BridgeOptions, BridgeRadio};
pub use bus::{HostDelay, SpiFault, VirtualIv, VirtualSpi};
pub use chip::{
    Activity, Chip, ChipMode, ChipSnapshot, Counters, IRQ_CAD_DETECTED, IRQ_CAD_DONE, IRQ_CRC_ERR,
    IRQ_HEADER_ERR, IRQ_RX_DONE, IRQ_TIMEOUT, IRQ_TX_DONE,
};
pub use executor::block_on;
#[cfg(feature = "hardware")]
pub use hardware::{HardwareError, HardwareRadio, LinuxWatch};
#[cfg(feature = "hardware")]
pub use linux::{LinuxIv, LinuxSpi, LinuxSpiError, Pins, SPI_SPEED_HZ};
pub use lora_phy::sx126x::TcxoCtrlVoltage;
pub use radio::{DriverHandle, StartError, Sx126xRadio, Watch};
