//! The virtual chip, the real driver and the medium, behind the [`Radio`] seam.
//!
//! Three threads per radio. The medium thread turns the hub's deliveries into
//! frames ending at the chip. The antenna thread carries what the chip
//! transmits to the hub. The driver thread runs `lora-phy`'s state machine
//! over the virtual bus: it listens continuously, services every IRQ, and
//! leaves receive mode only to transmit -- which is when the chip is deaf, as
//! the real one is. The node sees none of this: it transmits and polls.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::Duration;

use embedded_hal_async::spi::SpiDevice;
use lora_modulation::{Bandwidth, CodingRate, SpreadingFactor};
use lora_phy::LoRa;
use lora_phy::mod_params::{ModulationParams, PacketParams, RadioError as DriverError, RxMode};
use lora_phy::mod_traits::{InterfaceVariant, IrqState};
use lora_phy::sx126x::{Config, Sx126x, Sx1262};

use super::bus::{HostDelay, VirtualIv, VirtualSpi};
use super::chip::{Activity, Chip, IRQ_CRC_ERR, IRQ_HEADER_ERR};
use super::executor::block_on;
use crate::application::{PhyProfile, Radio, RadioError, RadioEvent, Received};
use crate::infrastructure::hub::{HubError, HubSocket, Inbound};

/// The chip's data buffer: the most a `LoRa` frame can carry.
const MAX_PAYLOAD_BYTES: usize = 255;
/// How long the driver thread waits for something to happen before it looks
/// around anyway.
const IDLE_WAIT: Duration = Duration::from_millis(250);
/// The longest timed receive the chip's 24-bit timer allows, for a profile
/// that does not listen continuously.
const LONGEST_TIMED_RX_MS: u32 = 262_000;

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

/// What the driver thread waits on between commands, and what it may look at
/// that the driver does not expose: the virtual chip's IRQ status, or nothing
/// on a real board.
pub trait Watch: Send + Sync {
    /// Block until an IRQ is pending, the host asks for attention, or the
    /// timeout passes.
    fn wait_for_activity(&self, timeout: Duration) -> Activity;
    /// Wake the driver thread: the host has a command.
    fn wake_host(&self);
    /// The chip's IRQ status right now, if it can be read without the driver:
    /// the virtual chip's, never a real one's.
    fn irq_flags(&self) -> Option<u16>;
}

impl Watch for Chip {
    fn wait_for_activity(&self, timeout: Duration) -> Activity {
        Chip::wait_for_activity(self, timeout)
    }

    fn wake_host(&self) {
        Chip::wake_host(self);
    }

    fn irq_flags(&self) -> Option<u16> {
        Some(self.snapshot().irq_status)
    }
}

pub(crate) enum Command {
    Transmit(Vec<u8>),
}

/// The node's end of a driver thread: commands in, events out, whichever
/// bus the driver is on.
pub struct DriverHandle {
    commands: Sender<Command>,
    events: Receiver<RadioEvent>,
    watch: Arc<dyn Watch>,
}

impl DriverHandle {
    pub(crate) fn new(
        commands: Sender<Command>,
        events: Receiver<RadioEvent>,
        watch: Arc<dyn Watch>,
    ) -> Self {
        Self {
            commands,
            events,
            watch,
        }
    }

    /// The watch the driver thread shares.
    pub(crate) fn watch(&self) -> Arc<dyn Watch> {
        Arc::clone(&self.watch)
    }

    /// Hand a frame to the driver thread and wake it.
    ///
    /// # Errors
    ///
    /// [`RadioError::TooLong`] past the chip's buffer, [`RadioError::Offline`]
    /// once the driver thread is gone.
    pub fn transmit(&self, bytes: &[u8]) -> Result<(), RadioError> {
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(RadioError::TooLong {
                len: bytes.len(),
                max: MAX_PAYLOAD_BYTES,
            });
        }
        self.commands
            .send(Command::Transmit(bytes.to_vec()))
            .map_err(|_| RadioError::Offline)?;
        self.watch.wake_host();
        Ok(())
    }

    /// The next event from the driver thread, if any.
    #[must_use]
    pub fn poll(&self) -> Option<RadioEvent> {
        self.events.try_recv().ok()
    }
}

/// A virtual SX1262 attached to `lcq-hub`, driven by the unmodified
/// `lora-phy` driver.
pub struct Sx126xRadio {
    handle: DriverHandle,
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
        let chip = Chip::new(
            scale,
            profile.preamble_symbols_to_lock,
            outbound,
            events_in.clone(),
        );

        let listener = chip.clone();
        thread::Builder::new()
            .name(format!("sx126x-medium-{index}"))
            .spawn(move || {
                for inbound in deliveries {
                    match inbound {
                        Inbound::Delivery(delivery) => listener.deliver(&delivery),
                        Inbound::Preamble(preamble) => listener.notice_preamble(&preamble),
                    }
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
        let watch: Arc<dyn Watch> = Arc::new(chip.clone());
        let driven = Arc::clone(&watch);
        thread::Builder::new()
            .name(format!("sx126x-driver-{index}"))
            .spawn(move || drive(driven.as_ref(), kind, profile, &inbox, &events_in))
            .map_err(StartError::Thread)?;

        Ok(Self {
            handle: DriverHandle::new(commands, events, watch),
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
        self.handle.transmit(bytes)
    }

    fn poll(&mut self) -> Option<RadioEvent> {
        self.handle.poll()
    }
}

fn note(events: &Sender<RadioEvent>, body: String) {
    let _ = events.send(RadioEvent::Note(body));
}

/// The driver thread: bring the chip up, then listen, service IRQs and
/// transmit on request until the node lets go of the radio. Generic over the
/// bus: the virtual chip's or a real board's.
pub(crate) fn drive<SPI, IV>(
    watch: &dyn Watch,
    kind: Sx126x<SPI, IV, Sx1262>,
    profile: PhyProfile,
    commands: &Receiver<Command>,
    events: &Sender<RadioEvent>,
) where
    SPI: SpiDevice<u8>,
    IV: InterfaceVariant,
{
    let mut driver = match Driver::bring_up(kind, profile) {
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
            "\"event\":\"chip_up\",\"phy\":\"{}\",\"sf\":{},\"bw_hz\":{},\"frequency_hz\":{}",
            profile.name, profile.spreading_factor, profile.bandwidth_hz, profile.frequency_hz
        ),
    );
    let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
    loop {
        let activity = watch.wait_for_activity(IDLE_WAIT);
        if activity.irq {
            driver.service_irq(watch, &mut buffer, events);
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
struct Driver<SPI, IV>
where
    SPI: SpiDevice<u8>,
    IV: InterfaceVariant,
{
    lora: LoRa<Sx126x<SPI, IV, Sx1262>, HostDelay>,
    modulation: ModulationParams,
    tx_params: PacketParams,
    rx_params: PacketParams,
    power_dbm: i32,
    rx_continuous: bool,
}

impl<SPI, IV> Driver<SPI, IV>
where
    SPI: SpiDevice<u8>,
    IV: InterfaceVariant,
{
    fn bring_up(kind: Sx126x<SPI, IV, Sx1262>, profile: PhyProfile) -> Result<Self, String> {
        let sf = spreading_factor(profile.spreading_factor)
            .ok_or_else(|| format!("spreading factor {}", profile.spreading_factor))?;
        let bw = bandwidth(profile.bandwidth_hz)
            .ok_or_else(|| format!("bandwidth {} Hz", profile.bandwidth_hz))?;
        let cr = coding_rate(profile.coding_rate_denominator)
            .ok_or_else(|| format!("coding rate 4/{}", profile.coding_rate_denominator))?;
        let mut lora = block_on(LoRa::with_syncword(kind, profile.sync_word, HostDelay))
            .map_err(|error| format!("init: {error:?}"))?;
        let modulation = lora
            .create_modulation_params(sf, bw, cr, profile.frequency_hz)
            .map_err(|error| format!("modulation: {error:?}"))?;
        if modulation.low_data_rate_optimize != u8::from(profile.low_data_rate_optimize) {
            return Err(format!(
                "profile says low data rate optimisation {}, the chip's rule says otherwise",
                profile.low_data_rate_optimize
            ));
        }
        let tx_params = lora
            .create_tx_packet_params(
                profile.preamble_symbols,
                !profile.explicit_header,
                profile.crc_on,
                profile.iq_inverted,
                &modulation,
            )
            .map_err(|error| format!("tx packet params: {error:?}"))?;
        let rx_params = lora
            .create_rx_packet_params(
                profile.preamble_symbols,
                !profile.explicit_header,
                u8::MAX,
                profile.crc_on,
                profile.iq_inverted,
                &modulation,
            )
            .map_err(|error| format!("rx packet params: {error:?}"))?;
        let mut driver = Self {
            lora,
            modulation,
            tx_params,
            rx_params,
            power_dbm: i32::from(profile.tx_power_dbm),
            rx_continuous: profile.rx_continuous,
        };
        driver
            .listen()
            .map_err(|error| format!("listen: {error:?}"))?;
        Ok(driver)
    }

    /// Receive until something else is asked of the chip: continuously, as
    /// the profile says, or the chip's longest timed receive otherwise.
    fn listen(&mut self) -> Result<(), DriverError> {
        let mode = if self.rx_continuous {
            RxMode::Continuous
        } else {
            RxMode::SingleMs(LONGEST_TIMED_RX_MS)
        };
        block_on(
            self.lora
                .prepare_for_rx(mode, &self.modulation, &self.rx_params),
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
    fn service_irq(&mut self, watch: &dyn Watch, buffer: &mut [u8], events: &Sender<RadioEvent>) {
        match block_on(self.lora.process_irq_event()) {
            Ok(Some(IrqState::Done)) => {
                // The driver does not say whether the frame passed its CRC --
                // it reports a reception either way -- but the chip does, in
                // the IRQ status nothing has cleared yet; a real adapter would
                // read `GetIrqStatus` for the same answer. A failed frame is
                // telemetry, never a frame: nothing above this decodes it.
                let corrupted = watch
                    .irq_flags()
                    .is_some_and(|flags| flags & IRQ_CRC_ERR != 0);
                match block_on(self.lora.get_rx_result(&self.rx_params, buffer)) {
                    Ok((_, status)) if corrupted => {
                        let _ = events.send(RadioEvent::CrcError {
                            rssi_dbm: status.rssi,
                        });
                    }
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
                // The driver says nothing about a header error; the chip's
                // IRQ status does, and the packet status carries its strength.
                if watch
                    .irq_flags()
                    .is_some_and(|flags| flags & IRQ_HEADER_ERR != 0)
                {
                    let rssi_dbm = block_on(self.lora.get_rx_result(&self.rx_params, buffer))
                        .map_or(0, |(_, status)| status.rssi);
                    let _ = events.send(RadioEvent::HeaderError { rssi_dbm });
                }
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
