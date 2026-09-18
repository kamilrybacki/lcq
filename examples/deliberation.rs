//! The whole protocol, end to end, against the radio-only model.
//!
//! `examples/fleet` measures one round of one stage. This runs all three
//! stages through the real state machine, the real journal and the real group
//! seal, and reports what that costs.

use lcq::sim::{Deliberation, Scenario, Topology};

const GUARD_MS: u64 = 200;

fn main() {
    let report = Deliberation::new(10).with_slots(GUARD_MS).run();
    println!(
        "Ramka na antenie: {} B (zapieczetowana)",
        report.frame_bytes
    );
    println!();
    println!(
        "{:<22}{:>8}{:>9}{:>9}{:>10}{:>11}",
        "etap", "ramki", "zderz", "przyjete", "odrzuc", "trwal[s]"
    );
    println!("{}", "-".repeat(70));
    for (name, stage) in ["niezalezny", "konsultacja", "wiazacy"]
        .iter()
        .zip(&report.stages)
    {
        #[allow(clippy::cast_precision_loss)]
        let secs = stage.elapsed_ms as f64 / 1000.0;
        println!(
            "{name:<22}{:>8}{:>9}{:>9}{:>10}{secs:>11.1}",
            stage.frames_sent, stage.collided, stage.admitted, stage.refused
        );
    }
    println!();
    println!(
        "kworum: {} poparc przy progu {} -> {}",
        report.binding_supporters,
        report.min_signers,
        if report.endorsed {
            "ZATWIERDZONE"
        } else {
            "zablokowane"
        }
    );
    println!("czas calkowity: {} s", report.elapsed_s);
    #[allow(clippy::cast_precision_loss)]
    let radio = report.radio_ms as f64 / 1000.0;
    println!(
        "z tego radio:   {radio:.1} s ({:.1} %)",
        report.radio_share() * 100.0
    );
    println!("antena lacznie: {} ms", report.airtime_ms);
    println!("odmowy maszyny stanow: {:?}", report.refusals);

    println!();
    println!("Dla porownania, model radiowy (jeden etap, bez maszyny stanow):");
    let radio_only = Scenario::new(10).with_slots(GUARD_MS).run();
    println!(
        "  {} ramek, {} zderzen, {} ms anteny",
        radio_only.frames_sent, radio_only.collided_frames, radio_only.airtime_ms
    );

    println!();
    println!(
        "{:<30}{:>10}{:>12}{:>12}",
        "wariant", "kworum", "czas[s]", "antena[ms]"
    );
    println!("{}", "-".repeat(64));
    for (name, d) in [
        ("10, losowo", Deliberation::new(10)),
        ("10, sloty", Deliberation::new(10).with_slots(GUARD_MS)),
        (
            "10, sloty, 30% strat",
            Deliberation::new(10)
                .with_slots(GUARD_MS)
                .with_loss(0.3)
                .with_seed(7),
        ),
        (
            "10, sloty, podzial",
            Deliberation::new(10)
                .with_slots(GUARD_MS)
                .with_topology(Topology::Partitioned),
        ),
        (
            "10, sloty, 4 milczace",
            Deliberation::new(10).with_slots(GUARD_MS).with_silent(4),
        ),
        (
            "10, sloty, konwoj 4 km",
            Deliberation::new(10)
                .with_slots(GUARD_MS)
                .with_spacing_m(4000.0)
                .with_seed(11),
        ),
        ("20, sloty", Deliberation::new(20).with_slots(GUARD_MS)),
    ] {
        let r = d.run();
        println!(
            "{name:<30}{:>10}{:>12}{:>12}",
            if r.endorsed { "TAK" } else { "nie" },
            r.elapsed_s,
            r.airtime_ms
        );
    }
}
