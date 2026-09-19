//! Dump simulation runs as JSON, for inspection or visualisation.
//!
//! Everything printed here comes from an actual run of [`Scenario::run`]. Times
//! are milliseconds since the trigger, on one clock across the whole run, so a
//! consumer can replay a scenario rather than only chart it. The JSON is written
//! by hand rather than with a serialiser, because one example binary is not
//! worth a dependency.

use lcq::domain::contracts::CONSULTATION_CUTOFF_SECONDS;
use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;
use lcq::sim::{
    ANTENNA_GAIN_DBI, DEFAULT_SPREADING_FACTOR, DUTY_CYCLE_BUDGET_MS, Deliberation, Outcome,
    SENSITIVITY_DBM, Scenario, TX_POWER_DBM, Topology, TraceEntry, airtime_ms_at, breakpoint_m,
    max_range_m, path_loss_db, radio_horizon_m, sensitivity_dbm_at,
};
use lcq::wire::{
    CompactEnvelope, GroupKey, Heard, MAX_FRAME_BYTES, RoundId, SigningKey, encode_compact,
    seal_frame,
};

/// A signed, sealed endorsement frame as the node puts it on the air: every
/// field at a realistic value, the acknowledgement bitmap and round label set.
/// Measured, not assumed, so the page follows the wire format (D20).
fn frame_bytes() -> usize {
    let mut heard = Heard::none();
    for member in 0..5 {
        heard.heard_from(member);
    }
    let envelope = CompactEnvelope::new(1, 1, 0, [0x11; 32], 1_700_000_000, 3, 3, 1, 4_242)
        .acknowledging(heard)
        .in_round(RoundId::new(0, 1));
    let signed = encode_compact(&envelope.sign(&SigningKey::from_seed([7; 32]), &[0x11; 32]))
        .expect("encodes");
    seal_frame(&GroupKey::from_bytes([9; 32]), 3, 4_242, &signed)
        .expect("seals")
        .len()
}

/// Loss per doubling of distance in the two-ray far field over water.
const DB_PER_DOUBLING: f64 = 12.04;

/// Dead time between slots: demodulation jitter plus drift across one round.
const GUARD_MS: u64 = 200;

fn main() {
    println!("{{");
    print_radio();
    print_path_loss();
    print_spreading();
    print_deliberation();
    print_scenarios(&cases());
    println!("}}");
}

/// The whole protocol, for the page that must not keep showing one stage as if
/// it were an endorsement.
fn print_deliberation() {
    println!("  \"deliberation\": {{");
    println!("    \"cutoffS\": {CONSULTATION_CUTOFF_SECONDS},");
    println!("    \"skewS\": {MAX_CLOCK_SKEW_SECONDS},");
    println!(
        "    \"floorS\": {},",
        CONSULTATION_CUTOFF_SECONDS + MAX_CLOCK_SKEW_SECONDS
    );
    for (key, built, last) in [
        ("random", Deliberation::new(10), false),
        ("slotted", Deliberation::new(10).with_slots(GUARD_MS), true),
    ] {
        let r = built.run();
        println!("    \"{key}\": {{");
        println!("      \"endorsed\": {},", r.endorsed);
        println!("      \"supporters\": {},", r.binding_supporters);
        println!("      \"threshold\": {},", r.min_signers);
        println!("      \"elapsedS\": {},", r.elapsed_s);
        println!("      \"radioMs\": {},", r.radio_ms);
        println!("      \"airtimeMs\": {},", r.airtime_ms);
        println!("      \"frameBytes\": {},", r.frame_bytes);
        print!("      \"stages\": [");
        for (index, stage) in r.stages.iter().enumerate() {
            if index > 0 {
                print!(", ");
            }
            print!(
                "{{\"frames\": {}, \"collided\": {}, \"lost\": {}, \"admitted\": {}, \"refused\": {}, \"airtimeMs\": {}, \"elapsedMs\": {}}}",
                stage.frames_sent,
                stage.collided,
                stage.lost,
                stage.admitted,
                stage.refused,
                stage.airtime_ms,
                stage.elapsed_ms
            );
        }
        println!("]");
        println!("    }}{}", if last { "" } else { "," });
    }
    println!("  }},");
}

fn cases() -> Vec<(&'static str, Scenario)> {
    vec![
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
    ]
}

fn print_radio() {
    println!("  \"horizonM\": {:.0},", radio_horizon_m());
    println!("  \"breakpointM\": {:.0},", breakpoint_m());
    println!("  \"rangeM\": {:.0},", max_range_m(TX_POWER_DBM));
    println!("  \"sensitivityDbm\": {SENSITIVITY_DBM},");
    println!(
        "  \"eirpDbm\": {:.0},",
        TX_POWER_DBM + 2.0 * ANTENNA_GAIN_DBI
    );
    println!("  \"frameBytes\": {},", frame_bytes());
    println!("  \"maxFrameBytes\": {MAX_FRAME_BYTES},");
}

fn print_path_loss() {
    print!("  \"pathLoss\": [");
    let mut distance = 100.0_f64;
    let mut first = true;
    while distance < 40_000.0 {
        if !first {
            print!(", ");
        }
        first = false;
        let loss = path_loss_db(distance);
        if loss.is_finite() {
            print!("[{distance:.0}, {loss:.2}]");
        } else {
            print!("[{distance:.0}, null]");
        }
        distance *= 1.15;
    }
    println!("],");
}

fn print_spreading() {
    print!("  \"spreading\": [");
    let reference = sensitivity_dbm_at(10);
    for sf in 7..=12u8 {
        let air = airtime_ms_at(sf, frame_bytes());
        let reach = 2f64.powf((reference - sensitivity_dbm_at(sf)) / DB_PER_DOUBLING);
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
}

fn print_scenarios(cases: &[(&str, Scenario)]) {
    // Each fleet is run twice, under both access schemes, so a reader can put
    // the two side by side rather than take the comparison on trust.
    let runs: Vec<(&str, &str, Scenario)> = cases
        .iter()
        .flat_map(|(name, scenario)| {
            [
                (*name, "random", scenario.clone()),
                (*name, "slotted", scenario.clone().with_slots(GUARD_MS)),
            ]
        })
        .collect();

    println!("  \"scenarios\": [");
    for (index, (name, access, scenario)) in runs.iter().enumerate() {
        let report = scenario.run();
        let duration = report
            .timeline
            .iter()
            .map(|f| f.start_ms + f.airtime_ms)
            .max()
            .unwrap_or(0);
        let comma = if index + 1 == runs.len() { "" } else { "," };

        println!("    {{");
        println!("      \"name\": \"{name}\",");
        println!("      \"access\": \"{access}\",");
        // The node sizes its slot to the widest frame, not the typical one.
        println!(
            "      \"slotMs\": {},",
            airtime_ms_at(DEFAULT_SPREADING_FACTOR, MAX_FRAME_BYTES) + GUARD_MS
        );
        println!("      \"fleet\": {},", scenario.fleet());
        match scenario.spacing_m() {
            // Null is not "zero metres apart": it means distance is not
            // modelled at all, so a consumer must not draw positions.
            None => println!("      \"spacingM\": null,"),
            Some(spacing) => println!("      \"spacingM\": {spacing:.0},"),
        }
        println!("      \"endorsed\": {},", report.endorsed);
        println!("      \"supporters\": {},", report.binding_supporters);
        println!("      \"threshold\": {},", report.min_signers);
        println!("      \"framesSent\": {},", report.frames_sent);
        println!("      \"collided\": {},", report.collided_frames);
        println!("      \"tooWeak\": {},", report.too_weak_frames);
        println!("      \"rejected\": {},", report.rejected_frames);
        println!("      \"dutyBlocked\": {},", report.duty_cycle_blocked);
        println!("      \"airtimeMs\": {},", report.airtime_ms);
        println!("      \"durationMs\": {duration},");
        print!("      \"windowsMs\": [");
        for round in 0..5usize {
            if round > 0 {
                print!(", ");
            }
            print!("{}", Scenario::window_ms(round));
        }
        println!("],");
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
