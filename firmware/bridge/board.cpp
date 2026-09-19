// Which board this firmware is for. The Arduino core defines
// ARDUINO_<board> from the FQBN it is compiled with; an unknown board is a
// build error, never a guess at pins.
#include "board.h"

#if defined(ARDUINO_XIAO_ESP32S3)
#include "boards/xiao_s3_wio_sx1262.h"
static XiaoS3WioSx1262 the_board;
#else
#error "No board definition for this FQBN: add one under boards/ and select it here."
#endif

Board& board() { return the_board; }

void Board::begin() {
  const RadioPins& p = pins();
  pinMode(p.nss, OUTPUT);
  digitalWrite(p.nss, HIGH);
  pinMode(p.nreset, OUTPUT);
  digitalWrite(p.nreset, HIGH);
  pinMode(p.busy, INPUT);
  pinMode(p.dio1, INPUT);
  // NSS is driven by hand so it can stay low across a whole transaction.
  spi().begin(p.sck, p.miso, p.mosi, -1);
}
