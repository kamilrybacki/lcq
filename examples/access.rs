//! Random contention against slotted access, on the same fleets.
//!
//! The protocol knows its membership: every frame already carries the author's
//! index into the manifest. This measures what that knowledge is worth if the
//! radio schedule uses it instead of making everyone draw for the channel.

use lcq::sim::{Scenario, Topology};

/// Dead time between slots. Covers the spread in when members decided the
/// trigger ended, plus oscillator drift across one round.
const GUARD_MS: u64 = 200;

fn main() {
    println!(
        "{:<30}{:>11}{:>11}{:>9}{:>9}{:>8}{:>8}",
        "scenariusz", "losowo[s]", "sloty[s]", "ramki", "ramki", "zderz", "zderz"
    );
    println!(
        "{:<30}{:>11}{:>11}{:>9}{:>9}{:>8}{:>8}",
        "", "", "", "los.", "slot", "los.", "slot"
    );
    println!("{}", "-".repeat(86));

    let cases: Vec<(&str, Scenario)> = vec![
        ("5 wezlow", Scenario::new(5)),
        ("10 wezlow", Scenario::new(10)),
        ("20 wezlow", Scenario::new(20)),
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
        ("10 wezlow, 4 milczace", Scenario::new(10).with_silent(4)),
        (
            "10 wezlow, 34 s zuzyte",
            Scenario::new(10).with_prior_airtime_ms(34_000),
        ),
        (
            "10 wezlow, konwoj 4 km",
            Scenario::new(10).with_spacing_m(4_000.0).with_seed(11),
        ),
    ];

    for (name, scenario) in cases {
        let random = scenario.clone().run();
        let slotted = scenario.with_slots(GUARD_MS).run();
        let mark = match (random.endorsed, slotted.endorsed) {
            (true, true) => "oba TAK",
            (false, false) => "oba nie",
            (false, true) => "slot ratuje",
            (true, false) => "slot psuje",
        };
        println!(
            "{name:<30}{:>11.1}{:>11.1}{:>9}{:>9}{:>8}{:>8}  {mark}",
            duration(&random),
            duration(&slotted),
            random.frames_sent,
            slotted.frames_sent,
            random.collided_frames,
            slotted.collided_frames,
        );
    }

    println!();
    println!("Sloty: nadawanie w szczelinie wynikajacej z indeksu w manifescie,");
    println!("liczonej od ramki wyzwalajacej runde -- nie od zegara sciennego.");
}

fn duration(report: &lcq::sim::Report) -> f64 {
    let end = report
        .timeline
        .iter()
        .map(|f| f.start_ms + f.airtime_ms)
        .max()
        .unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    {
        end as f64 / 1000.0
    }
}
