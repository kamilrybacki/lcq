# Research — emulating the LoRa module itself

**Asked 2026-09-19.** Can `lcq-node` talk to a *virtual* LoRa module the way it
would talk to a real Semtech SX1262/SX1276 — so that the binary that passes the
container suite is the binary that goes to sea — and does anything off the shelf
already do this? Own research plus a consultation with Hermes (Discord, six-part
answer and one refinement, 2026-09-19 14:37–14:39 UTC). Every claim below was
checked against the primary source unless marked *estimate*.

## Verdict

Yes, and at three distinct fidelity levels — but **nothing off the shelf gives a
Linux process a virtual SX1262 to attach to.** The closest existing things are:

- **Meshtastic `SimRadio`** — the firmware's radio interface reimplemented as a
  loopback over TCP to a Python medium with path loss, positions and optional
  collision emulation. That is the shape `lcq-hub` already has (D14).
- **`lora-rs/lora-rs` test emulator** — a private behavioural model of the
  SX126x *command set* (opcodes, 256-byte buffer, IRQ mask, RX injection, TX log)
  on which the real `Sx126x` driver runs unmodified. It has **no clock**: TX
  completes inside `SetTx`, RX timeouts collapse to zero. A test double, not a
  module.
- **`gr-lora_sdr`** (EPFL TCL) — a complete LoRa transceiver in GNU Radio with a
  frame-error-rate simulator. Signal-level truth, GPL-3.0, heavy.

The recommended path is a **`Radio` seam in the node, then a timed virtual
SX1262 chip model driven by the unmodified `lora-phy` driver and wired to
`lcq-hub`, then hardware-in-the-loop with two to five SX1262 boards** — GNU Radio
only as a calibration lab. Register-for-register emulation of the whole chip,
Renode/QEMU/Wokwi and the LoRaWAN network simulators are not the way; the
reasons are in §2.

## 1. Where today's emulator stops

`lcq-hub` is a *channel* emulator: the node hands it a payload, the hub knows
the airtime and decides, per receiver, at the frame's end (`settle`).

What it models: airtime occupancy (`sim::channel::airtime_ms`); per-receiver
sensitivity against a two-ray path loss with `--spacing-m` (`sim::phy`);
overlap in time judged against every frame that overlapped; a power-only capture
rule of 6 dB (`sim::medium::capture_wins`); residual loss; partition and timed
isolation. A node never receives its own frame, and a node that is transmitting
loses every overlapping frame — but only *implicitly*, because its own frame is
an overlapper at nominal power and so wins the collision check.

What a real module does that the hub does not:

| Module behaviour | Why the protocol should feel it |
|---|---|
| Explicit half-duplex: TX, standby, RX are chip modes; nothing is heard outside RX | Turnaround after own TX and the re-arm command are where frames get missed |
| Capture depends on *when* the second frame arrives, not only on power | A receiver locked on a payload does not re-sync to a stronger preamble; the hub's flat 6 dB rule over-delivers |
| CRC-error and header-error events reach the host | The node could treat garbage as "someone spoke" evidence; today collided frames vanish silently |
| Per-packet RSSI/SNR to the host | Range and split diagnostics; the node logs none today |
| RX timeouts, symbol timeout, preamble detection, CAD | Listen-before-talk experiments are impossible without them |
| Command latency and BUSY | Sub-millisecond; negligible at SF10 symbol time (8.2 ms) but real |
| 255-byte buffer, header modes, sync word, LDRO | `MAX_FRAME_BYTES = 176` fits; the module enforces it, the hub does not |

## 2. Five ways to emulate the module

### 2.1 Virtual radio behind the driver API — feasible, cheapest, lowest fidelity

Implement the node-facing radio interface directly over TCP. In Rust the
reference API is `lora-phy` (`lora-rs/lora-rs`, MIT, active — last push
2026-08-27; the old `embassy-rs/lora-phy` is archived). Its `RadioKind` trait
has 27 async methods (init, modulation/packet params, channel, TX, RX, CAD,
IRQ, payload, packet status with RSSI/SNR); `InterfaceVariant` has six
(`reset`, `wait_on_busy`, `await_irq`, RF switch ×3). A `VirtualRadioKind`
would model configuration, RX/TX/CAD, timeouts and IRQs and move bytes over TCP.

Hermes's architectural point stands: do not expose those 27 methods to the
protocol. The node needs `configure / transmit / receive / cad` — a small trait,
with the hub, the virtual chip and hardware as adapters behind it.

- Feasibility high. Fidelity: protocol and driver-facing state machine only.
- *Estimate* (Hermes): 1–2 weeks for a minimal virtual radio; my estimate for the
  seam alone in `lcq-node` is a day, because `Link` (connect/send/poll) already is
  the seam.

### 2.2 Chip model behind the *unmodified* driver — feasible, the recommended core

`Sx126x<SPI, IV, C>` in `lora-phy` is generic over `embedded_hal_async::spi::SpiDevice`
and `InterfaceVariant`. Substitute a `VirtualSpi` and `VirtualIv` that execute
SX126x opcodes against a chip model, and the real driver — its command
sequences, buffer offsets, IRQ masks, errata workarounds — runs exactly as on
hardware. Upstream does this in `lora-phy/src/sx126x/test/emulator.rs`
(11 KB; `ChipModel { mode, registers, buffer[256], tx_base_addr, payload_length,
frequency_raw, irq_status, irq_mask, dio1_mask, rx_len, cad_activity,
pending_rx, tx_log }`, opcodes WriteRegister/ReadRegister/WriteBuffer/ReadBuffer/
SetSleep/SetStandby/SetRFFrequency/SetBufferBaseAddress/SetPacketParams/
CfgDIOIrq/ClrIrqStatus/GetIrqStatus/SetTx/SetRx/GetRxBufferStatus/
GetPacketStatus/SetCAD, everything else accepted without effect). It is
`#[cfg(test)]` and uses crate-private enums, so it is a template, not a
dependency; the opcode bytes are public datasheet values.
Upstream also compares the driver's SPI byte stream against Semtech's reference
driver (SWL2001 via `smtc-modem-cores`) command by command, which is what makes
"unmodified driver on a virtual chip" worth more than a driver of our own.

What has to be added to make it a *module*: virtual time (TX occupies
`airtime(modulation, packet params)` and raises `TxDone` at the end; the chip is
deaf in TX and standby; RX timeouts run; RxDone fires at a frame's end only if
the chip was in RX for the whole frame), BUSY after commands, RSSI/SNR from the
medium (the hub → node protocol must carry them), CRC-error delivery for
collided frames the hub currently drops, CAD answered from the medium's
in-flight set, and LoRaSim's timing rule for capture (a frame survives an
overlap if the other frame ends before the last five symbols of its own
preamble; otherwise a 6 dB power difference decides, else both are lost).

- Feasibility high; fidelity good for everything the datasheet specifies and
  the hub can time. It does not know what a real front end does with a −127 dBm
  frame in sea spray — that stays for calibration (§2.3, §2.6).
- Effort (mine): chip model with a clock ≈ 500–800 lines; hub protocol
  extension for RSSI/SNR/CRC ≈ 50; driver integration on a radio thread with a
  minimal executor ≈ 200. Three to five days of work with test cycles.
- Dependency: `lora-phy` (MIT) and `embedded-hal`/`embedded-hal-async`. The node
  is synchronous; the driver is async. Run it on its own thread under a trivial
  `block_on` — the virtual `InterfaceVariant` may block inside `await_irq`.
- Payoff at the hardware step: only `VirtualSpi`/`VirtualIv` are swapped for
  `linux-embedded-hal` spidev + GPIO (Raspberry Pi with an SX1262 HAT, which is
  how Meshtastic's `meshtasticd` attaches real radios). The driver, the seam and
  the protocol do not change.

### 2.3 Signal level — feasible as a lab, not as a test harness

`tapparelj/gr-lora_sdr`: full TX and RX chains for GNU Radio 3.10 — SF 5–12,
CR 4/5–4/8, explicit/implicit header, CRC, sync word, LDRO, soft-decision
decoding; verified over the air against RFM95, SX1276 and SX1262. Its
`apps/simulation/mc_simulator.py` sweeps SNR and measures frame error rate
(default SF7, 32-byte payload, 100 frames per SNR, clock offset in ppm, sample
rate ≥ 4 × BW). EPFL also publishes a multi-user receiver that decodes two
colliding same-SF users. `jkadbear/gr-lora` decodes collisions with the Pyramid
algorithm; `rpp0/gr-lora` is older and not the one to build on.

This is the only software path in which capture, preamble timing and
sensitivity *emerge* from a demodulator instead of being rules we wrote. It is
also GPL-3.0 (process boundary, never linked into the crate), needs GNU Radio
installed, and no trustworthy CPU-per-frame figure exists — measure, do not
invent. Use it to produce two tables — FER vs SNR at SF10, and capture outcome
vs (power difference, arrival offset) — and feed them to the hub and the chip
model as calibrated curves.

- *Estimate* (Hermes): 2–4 weeks to a proof of concept; 1–2 months to
  reproducible calibration.

### 2.4 Full-system emulators — deferred

Renode has wireless mediums (IEEE 802.15.4, BLE), a C# peripheral-modelling
guide and virtual time, but **no SX127x/SX126x model**: a GitHub code search for
`sx1276|sx1262|sx127x|sx126x` across `renode/renode-infrastructure` and
`qemu/qemu` returns nothing. A 2025 paper adding an AT86RF233 model to Renode
shows the pattern and the price: the whole modem state machine, again. Wokwi has
a custom-chip API but no LoRa chip and no bridge to a medium. All three become
relevant only if LCQ ever ships as `no_std` firmware on an MCU and reset/GPIO/
SPI/IRQ paths must be tested without hardware.

### 2.5 Network simulators — oracles, not runtimes

`signetlabdei/lorawan` (ns-3) has a LoRa PHY with datasheet sensitivities and a
per-SF interference matrix; ELoRa (Orange) runs it in real time and bridges to
real ChirpStack/TTN servers over UDP; FLoRa (OMNeT++/INET) models collisions and
capture; LoRaSim (SimPy, Python 2, 2017) is the compact packet-level reference
whose timing rule is worth porting. All are built around LoRaWAN — end devices,
gateways, a network server, Class A. For a closed P2P fleet they are useful as
an *independent model to compare the hub against*, not as something the node
binary can attach to.

### 2.6 Hardware in the loop — the final gate, cheap

Two routes, neither requiring firmware work:

- **RNode firmware** (Mark Qvist; community fork `liberatedsystems/RNode_Firmware_CE`)
  turns Heltec LoRa32 V3/V4, LilyGO T3S3/T-Beam/T-Echo, RAK4631 and others
  (SX1262/SX1276/SX1278/SX1280) into a **KISS-over-USB LoRa modem**, flashed with
  `rnodeconf` from the `rns` pip package. The host protocol is small and
  documented on the wire: `FREQUENCY 0x01`, `BANDWIDTH 0x02`, `TXPOWER 0x03`,
  `SF 0x04`, `CR 0x05`, `RADIO_STATE 0x06`, `DETECT 0x08`, `DATA 0x00`, and per
  received packet `STAT_RSSI 0x23` (dBm = raw − 157), `STAT_SNR 0x24` (dB = raw/4),
  then `DATA`. The Rust crate `tulle` (MPL-2.0, v0.0.2, July 2026) already
  implements it sans-io behind a `Modem` trait (`params / set_params /
  max_frame_len / enqueue / poll`), pinned by captures from real RNodes, with an
  optional tokio serial pump. Young (tens of downloads) but the shape is exactly
  our seam.
- **SPI HAT on a Linux SBC** via `lora-phy` over `linux-embedded-hal`, or the
  blocking `radio-sx127x` (ryankurte) whose `sx127x-util` already runs on a
  Raspberry Pi; the `radio` crate also ships `radio::mock::Radio` for
  application tests.

*Estimates* (Hermes): LilyGO T3-S3 SX1262 ≈ $15 (EU €22–31), Heltec V3/V4
$18–28, RAK WisBlock kit from €28; two boards with antennas €45–70, four
€90–140. SMA attenuators, a combiner and a shielded box cost more than the
boards and are what make collision tests repeatable. Duty cycle on 868 MHz is
already enforced by the node (D12).

## 3. The precedent worth knowing

Meshtastic's native build (`meshtasticd`, Portduino) has `SimRadio`: the radio
interface implemented as a loopback that ships packets over TCP to the
`Meshtasticator` Python medium, which forwards each frame to the nodes that
can hear it from their simulated positions and a path-loss model, records
channel utilisation, and — behind `USERPREFS_SIMRADIO_EMULATE_COLLISIONS` — drops
both frames on overlap and flags a collision during transmission when the
overlap exceeds the preamble. `lcq-hub` is already at or past that fidelity;
what Meshtastic never did is run its real chip driver against the simulation.
That is the step §2.2 adds.

## 4. Plan

1. **`Radio` seam.** Replace `Link` in `lcq-node` with a trait —
   `configure(profile)`, `transmit(bytes) -> airtime`, `poll() -> Received {bytes,
   rssi, snr} | CrcError | TxDone` — and `HubRadio` as the first adapter. No
   behaviour change; the container suite is the regression test.
2. **Virtual SX1262.** Chip model with a clock (§2.2) + `VirtualSpi`/`VirtualIv`
   + the unmodified `lora-phy` `Sx126x` driver on a radio thread =
   `Sx126xRadio<VirtualBus>`. Extend hub → node frames with RSSI/SNR and a
   CRC flag. Run the whole container suite with `--radio virtual-sx1262`.
3. **Contract vectors.** One PHY profile, one payload set, expected
   TX airtime / RX / timeout / CAD outcomes, run against `HubRadio`, the virtual
   SX1262 and later hardware.
4. **Hardware.** Two boards first, five later (roadmap M8): RNode over USB for
   zero-firmware start, SPI HAT for the deployment shape. Same node binary.
5. **Calibration lab (optional).** `gr-lora_sdr` FER and capture tables → the
   hub's `sensitivity_dbm_at` and `capture_wins` become curves.

What this will validate: the protocol against a module's real state machine,
timing, half-duplex, CRC failures and the exact driver path. What it will not:
sea-state propagation, antenna placement, or a specific front end's behaviour
at the sensitivity floor. Those need §2.6 and, for repeatability, §2.3.

## 5. Sources

- lora-rs upstream: https://github.com/lora-rs/lora-rs (MIT); `RadioKind` /
  `InterfaceVariant`: `lora-phy/src/mod_traits.rs`; test emulator:
  `lora-phy/src/sx126x/test/{emulator.rs,fixtures.rs,mod.rs}`.
- gr-lora_sdr: https://github.com/tapparelj/gr-lora_sdr ; multi-user receiver:
  https://www.epfl.ch/labs/tcl/resources-and-sw/lora-multi-user-receiver/ ;
  collision decoding: https://github.com/jkadbear/gr-lora
- Meshtastic SimRadio: `src/platform/portduino/SimRadio.cpp` in
  https://github.com/meshtastic/firmware ; medium:
  https://github.com/meshtastic/Meshtasticator (INTERACTIVE_SIM.md)
- RNode: https://unsigned.io/software/RNode_Firmware.html ,
  https://github.com/liberatedsystems/RNode_Firmware_CE ; Rust host protocol:
  https://docs.rs/tulle (module `rnode`, trait `modem::Modem`)
- ryankurte: https://github.com/ryankurte/rust-radio (`radio::mock`),
  https://github.com/ryankurte/rust-radio-sx127x
- Network simulators: https://github.com/signetlabdei/lorawan ,
  https://github.com/Orange-OpenSource/elora , https://flora.aalto.fi/ ,
  https://github.com/mcbor/lorasim
- Renode wireless and peripheral modelling: https://renode.readthedocs.io/ ;
  AT86RF233 case study: doi:10.1109/induscon66435.2025.11241484
