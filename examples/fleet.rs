//! Run a fleet through endorsement under a few conditions.

use lorai::sim::{Scenario, Topology};

fn main() {
    println!(
        "{:<34}{:>7}{:>8}{:>8}{:>9}{:>11}",
        "scenariusz", "popar.", "prog", "ramki", "odrzuc.", "antena[ms]"
    );
    println!("{}", "-".repeat(77));

    let cases: Vec<(&str, Scenario)> = vec![
        ("5 wezlow, bez strat", Scenario::new(5)),
        ("10 wezlow, bez strat", Scenario::new(10)),
        (
            "10 wezlow, 30% strat",
            Scenario::new(10).with_loss(0.3).with_seed(7),
        ),
        (
            "10 wezlow, 60% strat",
            Scenario::new(10).with_loss(0.6).with_seed(7),
        ),
        (
            "10 wezlow, podzial sieci",
            Scenario::new(10).with_topology(Topology::Partitioned),
        ),
        ("10 wezlow, 1 milczacy", Scenario::new(10).with_silent(1)),
        (
            "10 wezlow, 4 milczace (budzet)",
            Scenario::new(10).with_silent(4),
        ),
        ("5 wezlow, 2 falszerzy", Scenario::new(5).with_forgers(2)),
        (
            "20 wezlow, 30% strat",
            Scenario::new(20).with_loss(0.3).with_seed(3),
        ),
    ];

    for (name, scenario) in cases {
        let r = scenario.run();
        let mark = if r.endorsed { "TAK" } else { "nie" };
        println!(
            "{name:<34}{:>7}{:>8}{:>8}{:>9}{:>11}  {mark}",
            r.binding_supporters, r.min_signers, r.frames_sent, r.rejected_frames, r.airtime_ms
        );
    }

    println!();
    println!("„popar.\" = zweryfikowane podpisy widziane przez JEDEN wezel obserwujacy.");
    println!("Blokada jest poprawnym wynikiem: protokol nigdy nie zmysla kworum.");
}
