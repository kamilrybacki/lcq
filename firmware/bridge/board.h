// The board under the bridge: its pins, and the few things that differ from
// one module to the next. The protocol code in bridge.ino knows only this
// interface; a new board is one file under boards/ and one line in board.cpp.
#pragma once

#include <Arduino.h>
#include <SPI.h>

// The lines every SX126x module exposes, as GPIO numbers.
struct RadioPins {
  uint8_t nss;
  uint8_t sck;
  uint8_t miso;
  uint8_t mosi;
  uint8_t dio1;
  uint8_t busy;
  uint8_t nreset;
};

// What the host asks of an external antenna switch. The values are the
// protocol's RF modes.
enum class RfMode : uint8_t { Off = 0, Rx = 1, Tx = 2 };

class Board {
 public:
  virtual ~Board() = default;

  // Reported in HELLO; the ids are listed in bridge.rs.
  virtual uint8_t id() const = 0;
  virtual const char* name() const = 0;
  virtual const RadioPins& pins() const = 0;

  // The SPI host the chip hangs off: the default one unless a board says
  // otherwise.
  virtual SPIClass& spi() { return SPI; }

  // Configure the pins and the SPI host. A board with more lines overrides
  // this and calls it first.
  virtual void begin();

  // Steer an external antenna switch, if the board has one the host must
  // drive. A module that switches from DIO2 alone needs nothing here.
  virtual void rf_switch(RfMode) {}
};

// The board this firmware was built for, chosen at compile time in board.cpp.
Board& board();
