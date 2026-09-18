//! Dump simulation runs as JSON, for inspection or visualisation.
//!
//! Everything printed here comes from an actual run of [`Scenario::run`]. The
//! JSON is written by hand rather than with a serialiser, because one example
//! binary is not worth a dependency.

use lorai::sim::{
    ANTENNA_GAIN_DBI, DUTY_CYCLE_BUDGET_MS, Outcome, SENSITIVITY_DBM, Scenario, TX_POWER_DBM,
    Topology, TraceEntry, airtime_ms_at, breakpoint_m, path_loss_db, radio_horizon_m,
    sensitivity_dbm_at,
};

/// A signed, compacted endorsement frame.
const FRAME_BYTES: usize = 105;

fn main() {
    let cases: Vec<(&str, Scenario)> = vec![
        ("5 wezlow, bez strat", Scenario::new(5)),
        ("10 wezlow, bez strat", Scenario::new(10)),
        (
            "10 wezlow, 30% strat",
            Scenario::new(10).with_loss(0.3).with_seed(7),
        ),
        (
            "10 wezlow, podzial sieci",
            Scenario::new(10).with_topology(Topology::Partitioned),
        ),
        ("10 wezlow, 4 milczace", Scenario::new(10).with_silent(4)),
        ("5 wezlow, 2 falszerzy", Scenario::new(5).with_forgers(2)),
        (
            "20 wezlow, 30% strat",
            Scenario::new(20).with_loss(0.3).with_seed(3),
        ),
        (
            "10 wezlow, 34 s juz zuzyte",
            Scenario::new(10).with_prior_airtime_ms(34_000),
        ),
        (
            "10 wezlow, konwoj 500 m",
            Scenario::new(10).with_spacing_m(500.0).with_seed(11),
        ),
        (
            "10 wezlow, konwoj 4 km",
            Scenario::new(10).with_spacing_m(4_000.0).with_seed(11),
        ),
        (
            "10 wezlow, konwoj 5 km",
            Scenario::new(10).with_spacing_m(5_000.0).with_seed(11),
        ),
    ];

    println!("{{");
    println!("  \"horizonM\": {:.0},", radio_horizon_m());
    println!("  \"breakpointM\": {:.0},", breakpoint_m());
    println!("  \"sensitivityDbm\": {SENSITIVITY_DBM},");
    println!(
        "  \"eirpDbm\": {:.0},",
        TX_POWER_DBM + 2.0 * ANTENNA_GAIN_DBI
    );
    print!("  \"pathLoss\": [");
    let mut first = true;
    let mut distance = 100.0_f64;
    while distance < 40_000.0 {
        let loss = path_loss_db(distance);
        if !first {
            print!(", ");
        }
        first = false;
        if loss.is_finite() {
            print!("[{distance:.0}, {loss:.2}]");
        } else {
            print!("[{distance:.0}, null]");
        }
        distance *= 1.15;
    }
    println!("],");

    // Every spreading factor, so the page cannot drift from the crate.
    print!("  \"spreading\": [");
    let reference = sensitivity_dbm_at(10);
    for sf in 7..=12u8 {
        let air = airtime_ms_at(sf, FRAME_BYTES);
        let reach = 2f64.powf((reference - sensitivity_dbm_at(sf)) / 12.04);
        if sf > 7 {
            print!(", ");
        }
        print!(
            "{{\"sf\": {sf}, \"airtimeMs\": {air}, \"sensitivityDbm\": {}, \"framesPerHour\": {}, \"reach\": {reach:.2}}}",
            sensitivity_dbm_at(sf),
            DUTY_CYCLE_BUDGET_MS / air.max(1)
        );
    }
    println!("],");
    println!("  \"frameBytes\": {FRAME_BYTES},");

    println!("  \"scenarios\": [");
    for (index, (name, scenario)) in cases.iter().enumerate() {
        let report = scenario.run();
        let comma = if index + 1 == cases.len() { "" } else { "," };
        println!("    {{");
        println!("      \"name\": \"{name}\",");
        println!("      \"fleet\": {},", scenario.fleet());
        println!("      \"endorsed\": {},", report.endorsed);
        println!("      \"supporters\": {},", report.binding_supporters);
        println!("      \"threshold\": {},", report.min_signers);
        println!("      \"framesSent\": {},", report.frames_sent);
        println!("      \"collided\": {},", report.collided_frames);
        println!("      \"tooWeak\": {},", report.too_weak_frames);
        println!("      \"rejected\": {},", report.rejected_frames);
        println!("      \"dutyBlocked\": {},", report.duty_cycle_blocked);
        println!("      \"airtimeMs\": {},", report.airtime_ms);
        println!("      \"timeline\": [");
        let last = report.timeline.len().saturating_sub(1);
        for (at, entry) in report.timeline.iter().enumerate() {
            let tail = if at == last { "" } else { "," };
            println!("        {}{tail}", frame_json(entry));
        }
        println!("      ]");
        println!("    }}{comma}");
    }
    println!("  ]");
    println!("}}");
}

fn frame_json(entry: &TraceEntry) -> String {
    let rssi = if entry.rssi_dbm.is_finite() {
        format!("{:.1}", entry.rssi_dbm)
    } else {
        // JSON has no infinity. Null reads as "not a received power".
        "null".to_string()
    };
    format!(
        "{{\"r\": {}, \"s\": {}, \"t\": {}, \"d\": {}, \"rssi\": {rssi}, \"o\": \"{}\"}}",
        entry.round,
        entry.sender,
        entry.start_ms,
        entry.airtime_ms,
        outcome_name(entry.outcome)
    )
}

const fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Decoded => "decoded",
        Outcome::OwnTransmission => "own",
        Outcome::Rejected => "rejected",
        Outcome::Collided => "collided",
        Outcome::TooWeak => "tooWeak",
        Outcome::Unreachable => "unreachable",
        Outcome::LostToResidualNoise => "noise",
        Outcome::DutyCycleBlocked => "dutyBlocked",
    }
}
