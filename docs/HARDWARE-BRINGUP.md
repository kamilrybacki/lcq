# Hardware bring-up: three XIAO ESP32-S3 + Wio-SX1262 kits

The runbook for the first boards (D23). Every step names what to expect, so
that what happens instead is a finding and not a mystery. Record findings in
`HANDOFF.md` as you go; the model's guesses that hardware corrects go into
`DECISIONS.md`.

## 0. Before plugging anything in

- **The kit, not the bare module.** The XIAO ESP32-S3 & Wio-SX1262 *Kit*
  joins the two boards with a 30-pin board-to-board connector. The standalone
  "Wio-SX1262 for XIAO" has the same name and a different pinout; the board
  file under `firmware/bridge/boards/` is for the kit.
- **Antenna first.** Screw the 868 MHz antenna onto the u.FL pigtail before
  the first transmission. A radio transmitting into nothing can damage its
  power amplifier. The small antenna in the box is, in Seeed's words, for
  testing only.
- **Desk distance.** At the profile's 14 dBm, two boards a hand apart drive
  each other's front ends into saturation and the CRC errors that come with
  it. Keep them two metres or more apart, or put attenuators in the antenna
  path. If CRC errors appear at an RSSI above about -30 dBm, that is why.
- **Duty cycle.** 868.1 MHz sits in a 1 % band: 36 s of airtime per hour per
  transmitter, about thirty frames of the protocol's size. Runs are short;
  the node's `airtime_ms` meter says where you are.
- **Serial permissions.** The board enumerates as `/dev/ttyACM0` (then `1`,
  `2`). Your user needs to be in the `dialout` group, or the port needs a udev
  rule.

## 1. Flash the bridge firmware

The `firmware` workflow builds it; download the artifact of the latest green
run and flash the merged image at offset zero:

```sh
gh run download --repo kamilrybacki/lcq --name bridge-xiao-esp32s3 --dir build
pipx install esptool
esptool --chip esp32s3 --port /dev/ttyACM0 --baud 921600 write_flash 0x0 \
  build/esp32.esp32.XIAO_ESP32S3/bridge.ino.merged.bin
```

Expect `Hash of data verified` and a reset. If `esptool` cannot open the
port, hold BOOT while pressing RESET on the XIAO to enter the bootloader by
hand, then flash again. Repeat for the three boards; label them 0, 1, 2.

## 2. First contact: one board

```sh
cargo build --release
./target/release/lcq-node --index 1 --fleet 3 --slots --journal ship1.log \
  --radio bridge:/dev/ttyACM0
```

Expect, in the first second of the log:

1. `bridge_up` with `firmware` `0.1` and `board` `1` -- the firmware answered
   `HELLO`.
2. `chip_up` with `phy` `eu868-sf10-v1` -- the driver reset the chip,
   configured it and put it in continuous receive.

What else can happen, and what it means:

| Log says | Meaning | Do |
| --- | --- | --- |
| `bridge on /dev/ttyACM0: no reply to command 0x01` | Nothing spoke the protocol on that port. | Wrong port, or the flash did not take: `ls /dev/ttyACM*`, reflash. |
| `bridge on ...: the device speaks protocol version N` | Firmware and adapter disagree. | Rebuild both from the same commit. |
| `chip_failed` with `init: Busy` | BUSY never fell after reset: the chip is not there, or not powered as the driver assumed. | Check the board-to-board connector is seated. Try `,ldo` (no DC-DC), then `,tcxo=none`. |
| `chip_failed` with `init: SPI` | The bridge refused a transaction. | The firmware's `EVENT_ERROR` code is in the note; likely a wiring or board-file mismatch. |
| `chip_up` never comes, `chip_rx_failed` repeats | The driver's receive setup errors. | Note the error variant; it is a driver-level question. |

A `chip_up` on the first board is the qualification of the bridge itself.
Leave it running; the process should stay quiet, with no `chip_missed`.

## 3. A frame across the desk: two boards

Two terminals, two ports, one host:

```sh
./target/release/lcq-node --index 1 --fleet 3 --slots --journal ship1.log --radio bridge:/dev/ttyACM0
./target/release/lcq-node --index 2 --fleet 3 --slots --journal ship2.log --radio bridge:/dev/ttyACM1
```

Then a third process opens a round on a third board (below), or -- with only
two boards on the desk -- start member 2 with `--trigger` so it sends the
trigger itself. Expect on the sender a `sent` line and no
`chip_tx_failed`; on the other board an `admitted` line naming the frame's
author and sequence, with an RSSI between about -30 and -80 dBm at desk
distance and an SNR near +10 dB where the log carries the signal report.

Measure here, before anything else:

| Quantity | How | The model says |
| --- | --- | --- |
| SPI round trip | Time `chip_up` from `bridge_up` in the log: it is about sixty transactions. | Under a millisecond each. |
| Airtime | Sender's `SetTx` to `TxDone`, from the driver thread's timestamps or a logic analyser on DIO1. | 122 bytes at SF10/125 kHz/CR 4/5, 8 symbols of preamble: about 1.19 s. |
| DIO1 to host | Receiver's `RxDone` (DIO1 edge) to the `admitted` log line. | A few milliseconds: one USB event plus three transactions, then the signature check. |
| Sensitivity | Step the boards apart; note the RSSI at which `crc_errors` start. | The profile's sensitivity minus a margin. |

## 4. A round: three boards

```sh
./target/release/lcq-node --index 1 --fleet 3 --slots --journal ship1.log --radio bridge:/dev/ttyACM0
./target/release/lcq-node --index 2 --fleet 3 --slots --journal ship2.log --radio bridge:/dev/ttyACM1
./target/release/lcq-node --index 0 --fleet 3 --slots --journal ship0.log --radio bridge:/dev/ttyACM2 --trigger
```

With a fleet of three the threshold is three: every vote is needed, and a
single lost frame goes to repair. Expect: the trigger, each member's vote in
its slot, acknowledgements, and a `final` line on every node with
`supporters` `3`, `threshold` `3`, `endorsed` `true`, `verifications` around
a dozen, `crc_errors` `0`, `chip_missed` `0`, `splits` `0`. Compare the
meters with a
`LCQ_RADIO=sx1262` container run of the same fleet size; the virtual chip's
numbers are the prediction, the boards' are the measurement.

Then, deliberately:

- Start member 2 a slot late (`--offset`, or by hand) and watch repair fetch
  its vote.
- Move one board out of range and watch the round declare what it can.
- Run it ten times and keep the `splits` column: a split with three boards
  on a desk is a timing finding, not a fleet finding.

## 5. What to bring back to the repository

- The measured airtime and DIO1 latency against the profile's numbers, in
  `HANDOFF.md`.
- Any change to `preamble_symbols_to_lock` or `capture_threshold_db` the
  boards force, as a new profile name (D18: a frozen profile never changes
  under its name).
- Whether DC-DC and the 1.8 V TCXO were right for the Wio-SX1262 (the
  defaults of `--radio bridge:`); if not, the defaults change in
  `BridgeOptions::default` and this file.
- Anything the firmware did that the reference device does not: that is a
  firmware bug, fixed in `firmware/bridge`, and the reference stays the
  specification.
