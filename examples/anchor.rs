//! Where a slot schedule counts from, and what each choice costs.
//!
//! The two candidate anchors fail in opposite directions, and neither is free.
//! This measures the clock-skew half; the equivocation half cannot be measured,
//! only reasoned about, and is recorded in `DECISIONS.md` D5.

use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;
use lcq::sim::{Deliberation, SlotAnchor};

const GUARD_MS: u64 = 200;

/// Enough draws that one lucky arrangement of clock errors cannot carry a row.
const SEEDS: u64 = 20;

fn main() {
    println!("Budzet skosu zegarow: {MAX_CLOCK_SKEW_SECONDS} s (parami)");
    println!();
    println!(
        "{:<16}{:>8}{:>10}{:>10}{:>10}{:>10}",
        "kotwica", "skos[s]", "ramki", "zderz", "antena[s]", "kworum"
    );
    println!("{}", "-".repeat(66));

    for anchor in [SlotAnchor::Trigger, SlotAnchor::Clock] {
        let name = match anchor {
            SlotAnchor::Trigger => "wyzwalacz",
            SlotAnchor::Clock => "zegar",
        };
        for skew in [0u64, 2, 10, MAX_CLOCK_SKEW_SECONDS] {
            // Averaged over seeds: one draw of the clock errors says little,
            // because whether two members land on each other is exactly what
            // the draw decides.
            let mut frames = 0usize;
            let mut collided = 0usize;
            let mut airtime = 0u64;
            let mut blocked = 0usize;
            for seed in 1..=SEEDS {
                let report = Deliberation::new(10)
                    .with_slots(GUARD_MS)
                    .with_anchor(anchor)
                    .with_clock_skew_s(skew)
                    .with_seed(seed)
                    .run();
                frames += report.stages.iter().map(|s| s.frames_sent).sum::<usize>();
                collided += report.stages.iter().map(|s| s.collided).sum::<usize>();
                airtime += report.airtime_ms;
                if !report.endorsed {
                    blocked += 1;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let seeds = SEEDS as f64;
            #[allow(clippy::cast_precision_loss)]
            let (f, c, a) = (
                frames as f64 / seeds,
                collided as f64 / seeds,
                airtime as f64 / seeds / 1000.0,
            );
            println!(
                "{name:<16}{skew:>8}{f:>10.1}{c:>10.1}{a:>10.1}{:>10}",
                if blocked == 0 { "TAK" } else { "nie x" }
            );
        }
    }

    println!();
    println!("Kotwica w wyzwalaczu jest odporna na skos: liczy sie od ramki, ktora");
    println!("wszyscy uslyszeli, z dokladnoscia demodulatora. Ale przejety czlonek moze");
    println!("rozeslac rozne, poprawnie podpisane wyzwalacze i rozdzielic flote.");
    println!();
    println!("Kotwica w zegarze jest kanoniczna -- nie ma czego rozeslac rozne -- ale");
    println!("kazdy wezel zaczyna tam, gdzie mowi JEGO zegar, a budzet dopuszcza roznice");
    println!("znacznie szersza niz szczelina.");
}
