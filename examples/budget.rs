//! Where the airtime actually goes, and what each proposed saving is worth.
//!
//! Under slotted access the channel is busy most of the round, so further gains
//! have to come from fewer frames or smaller ones. This measures which.

use lcq::sim::{DUTY_CYCLE_BUDGET_MS, Scenario, airtime_ms};

/// Sizes taken from the wire format, not estimated.
const SIGNATURE_BYTES: usize = 64;
const CONTENT_HASH_BYTES: usize = 32;
const FRAME_BYTES: usize = 105;

fn main() {
    println!(
        "Ramka poparcia: {FRAME_BYTES} B, {} ms na antenie",
        airtime_ms(FRAME_BYTES)
    );
    println!();
    println!("{:<34}{:>8}{:>8}", "skladnik", "bajty", "udzial");
    println!("{}", "-".repeat(50));
    let other = FRAME_BYTES - SIGNATURE_BYTES - CONTENT_HASH_BYTES;
    for (name, bytes) in [
        ("podpis ed25519", SIGNATURE_BYTES),
        ("skrot tresci sprawy", CONTENT_HASH_BYTES),
        ("reszta naglowka", other),
    ] {
        #[allow(clippy::cast_precision_loss)]
        let share = 100.0 * bytes as f64 / FRAME_BYTES as f64;
        println!("{name:<34}{bytes:>8}{share:>7.0}%");
    }

    println!();
    println!("Co dalaby kazda zmiana, dla floty 10 jednostek:");
    println!();
    println!(
        "{:<40}{:>9}{:>11}{:>15}",
        "wariant", "B/ramke", "ms/ramke", "runda 10 [s]"
    );
    println!("{}", "-".repeat(75));

    // A short case tag instead of the full hash. The signature still covers the
    // full 32 bytes -- the receiver reconstructs them from the case it already
    // knows -- so nothing is truncated and no security is traded away.
    let variants: [(&str, usize, usize); 4] = [
        ("dzis", FRAME_BYTES, 10),
        ("krotki znacznik sprawy (4 B)", FRAME_BYTES - 28, 10),
        ("+ stop po osiagnieciu progu", FRAME_BYTES - 28, 8),
        ("+ agregacja BLS (1 ramka)", 48 + 2 * 10 + 13, 1),
    ];
    for (name, bytes, frames) in variants {
        let air = airtime_ms(bytes);
        #[allow(clippy::cast_precision_loss)]
        let round = (air * frames as u64) as f64 / 1000.0;
        println!("{name:<40}{bytes:>9}{air:>11}{round:>15.1}");
    }

    println!();
    println!("Laczenie kilku podpisow w jedna ramke NIE pomaga:");
    for votes in [1usize, 3, 5] {
        let bytes = 12 + votes * (SIGNATURE_BYTES + 2);
        if bytes > 255 {
            println!("  {votes} glosow: {bytes} B -- nie miesci sie w LoRa");
            continue;
        }
        let air = airtime_ms(bytes);
        #[allow(clippy::cast_precision_loss)]
        let per_vote = air as f64 / votes as f64;
        println!("  {votes} glosow w ramce: {bytes} B, {air} ms, czyli {per_vote:.0} ms na glos");
    }
    println!("  (czas na antenie jest prawie liniowy wzgledem ladunku, wiec");
    println!("   laczenie amortyzuje tylko preambule)");

    println!();
    println!("Koszt braku potwierdzen na antenie, flota 10, sloty:");
    let with_ack = Scenario::new(10).with_slots(200).run();
    let without = Scenario::new(10)
        .with_slots(200)
        .without_acknowledgement()
        .run();
    println!(
        "  z potwierdzeniem:  {:>2} ramek, {:>5.1} s anteny na wezel",
        with_ack.frames_sent,
        per_node(&with_ack, 10)
    );
    println!(
        "  bez potwierdzenia: {:>2} ramek, {:>5.1} s anteny na wezel",
        without.frames_sent,
        per_node(&without, 10)
    );
    // Measured, and it corrected the guess that came first: the waste never
    // costs the quorum, because under slots the first attempt already landed.
    // It costs how OFTEN a fleet may decide, which the regulation meters.
    let decisions = |airtime: u64| DUTY_CYCLE_BUDGET_MS * 10 / airtime.max(1);
    println!(
        "  decyzji na godzine na wezel: {} z potwierdzeniem, {} bez",
        decisions(with_ack.airtime_ms),
        decisions(without.airtime_ms)
    );
    println!("  (kworum zapada w obu przypadkach -- brak ack kosztuje antene, nie poprawnosc)");
}

#[allow(clippy::cast_precision_loss)]
fn per_node(report: &lcq::sim::Report, fleet: usize) -> f64 {
    report.airtime_ms as f64 / fleet as f64 / 1000.0
}
