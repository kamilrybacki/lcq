//! The virtual chip, the real driver and the medium, behind the [`Radio`] seam.
//!
//! Three threads per radio. The medium thread turns the hub's deliveries into
//! frames ending at the chip. The antenna thread carries what the chip
//! transmits to the hub. The driver thread runs `lora-phy`'s state machine
//! over the virtual bus: it listens continuously, services every IRQ, and
//! leaves receive mode only to transmit -- which is when the chip is deaf, as
//! the real one is. The node sees none of this: it transmits and polls.

use std::fmt;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::Duration;

use lora_modulation::{Bandwidth, CodingRate, SpreadingFactor};
use lora_phy::LoRa;
use lora_phy::mod_params::{ModulationParams, PacketParams, RadioError as DriverError, RxMode};
use lora_phy::mod_traits::IrqState;
use lora_phy::sx126x::{Config, Sx126x, Sx1262};

use super::bus::{HostDelay, VirtualIv, VirtualSpi};
use super::chip::Chip;
use super::executor::block_on;
use crate::application::{PhyProfile, Radio, RadioError, RadioEvent, Received};
use crate::infrastructure::hub::{HubError, HubSocket};

/// The chip's data buffer: the most a `LoRa` frame can carry.
const MAX_PAYLOAD_BYTES: usize = 255;
/// How long the driver thread waits for something to happen before it looks
/// around anyway.
const IDLE_WAIT: Duration = Duration::from_millis(250);

/// Why the radio could not be started.
#[derive(Debug)]
pub enum StartError {
    /// The medium could not be reached.
    Hub(HubError),
    /// A thread could not be spawned.
    Thread(std::io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hub(error) => write!(f, "{error}"),
            Self::Thread(error) => write!(f, "radio thread: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

enum Command {
    Transmit(Vec<u8>),
}

/// A virtual SX1262 attached to `lcq-hub`, driven by the unmodified
/// `lora-phy` driver.
pub struct Sx126xRadio {
    commands: Sender<Command>,
    events: Receiver<RadioEvent>,
    chip: Chip,
}

impl Sx126xRadio {
    /// Bring the radio up: join the hub as member `index`, start the chip on
    /// the given time scale, and have the driver initialise it for `profile`.
    ///
    /// Driver failures after this returns -- an initialisation that does not
    /// take, a transmit that errors -- arrive as [`RadioEvent::Note`]s.
    ///
    /// # Errors
    ///
    /// [`StartError::Hub`] if the medium never answered, [`StartError::Thread`]
    /// if the host refused a thread.
    pub fn start(
        hub: &str,
        index: usize,
        profile: PhyProfile,
        scale: u32,
    ) -> Result<Self, StartError> {
        let socket = HubSocket::connect(hub, index).map_err(StartError::Hub)?;
        let (mut writer, deliveries) = socket.into_parts();
        let (outbound, to_medium) = channel::<Vec<u8>>();
        let (events_in, events) = channel::<RadioEvent>();
        let chip = Chip::new(scale, outbound, events_in.clone());

        let listener = chip.clone();
        thread::Builder::new()
            .name(format!("sx126x-medium-{index}"))
            .spawn(move || {
                for delivery in deliveries {
                    listener.deliver(&delivery);
                }
            })
            .map_err(StartError::Thread)?;
        thread::Builder::new()
            .name(format!("sx126x-antenna-{index}"))
            .spawn(move || {
                for bytes in to_medium {
                    writer.send(&bytes);
                }
            })
            .map_err(StartError::Thread)?;

        let (commands, inbox) = channel::<Command>();
        let driven = chip.clone();
        thread::Builder::new()
            .name(format!("sx126x-driver-{index}"))
            .spawn(move || drive(&driven, profile, &inbox, &events_in))
            .map_err(StartError::Thread)?;

        Ok(Self {
            commands,
            events,
            chip,
        })
    }

    /// The chip, for whoever wants to look at it.
    #[must_use]
    pub fn chip(&self) -> &Chip {
        &self.chip
    }
}

impl Radio for Sx126xRadio {
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), RadioError> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(RadioError::TooLong {
                len: bytes.len(),
                max: MAX_PAYLOAD_BYTES,
            });
        }
        self.commands
            .send(Command::Transmit(bytes.to_vec()))
            .map_err(|_| RadioError::Offline)?;
        self.chip.wake_host();
        Ok(())
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        self.events.try_recv().ok()
    }
}

fn note(events: &Sender<RadioEvent>, body: String) {
    let _ = events.send(RadioEvent::Note(body));
}

/// The driver thread: bring the chip up, then listen, service IRQs and
/// transmit on request until the node lets go of the radio.
fn drive(
    chip: &Chip,
    profile: PhyProfile,
    commands: &Receiver<Command>,
    events: &Sender<RadioEvent>,
) {
    let mut driver = match Driver::bring_up(chip, profile) {
        Ok(driver) => driver,
        Err(why) => {
            note(
                events,
                format!("\"event\":\"chip_failed\",\"why\":\"{why}\""),
            );
            return;
        }
    };
    note(
        events,
        format!(
            "\"event\":\"chip_up\",\"sf\":{},\"bw_hz\":{},\"frequency_hz\":{},\"scale\":{}",
            profile.spreading_factor,
            profile.bandwidth_hz,
            profile.frequency_hz,
            chip.scale()
        ),
    );
    let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
    loop {
        let activity = chip.wait_for_activity(IDLE_WAIT);
        if activity.irq {
            driver.service_irq(&mut buffer, events);
        }
        loop {
            match commands.try_recv() {
                Ok(Command::Transmit(bytes)) => {
                    if let Err(why) = driver.transmit(&bytes) {
                        note(
                            events,
                            format!("\"event\":\"chip_tx_failed\",\"why\":\"{why:?}\""),
                        );
                    }
                    if let Err(why) = driver.listen() {
                        note(
                            events,
                            format!("\"event\":\"chip_rx_failed\",\"why\":\"{why:?}\""),
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }
}

/// The driver and the parameters it was brought up with.
struct Driver {
    lora: LoRa<Sx126x<VirtualSpi, VirtualIv, Sx1262>, HostDelay>,
    modulation: ModulationParams,
    tx_params: PacketParams,
    rx_params: PacketParams,
    power_dbm: i32,
}

impl Driver {
    fn bring_up(chip: &Chip, profile: PhyProfile) -> Result<Self, String> {
        let sf = spreading_factor(profile.spreading_factor)
            .ok_or_else(|| format!("spreading factor {}", profile.spreading_factor))?;
        let bw = bandwidth(profile.bandwidth_hz)
            .ok_or_else(|| format!("bandwidth {} Hz", profile.bandwidth_hz))?;
        let cr = coding_rate(profile.coding_rate_denominator)
            .ok_or_else(|| format!("coding rate 4/{}", profile.coding_rate_denominator))?;
        let kind = Sx126x::new(
            VirtualSpi::new(chip.clone()),
            VirtualIv::new(chip.clone()),
            Config {
                chip: Sx1262,
                tcxo_ctrl: None,
                use_dcdc: false,
                rx_boost: false,
            },
        );
        let mut lora = block_on(LoRa::with_syncword(kind, profile.sync_word, HostDelay))
            .map_err(|error| format!("init: {error:?}"))?;
        let modulation = lora
            .create_modulation_params(sf, bw, cr, profile.frequency_hz)
            .map_err(|error| format!("modulation: {error:?}"))?;
        let tx_params = lora
            .create_tx_packet_params(profile.preamble_symbols, false, true, false, &modulation)
            .map_err(|error| format!("tx packet params: {error:?}"))?;
        let rx_params = lora
            .create_rx_packet_params(
                profile.preamble_symbols,
                false,
                u8::MAX,
                true,
                false,
                &modulation,
            )
            .map_err(|error| format!("rx packet params: {error:?}"))?;
        let mut driver = Self {
            lora,
            modulation,
            tx_params,
            rx_params,
            power_dbm: i32::from(profile.tx_power_dbm),
        };
        driver
            .listen()
            .map_err(|error| format!("listen: {error:?}"))?;
        Ok(driver)
    }

    /// Receive continuously until something else is asked of the chip.
    fn listen(&mut self) -> Result<(), DriverError> {
        block_on(
            self.lora
                .prepare_for_rx(RxMode::Continuous, &self.modulation, &self.rx_params),
        )?;
        block_on(self.lora.start_rx())
    }

    /// Send one frame and wait for `TxDone`. The chip hears nothing meanwhile.
    fn transmit(&mut self, bytes: &[u8]) -> Result<(), DriverError> {
        block_on(self.lora.prepare_for_tx(
            &self.modulation,
            &mut self.tx_params,
            self.power_dbm,
            bytes,
        ))?;
        block_on(self.lora.tx())
    }

    /// Something reached DIO1 while listening.
    fn service_irq(&mut self, buffer: &mut [u8], events: &Sender<RadioEvent>) {
        match block_on(self.lora.process_irq_event()) {
            Ok(Some(IrqState::Done)) => {
                match block_on(self.lora.get_rx_result(&self.rx_params, buffer)) {
                    Ok((length, status)) => {
                        let _ = events.send(RadioEvent::Received(Received {
                            bytes: buffer[..usize::from(length)].to_vec(),
                            rssi_dbm: status.rssi,
                            snr_db: i8::try_from(status.snr).unwrap_or(if status.snr < 0 {
                                i8::MIN
                            } else {
                                i8::MAX
                            }),
                        }));
                    }
                    Err(why) => note(
                        events,
                        format!("\"event\":\"chip_rx_failed\",\"why\":\"{why:?}\""),
                    ),
                }
                let _ = block_on(self.lora.clear_irq_status());
            }
            Ok(Some(IrqState::PreambleReceived) | None) => {
                let _ = block_on(self.lora.clear_irq_status());
            }
            Err(DriverError::ReceiveTimeout) => {
                let _ = block_on(self.lora.clear_irq_status());
                if let Err(why) = self.listen() {
                    note(
                        events,
                        format!("\"event\":\"chip_rx_failed\",\"why\":\"{why:?}\""),
                    );
                }
            }
            Err(why) => {
                note(
                    events,
                    format!("\"event\":\"chip_irq_failed\",\"why\":\"{why:?}\""),
                );
                let _ = block_on(self.lora.clear_irq_status());
            }
        }
    }
}

fn spreading_factor(value: u8) -> Option<SpreadingFactor> {
    Some(match value {
        5 => SpreadingFactor::_5,
        6 => SpreadingFactor::_6,
        7 => SpreadingFactor::_7,
        8 => SpreadingFactor::_8,
        9 => SpreadingFactor::_9,
        10 => SpreadingFactor::_10,
        11 => SpreadingFactor::_11,
        12 => SpreadingFactor::_12,
        _ => return None,
    })
}

fn bandwidth(hz: u32) -> Option<Bandwidth> {
    Some(match hz {
        7_800 => Bandwidth::_7KHz,
        10_400 => Bandwidth::_10KHz,
        15_600 => Bandwidth::_15KHz,
        20_800 => Bandwidth::_20KHz,
        31_250 => Bandwidth::_31KHz,
        41_700 => Bandwidth::_41KHz,
        62_500 => Bandwidth::_62KHz,
        125_000 => Bandwidth::_125KHz,
        250_000 => Bandwidth::_250KHz,
        500_000 => Bandwidth::_500KHz,
        _ => return None,
    })
}

fn coding_rate(denominator: u8) -> Option<CodingRate> {
    Some(match denominator {
        5 => CodingRate::_4_5,
        6 => CodingRate::_4_6,
        7 => CodingRate::_4_7,
        8 => CodingRate::_4_8,
        _ => return None,
    })
}
