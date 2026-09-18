//! What a spreading factor actually costs, for one endorsement frame.
//!
//! Range is not free: every step up doubles the symbol time while buying only
//! about 2.5 dB of link budget. The question is never "does the frame fit" --
//! raw `LoRa` carries 255 bytes at any spreading factor -- but what the hourly
//! airtime budget buys at each one.

use lorai::domain::contracts::Subject;
use lorai::domain::time::Timestamp;
use lorai::sim::{DUTY_CYCLE_BUDGET_MS, airtime_ms_at, sensitivity_dbm_at};
use lorai::wire::{CompactEnvelope, SigningKey, encode_compact};

/// Loss grows this fast with distance in the two-ray far field over water.
const DB_PER_DOUBLING: f64 = 12.04;

fn main() {
    let subject = Subject::new("sim", "evt-1", 0, [0x11; 32], Timestamp::from_secs(0))
        .expect("valid subject");
    let key = SigningKey::from_seed([7u8; 32]);
    let envelope = CompactEnvelope::new(
        1,
        1,
        0,
        *subject.content_hash(),
        subject.started_at().as_secs(),
        0,
        3,
        1,
        0,
    );
    let frame = encode_compact(&envelope.sign(&key)).expect("encodes");
    let reference = sensitivity_dbm_at(10);

    println!(
        "ramka poparcia: {} B (skompaktowana, podpisana)",
        frame.len()
    );
    println!();
    println!(
        "{:<5}{:>10}{:>12}{:>10}{:>14}{:>14}",
        "SF", "czulosc", "antena[ms]", "ramek/h", "runda 10 [s]", "zasieg vs SF10"
    );
    println!("{}", "-".repeat(65));

    for sf in 7..=12u8 {
        let air = airtime_ms_at(sf, frame.len());
        let per_hour = DUTY_CYCLE_BUDGET_MS / air.max(1);
        // One endorsement round for ten members with no retries and no
        // collisions: the floor, not a forecast.
        #[allow(clippy::cast_precision_loss)]
        let round = (air * 10) as f64 / 1000.0;
        let reach = 2f64.powf((reference - sensitivity_dbm_at(sf)) / DB_PER_DOUBLING);
        let sensitivity = sensitivity_dbm_at(sf);
        println!("{sf:<5}{sensitivity:>10.1}{air:>12}{per_hour:>10}{round:>14.1}{reach:>13.2}x");
    }

    println!();
    println!("ramek/h = ile sie miesci w limicie 1 % (36 s/h) na wezel.");
    println!(
        "zasieg = z czulosci datasheetu SX1276, przy 12 dB na podwojenie odleglosci nad woda."
    );
    println!();
    println!("Przekaz zamiast wolniejszego SF:");
    let sf10 = airtime_ms_at(10, frame.len());
    let sf12 = airtime_ms_at(12, frame.len());
    #[allow(clippy::cast_precision_loss)]
    let ratio = sf12 as f64 / (2.0 * sf10 as f64);
    let sf12_reach = 2f64.powf((reference - sensitivity_dbm_at(12)) / DB_PER_DOUBLING);
    println!("  1 skok SF12:  {sf12} ms, zasieg {sf12_reach:.2}x");
    println!("  2 skoki SF10: {} ms, zasieg 2.00x", 2 * sf10);
    println!("  SF12 kosztuje {ratio:.2}x wiecej anteny za mniejszy zasieg.");
}
