# Hardware bring-up: three XIAO ESP32-S3 + Wio-SX1262 kits

The runbook for the first boards (D23). Every step names what to expect, so
that what happens instead is a finding and not a mystery. Record findings in
`HANDOFF.md` as you go; the model's guesses that hardware corrects go into
`DECISIONS.md`.

## 0. Write down what you have

A measurement whose hardware is not identified is a rumour. Before anything
else, for each of the three boards, record in `HANDOFF.md`:

| What | Where it comes from |
| --- | --- |
| A label (0, 1, 2) and the USB serial number | `udevadm info /dev/ttyACM0 \| grep SERIAL` |
| XIAO and Wio board revisions | printed on the boards |
| Firmware artifact digest | `sha256sum build/esp32.esp32.XIAO_ESP32S3/bridge.ino.merged.bin` |
| Workflow run the artifact came from | its URL |
| Host commit | `git rev-parse HEAD` |
| Protocol, firmware, board and session id | the `hello` line of `lcq-bridge` |

A GitHub artifact is not by itself a chain of custody; the digest and the
run are.

## 0b. Before plugging anything in

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
- **Opening the port may reset the board.** The XIAO's USB is the ESP32-S3's
  own USB-Serial-JTAG peripheral, which does not reset on DTR the way a
  CP2102 or CH340 does, and nothing here asserts DTR or RTS. Expect the
  session id in `hello` to stay the same across reconnections; if it changes,
  the board is resetting when the port opens, and every host that was
  mid-operation will report `bridge_reset` rather than carrying on. That is
  the designed behaviour, not a fault -- but it means a bench script must not
  open the port twice.
- **Attenuation, concretely.** At 14 dBm with the supplied patch antennas,
  two boards on the same desk sit around -20 dBm at the receiver, which is
  above the front end's linear range. Either separate them (different rooms,
  or one outside) or, better, go conducted: u.FL to SMA pigtails, a 30 dB
  attenuator at each transmitter and a resistive splitter between them, which
  puts about -70 dBm at each receiver and makes capture and CRC thresholds
  repeatable. Never transmit into a board whose pigtail is disconnected.

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

## 2. The pre-flight, before any protocol

`lcq-node` can only say `chip_failed`, which confuses the firmware, the USB
link, the pin map, the module's power and the driver. `lcq-bridge` walks
those rungs one at a time and names the lowest one that failed. It is the
first thing to run on every board, and the same tool passes against the
reference device in the test suite, so a failure here is the board.

```sh
cargo build --release
./target/release/lcq-bridge --port /dev/ttyACM0
```

Expect nine `ok` lines: `hello`, `reset`, `busy low`, `get status`,
`get device errors`, `get irq status`, `dio1 reads low`, `rf switch`,
`round trip`. Record the `round trip` median and p95 -- that is the USB cost
every SPI transaction pays.

| The lowest failure | What it means |
| --- | --- |
| `hello` | Nothing speaks the protocol on that port: wrong port, or the flash did not take. |
| `reset` or `busy low` | The bridge is fine and the module is not: the board-to-board connector, or power. Try `--ldo`, then `--tcxo none`. |
| `get status` | SPI reaches nothing: the pin map, or a module that is not powered. |
| `get device errors` non-zero | The chip is up and unhappy: the error bits name the oscillator or the PLL, which usually means the TCXO setting. |
| `dio1 reads low` fails | DIO1 is high with nothing set: the line is misassigned or stuck. |
| `round trip` slow or erratic | The cable or the hub, not the radio. |

Then bring the chip up under the driver, still without LCQ:

```sh
./target/release/lcq-bridge --port /dev/ttyACM0 --radio
```

This reports `chip_up` or `chip_failed`, and only the driver and the module
are left to blame. Do both DC-DC and LDO here (`--ldo`) and both oscillator
settings (`--tcxo none`) before deciding what the defaults should be: the
review would not confirm from the schematic that the Wio-SX1262 carries the
DC-DC inductor, so the board must say. The fastest honest test is current
draw, in the table below.

## 3. First contact with a node: one board

```sh
./target/release/lcq-node --index 1 --fleet 3 --slots --journal ship1.log \
  --radio bridge:/dev/ttyACM0
```

Expect, in the first second of the log:

1. `bridge_up` with `protocol` `2`, `firmware` `0.2`, `board` `1` and a
   `session` -- the firmware answered `HELLO`.
2. `chip_up` with `phy` `eu868-sf10-v1` -- the driver reset the chip,
   configured it and put it in continuous receive.

What else can happen, and what it means:

| Log says | Meaning | Do |
| --- | --- | --- |
| `bridge on /dev/ttyACM0: no reply to command 0x01` | Nothing spoke the protocol on that port. | Wrong port, or the flash did not take: `ls /dev/ttyACM*`, reflash. |
| `bridge on ...: the device speaks protocol version N` | Firmware and adapter disagree. | Rebuild both from the same commit. |
| `bridge_reset` | The board restarted mid-operation; the radio is offline on purpose. | Power or cable. The node will not recover by itself, by design. |
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

Measure here, before anything else. Report a median, a p95 and a maximum,
not a single number: the tail is what a slot budget has to survive.

| Quantity | How | The model says |
| --- | --- | --- |
| USB round trip | `lcq-bridge`'s `round trip` step, then again with the largest SPI transaction the driver uses. | Well under a millisecond, against frames of a second. |
| `HELLO` to first SPI, `RESET` to BUSY low | The pre-flight's own timings. | Milliseconds. |
| Airtime | Sender's `SetTx` to `TxDone`, on a logic analyser on DIO1. | 122 bytes at SF10/125 kHz/CR 4/5, 8 symbols of preamble: about 1.19 s. |
| DIO1 edge to host event to `GetIrqStatus` | Log all three; the gaps are the bridge's cost. | A few milliseconds each. |
| DIO1 high to `ClearIrqStatus` to DIO1 low | Logic analyser. | The line must fall; if it does not, the driver loops. |
| Earliest catchable preamble | `SetRx` against a frame already on the air, as a matrix over preambles of 8 and 12 symbols, not one number. | The model's lock window is preamble minus six symbols, floor 5 ms (D18). |
| Capture and sensitivity | Step the attenuation; note where `crc_errors` start and where a stronger frame wins against a weaker one. | Capture threshold 6 dB, noise floor -117 dBm (D18, guessed). |
| `RxDone`, `CrcErr`, `HeaderErr` | For each: the IRQ flags, whether DIO1 rose, whether the payload could be read, the RSSI and SNR, and which event the node logged. | The three outcomes of D19. |
| Half-duplex overlap | A frame starting before, during and after the board's own transmission. | Deaf for the whole of its own airtime. |
| CAD | `CadDone` and `CadDetected` latency against silence, a preamble, a payload with no preamble, and a signal too weak. | The CAD model of D19. |
| Current draw | A meter in the USB line: sleep, standby RC, standby XOSC, receive with RXEN low and high, transmit at 14 dBm. | The fastest honest test of DC-DC against LDO, and of whether RXEN does anything. |
| Supply and warmth | USB voltage and board temperature over 20 to 30 transmissions in a row. | Three kits on one hub may not behave like one kit on a good cable. |

## 4. A round: three boards

```sh
./target/release/lcq-node --index 1 --fleet 3 --slots --journal ship1.log --radio bridge:/dev/ttyACM0
./target/release/lcq-node --index 2 --fleet 3 --slots --journal ship2.log --radio bridge:/dev/ttyACM1
./target/release/lcq-node --index 0 --fleet 3 --slots --journal ship0.log --radio bridge:/dev/ttyACM2 --trigger
```

The three nodes above all count the same because `--fleet 3` says so. To
give them different standing, hand every one the same manifest instead
(D25); `docs/three-vessels.manifest` is ready to copy:

```sh
./target/release/lcq-node --index 0 --manifest fleet.manifest --slots \
  --journal ship0.log --radio bridge:/dev/ttyACM2 --trigger
```

The fleet's size, its members' names and its mission epoch then come from the
file, and the logs name the vessels rather than `n0`. Worth doing on day one
even with equal competences, because it is the path a real fleet takes and it
exercises the epoch the manifest names.

With a fleet of three the threshold is three: every vote is needed, and a
single lost frame goes to repair. Expect: the trigger, each member's vote in
its slot, acknowledgements, and a `final` line on every node with
`supporters` `3`, `threshold` `3`, `competence` equal to `total_competence`
(both thresholds are reported, because "everyone voted" and "not enough of
the fleet's competence" are different answers), `endorsed` `true`,
`verifications` around
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

## 5b. Break it on purpose

A first day that only records successes has not qualified anything. Each of
these has a designed answer; if the board does something else, that is a
finding.

| Do | Expect |
| --- | --- |
| Unplug the USB cable while receiving | The node stops; nothing claims a frame it did not get. |
| Unplug it mid-transmission | `chip_tx_failed`, then silence. No recovery, no half state. |
| Press the XIAO's reset while a node runs | `bridge_reset`, and the radio stays offline until the node is restarted. |
| Force an interrupt, then watch DIO1 | Notice arrives, host reads the flags, `ClearIrqStatus`, and the line actually returns low. A line that stays high is a permanent-interrupt loop waiting to happen. |
| Unseat the radio module, then run `lcq-bridge` | `busy low` fails with BUSY stuck. The host must report it, not retry forever. |
| Run two `lcq-bridge` against the same port | The second should find a busy port. Two readers on one cable is a bench mistake worth recognising. |

## 6. What to bring back to the repository

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
