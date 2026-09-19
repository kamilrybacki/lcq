// Seeed XIAO ESP32-S3 + Wio-SX1262 kit (the B2B-connector version).
//
// Pins confirmed by Meshtastic's `seeed_xiao_s3` variant and Seeed's
// one-channel-hub board file. The module switches its transmit path from
// DIO2 inside the chip and powers a 1.8 V TCXO from DIO3 -- the host's
// concern; the receive-side LNA enable, RXEN, is the one line the board
// drives itself.
#pragma once

#include "../board.h"

class XiaoS3WioSx1262 final : public Board {
 public:
  uint8_t id() const override { return 0x01; }
  const char* name() const override { return "xiao-esp32s3-wio-sx1262"; }

  const RadioPins& pins() const override {
    static const RadioPins wiring{/*nss*/ 41, /*sck*/ 7,   /*miso*/ 8,   /*mosi*/ 9,
                                  /*dio1*/ 39, /*busy*/ 40, /*nreset*/ 42};
    return wiring;
  }

  void begin() override {
    Board::begin();
    pinMode(RXEN, OUTPUT);
    digitalWrite(RXEN, LOW);
  }

  // RadioLib and Meshtastic drive RXEN high to receive and low otherwise.
  void rf_switch(RfMode mode) override { digitalWrite(RXEN, mode == RfMode::Rx ? HIGH : LOW); }

 private:
  static constexpr uint8_t RXEN = 38;
};
