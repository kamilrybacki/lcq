// LCQ bridge: a Wio-SX1262 as a remote SPI device over USB.
//
// The host runs the unmodified `lora-phy` SX126x driver; this board is the
// wires -- SPI, BUSY, DIO1, NRESET and the receive-side antenna switch -- on
// the far end of a USB CDC serial port. Nothing here knows LoRa: it clocks
// bytes and reads pins, so the timing the host sees is the chip's, not a
// modem firmware's. The protocol is specified in the host adapter,
// `src/infrastructure/sx126x/bridge.rs`; this file mirrors it byte for byte,
// and the reference device in that module is what this firmware must match.
//
// Boards live under boards/ behind the Board interface in board.h; this file
// never names a pin. Build and flash: README.md next to this file.

#include <SPI.h>
#include <esp_random.h>

#include "board.h"

namespace {

// Well under the SX126x's 16 MHz ceiling.
constexpr uint32_t SPI_HZ = 2000000;

// Version 2 added the session id to the HELLO reply.
constexpr uint8_t PROTOCOL_VERSION = 2;
constexpr uint8_t FIRMWARE_MAJOR = 0;
constexpr uint8_t FIRMWARE_MINOR = 2;

// KISS framing: FEND delimits a frame; FEND and FESC inside it are escaped.
constexpr uint8_t FEND = 0xC0;
constexpr uint8_t FESC = 0xDB;
constexpr uint8_t TFEND = 0xDC;
constexpr uint8_t TFESC = 0xDD;

// Requests. A reply is the request byte with the high bit set.
constexpr uint8_t CMD_HELLO = 0x01;
constexpr uint8_t CMD_RESET = 0x02;
constexpr uint8_t CMD_BUSY = 0x03;
constexpr uint8_t CMD_PINS = 0x04;
constexpr uint8_t CMD_RF = 0x05;
constexpr uint8_t CMD_SPI = 0x06;
constexpr uint8_t CMD_EVENTS = 0x07;
constexpr uint8_t REPLY = 0x80;
// Events: sent on the device's own initiative, never a reply.
constexpr uint8_t EVENT_DIO1 = 0xE1;
constexpr uint8_t EVENT_ERROR = 0xEE;

// Status bytes.
constexpr uint8_t STATUS_OK = 0;
constexpr uint8_t STATUS_TIMEOUT = 1;
constexpr uint8_t STATUS_BAD_REQUEST = 2;
constexpr uint8_t STATUS_TOO_LONG = 3;
constexpr uint8_t STATUS_UNKNOWN_COMMAND = 4;

// Operations inside one SPI transaction: NSS stays low across all of them.
constexpr uint8_t OP_WRITE = 0;
constexpr uint8_t OP_READ = 1;
constexpr uint8_t OP_TRANSFER = 2;
constexpr uint8_t OP_DELAY = 3;

// RF switch modes.
constexpr uint8_t RF_OFF = 0;
constexpr uint8_t RF_RX = 1;
constexpr uint8_t RF_TX = 2;

// The most a decoded frame may hold either way: the chip's 256-byte buffer
// and its command overhead, with room to spare.
constexpr size_t FRAME_LIMIT = 1100;
// How long BUSY may stay high before the chip is declared stuck.
constexpr uint32_t BUSY_LIMIT_MS = 100;

uint8_t request[FRAME_LIMIT];
size_t request_len = 0;
bool in_frame = false;
bool escaped = false;
bool overflow = false;

uint8_t reply[FRAME_LIMIT];
uint8_t encoded[2 * FRAME_LIMIT + 4];

// The board's pins, copied once in setup().
RadioPins pins;

// Drawn once at boot and never again: the host compares it to tell a board
// that restarted mid-operation from one that has merely gone quiet. Never
// zero, so that "no session yet" stays distinguishable on the host.
uint32_t session = 0;

volatile bool dio1_rose = false;
bool events_on = false;

void IRAM_ATTR on_dio1() { dio1_rose = true; }

void send_frame(uint8_t command, const uint8_t* payload, size_t len) {
  size_t n = 0;
  encoded[n++] = FEND;
  encoded[n++] = command;
  for (size_t i = 0; i < len; i++) {
    uint8_t byte = payload[i];
    if (byte == FEND) {
      encoded[n++] = FESC;
      encoded[n++] = TFEND;
    } else if (byte == FESC) {
      encoded[n++] = FESC;
      encoded[n++] = TFESC;
    } else {
      encoded[n++] = byte;
    }
  }
  encoded[n++] = FEND;
  Serial.write(encoded, n);
}

void reply_status(uint8_t command, uint8_t status) {
  send_frame(command | REPLY, &status, 1);
}

void report_error(uint8_t code, uint8_t command) {
  uint8_t body[2] = {code, command};
  send_frame(EVENT_ERROR, body, sizeof body);
}

bool wait_busy_low(uint32_t limit_ms) {
  uint32_t start = millis();
  while (digitalRead(pins.busy) == HIGH) {
    if (millis() - start > limit_ms) return false;
    delayMicroseconds(50);
  }
  return true;
}

void handle_hello() {
  // Clear the latch first and read the line second. An edge arriving between
  // the two is then still latched and reported as an event; the other order
  // would read it into the reply and then throw the latch away, losing it.
  events_on = true;
  dio1_rose = false;
  uint8_t busy = (uint8_t)digitalRead(pins.busy);
  uint8_t level = (uint8_t)digitalRead(pins.dio1);
  uint8_t body[10] = {PROTOCOL_VERSION,
                      FIRMWARE_MAJOR,
                      FIRMWARE_MINOR,
                      board().id(),
                      (uint8_t)(session & 0xFF),
                      (uint8_t)((session >> 8) & 0xFF),
                      (uint8_t)((session >> 16) & 0xFF),
                      (uint8_t)((session >> 24) & 0xFF),
                      busy,
                      level};
  send_frame(CMD_HELLO | REPLY, body, sizeof body);
}

void handle_reset() {
  // DS.SX1261-2 8.1: NRESET low for at least 100 us, then the chip comes
  // out of reset with BUSY high until it is ready.
  digitalWrite(pins.nreset, LOW);
  delay(2);
  digitalWrite(pins.nreset, HIGH);
  delay(5);
  dio1_rose = false;
  reply_status(CMD_RESET, wait_busy_low(BUSY_LIMIT_MS) ? STATUS_OK : STATUS_TIMEOUT);
}

void handle_busy(const uint8_t* p, size_t len) {
  if (len != 2) {
    reply_status(CMD_BUSY, STATUS_BAD_REQUEST);
    return;
  }
  uint32_t limit = p[0] | (uint32_t)p[1] << 8;
  reply_status(CMD_BUSY, wait_busy_low(limit) ? STATUS_OK : STATUS_TIMEOUT);
}

void handle_pins() {
  uint8_t body[2] = {(uint8_t)digitalRead(pins.busy), (uint8_t)digitalRead(pins.dio1)};
  send_frame(CMD_PINS | REPLY, body, sizeof body);
}

void handle_rf(const uint8_t* p, size_t len) {
  if (len != 1 || p[0] > RF_TX) {
    reply_status(CMD_RF, STATUS_BAD_REQUEST);
    return;
  }
  board().rf_switch(static_cast<RfMode>(p[0]));
  reply_status(CMD_RF, STATUS_OK);
}

void handle_events(const uint8_t* p, size_t len) {
  if (len != 1) {
    reply_status(CMD_EVENTS, STATUS_BAD_REQUEST);
    return;
  }
  // Clear before reading, as in handle_hello().
  events_on = p[0] != 0;
  dio1_rose = false;
  uint8_t level = digitalRead(pins.dio1);
  send_frame(CMD_EVENTS | REPLY, &level, 1);
}

// Check the operation list and add up what it reads back; -1 if malformed.
long spi_read_total(const uint8_t* p, size_t len) {
  size_t i = 0;
  size_t total = 0;
  while (i < len) {
    if (len - i < 3) return -1;
    uint8_t kind = p[i];
    size_t n = p[i + 1] | (size_t)p[i + 2] << 8;
    i += 3;
    switch (kind) {
      case OP_WRITE:
        if (len - i < n) return -1;
        i += n;
        break;
      case OP_READ:
        total += n;
        break;
      case OP_TRANSFER:
        if (len - i < n) return -1;
        i += n;
        total += n;
        break;
      case OP_DELAY:
        break;
      default:
        return -1;
    }
  }
  return (long)total;
}

void handle_spi(const uint8_t* p, size_t len) {
  long total = spi_read_total(p, len);
  if (total < 0) {
    reply_status(CMD_SPI, STATUS_BAD_REQUEST);
    return;
  }
  if ((size_t)total + 1 > FRAME_LIMIT) {
    reply_status(CMD_SPI, STATUS_TOO_LONG);
    return;
  }
  // DS.SX1261-2 8.3.1: NSS may only fall while BUSY is low.
  if (!wait_busy_low(BUSY_LIMIT_MS)) {
    reply_status(CMD_SPI, STATUS_TIMEOUT);
    return;
  }
  reply[0] = STATUS_OK;
  size_t out = 1;
  SPIClass& spi = board().spi();
  spi.beginTransaction(SPISettings(SPI_HZ, MSBFIRST, SPI_MODE0));
  digitalWrite(pins.nss, LOW);
  size_t i = 0;
  while (i < len) {
    uint8_t kind = p[i];
    size_t n = p[i + 1] | (size_t)p[i + 2] << 8;
    i += 3;
    switch (kind) {
      case OP_WRITE:
        spi.writeBytes(p + i, n);
        i += n;
        break;
      case OP_READ:
        // The chip clocks its answer out against NOPs.
        memset(reply + out, 0, n);
        spi.transferBytes(reply + out, reply + out, n);
        out += n;
        break;
      case OP_TRANSFER:
        spi.transferBytes(p + i, reply + out, n);
        i += n;
        out += n;
        break;
      case OP_DELAY:
        delayMicroseconds(n);
        break;
    }
  }
  digitalWrite(pins.nss, HIGH);
  spi.endTransaction();
  send_frame(CMD_SPI | REPLY, reply, out);
}

void handle_frame(const uint8_t* frame, size_t len) {
  uint8_t command = frame[0];
  const uint8_t* p = frame + 1;
  size_t n = len - 1;
  switch (command) {
    case CMD_HELLO: handle_hello(); break;
    case CMD_RESET: handle_reset(); break;
    case CMD_BUSY: handle_busy(p, n); break;
    case CMD_PINS: handle_pins(); break;
    case CMD_RF: handle_rf(p, n); break;
    case CMD_SPI: handle_spi(p, n); break;
    case CMD_EVENTS: handle_events(p, n); break;
    default: report_error(STATUS_UNKNOWN_COMMAND, command); break;
  }
}

void feed(uint8_t byte) {
  if (byte == FEND) {
    if (in_frame && overflow) {
      report_error(STATUS_TOO_LONG, request_len > 0 ? request[0] : 0);
    } else if (in_frame && request_len > 0) {
      handle_frame(request, request_len);
    }
    request_len = 0;
    in_frame = true;
    escaped = false;
    overflow = false;
    return;
  }
  if (!in_frame) return;
  uint8_t decoded = byte;
  if (escaped) {
    escaped = false;
    decoded = byte == TFEND ? FEND : byte == TFESC ? FESC : byte;
  } else if (byte == FESC) {
    escaped = true;
    return;
  }
  if (request_len >= FRAME_LIMIT) {
    overflow = true;
    return;
  }
  request[request_len++] = decoded;
}

}  // namespace

void setup() {
  // esp_random() is a true random source once the RF subsystem is up; at
  // this point it is seeded well enough for an identifier, and the low bit
  // is forced so that the value is never zero.
  session = esp_random() | 1u;
  board().begin();
  pins = board().pins();
  attachInterrupt(digitalPinToInterrupt(pins.dio1), on_dio1, RISING);
  // USB CDC: the rate is nominal. The buffers must hold a whole transaction
  // while the loop is busy with the previous one.
  Serial.setRxBufferSize(2048);
  Serial.setTxBufferSize(2048);
  Serial.begin(115200);
  Serial.setTimeout(0);
}

void loop() {
  int available = Serial.available();
  while (available-- > 0) {
    int byte = Serial.read();
    if (byte >= 0) feed((uint8_t)byte);
  }
  if (events_on && dio1_rose) {
    dio1_rose = false;
    uint8_t level = digitalRead(pins.dio1);
    send_frame(EVENT_DIO1, &level, 1);
  }
}
