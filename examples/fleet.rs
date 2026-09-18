//! Run a fleet through endorsement under a few conditions.

use lcq::sim::{Scenario, Topology};

fn main() {
    println!(
        "{:<32}{:>7}{:>6}{:>7}{:>7}{:>7}{:>7}{:>6}{:>10}",
        "scenariusz", "popar.", "prog", "ramki", "kolizj", "slabe", "odrzuc", "duty", "antena[s]"
    );
    println!("{}", "-".repeat(89));

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
        ("10 wezlow, 4 milczace", Scenario::new(10).with_silent(4)),
        ("5 wezlow, 2 falszerzy", Scenario::new(5).with_forgers(2)),
        (
            "20 wezlow, 30% strat",
            Scenario::new(20).with_loss(0.3).with_seed(3),
        ),
        // The 1 % hourly budget, mostly spent on other traffic already.
        (
            "10 wezlow, 34 s juz zuzyte",
            Scenario::new(10).with_prior_airtime_ms(34_000),
        ),
        (
            "10 wezlow, 30 s zuzyte + straty",
            Scenario::new(10)
                .with_prior_airtime_ms(30_000)
                .with_loss(0.5)
                .with_seed(5),
        ),
        // Geometry on: path loss and fading decide who is still audible.
        (
            "10 wezlow, konwoj 500 m",
            Scenario::new(10).with_spacing_m(500.0).with_seed(11),
        ),
        (
            "10 wezlow, konwoj 2 km",
            Scenario::new(10).with_spacing_m(2_000.0).with_seed(11),
        ),
        (
            "10 wezlow, konwoj 4 km",
            Scenario::new(10).with_spacing_m(4_000.0).with_seed(11),
        ),
    ];

    for (name, scenario) in cases {
        let r = scenario.run();
        let mark = if r.endorsed { "TAK" } else { "nie" };
        #[allow(clippy::cast_precision_loss)]
        let seconds = r.airtime_ms as f64 / 1_000.0;
        println!(
            "{name:<32}{:>7}{:>6}{:>7}{:>7}{:>7}{:>7}{:>6}{seconds:>10.1}  {mark}",
            r.binding_supporters,
            r.min_signers,
            r.frames_sent,
            r.collided_frames,
            r.too_weak_frames,
            r.rejected_frames,
            r.duty_cycle_blocked,
        );
    }

    println!();
    println!("„popar.\" = zweryfikowane podpisy widziane przez JEDEN wezel obserwujacy.");
    println!(
        "„kolizj\" obejmuje ramki zagluszone przez wlasne nadawanie obserwatora (half-duplex)."
    );
    println!("„slabe\" = ponizej czulosci odbiornika; „duty\" = odmowy limitu 1 % / h.");
    println!("Konwoj = wezly w linii, obserwator na koncu: dziala tlumienie trasy i zanik.");
    println!("Blokada jest poprawnym wynikiem: protokol nigdy nie zmysla kworum.");
}
