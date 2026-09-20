# The bridge firmware

A Wio-SX1262 as a remote SPI device over USB. The host runs the unmodified
`lora-phy` SX126x driver -- the same driver that runs on the virtual chip
(D16) -- and this firmware is the wires on the far end of a USB CDC serial
port: SPI, BUSY, DIO1, NRESET and the receive-side antenna switch. It knows
nothing about LoRa, so the timing the node sees is the chip's own, not a
modem firmware's medium-access scheme (D23 says why that matters).

The protocol is specified in `src/infrastructure/sx126x/bridge.rs`, and the
reference device in that module -- the same protocol on the virtual chip --
is what this firmware must match byte for byte. The tests run against the
reference device; the boards run against this. Three places have to agree
and only two of them are tested, so after any change to the protocol, read
this sketch against the table in that module's documentation.

Two rules the protocol depends on and this firmware must keep (D23):

- **A DIO1 notice carries the level as the device reads it *after* the
  edge**, and the latch is cleared *before* the line is read, never after.
  The host treats a notice as a hint and confirms by reading the line; a
  notice that says the line has fallen is dropped.
- **The session id is drawn once at boot and never changes.** A host that
  sees it change knows the board restarted under it and stops using the
  radio rather than transmitting into a chip nothing configured.

## Layout

| File | What |
| --- | --- |
| `bridge.ino` | The protocol: framing, commands, the SPI transaction, DIO1 events. Names no pin. |
| `board.h` | The `Board` interface: pins, the SPI host, `begin()`, the antenna switch. |
| `board.cpp` | Which board this build is for, from the `ARDUINO_<board>` define the core sets for the FQBN. |
| `boards/xiao_s3_wio_sx1262.h` | Seeed XIAO ESP32-S3 + Wio-SX1262 kit (B2B-connector version): NSS 41, SCK 7, MISO 8, MOSI 9, DIO1 39, BUSY 40, NRESET 42, RXEN 38. |

A new board is one file under `boards/` and one `#elif` in `board.cpp`.

## Building

The esp32 Arduino core is several gigabytes, so the build runs in GitHub
Actions (`.github/workflows/firmware.yml`) on every push that touches
`firmware/`, and the build directory is published as the artifact
`bridge-xiao-esp32s3`. Download it from the workflow run, or:

```sh
gh run download --repo kamilrybacki/lcq --name bridge-xiao-esp32s3 --dir build
```

To build on a machine with the room for it:

```sh
arduino-cli config init --additional-urls https://espressif.github.io/arduino-esp32/package_esp32_index.json
arduino-cli core update-index
arduino-cli core install esp32:esp32@3.3.12
arduino-cli compile --fqbn esp32:esp32:XIAO_ESP32S3 --warnings all --export-binaries firmware/bridge
```

## Flashing

Only `esptool` is needed on the flashing machine (`pipx install esptool`).
Plug the XIAO in over USB-C; it enumerates as `/dev/ttyACM0` on Linux. If
the port does not appear, hold BOOT while pressing RESET to enter the
bootloader by hand.

With the artifact's `esp32.esp32.XIAO_ESP32S3` directory in `build/`, the
merged image (bootloader, partition table and application in one) goes at
offset zero:

```sh
esptool --chip esp32s3 --port /dev/ttyACM0 --baud 921600 write_flash 0x0 \
  build/esp32.esp32.XIAO_ESP32S3/bridge.ino.merged.bin
```

The separate images are there too: `bridge.ino.bootloader.bin` at `0x0`,
`bridge.ino.partitions.bin` at `0x8000`, `bridge.ino.bin` at `0x10000`.

## First contact

Not with a node. `lcq-bridge` asks the board what it is, one rung at a time,
so that a board which will not come up is one identifiable fault instead of
five possible ones:

```sh
lcq-bridge --port /dev/ttyACM0            # the bridge alone
lcq-bridge --port /dev/ttyACM0 --radio    # and then the chip under the driver
```

Then a node:

```sh
lcq-node --index 1 --fleet 3 --slots --journal ship1.log --radio bridge:/dev/ttyACM0
```

The node logs `bridge_up` with the protocol version, the firmware version,
the board id and the session, then `chip_up` once the driver has brought the
SX1262 into receive mode. Anything else is in `docs/HARDWARE-BRINGUP.md`.

## Serial port notes

- The XIAO's USB is the ESP32-S3's own USB-Serial-JTAG peripheral, so the
  baud rate is nominal and a host reconnect does not reset the board.
- Nothing is printed on the port but protocol frames. A terminal will show
  noise; that is expected.
