//! Measure a core frame against real `LoRa` payload limits.

use lorai::domain::contracts::{Stage, Subject, Verdict};
use lorai::domain::time::Timestamp;
use lorai::wire::{CompactEnvelope, Envelope, GroupKey, SigningKey, encode, encode_compact, seal};

fn main() {
    let subject = Subject::new(
        "baltic-2026",
        "evt-000123",
        0,
        [0x5A; 32],
        Timestamp::from_secs(1_700_000_000),
    )
    .expect("valid subject");
    let signed = Envelope::new(
        &subject,
        "morsik-07",
        Stage::BindingSupport,
        Verdict::Support,
        4_242,
    )
    .sign(&SigningKey::from_seed([1; 32]));

    let plain = encode(&signed).expect("encodes");
    let sealed = seal(&GroupKey::from_bytes([9; 32]), 4_242, &plain).expect("seals");

    println!("core frame        : {} B", plain.len());
    println!("sealed (AEAD +16) : {} B", sealed.len());
    println!();
    for (name, limit) in [
        ("SF12 (max zasieg)", 51usize),
        ("SF10", 133),
        ("SF7 (max przeplyw)", 222),
    ] {
        let verdict = if sealed.len() <= limit {
            "MIESCI SIE"
        } else {
            "ZA DUZE"
        };
        println!("{name:<20} limit {limit:>3} B  -> {verdict}");
    }

    let compact = CompactEnvelope::new(
        7,
        0x0001_E240,
        0,
        [0x5A; 32],
        1_700_000_000,
        12,
        3,
        1,
        4_242,
    )
    .sign(&SigningKey::from_seed([1; 32]));
    let compact_bytes = encode_compact(&compact).expect("encodes");
    let compact_sealed =
        seal(&GroupKey::from_bytes([9; 32]), 4_242, &compact_bytes).expect("seals");

    println!();
    println!("--- forma zwarta (indeksy manifestu) ---");
    println!("ramka             : {} B", compact_bytes.len());
    println!("zapieczetowana    : {} B", compact_sealed.len());
    for (name, limit) in [
        ("SF12 (max zasieg)", 51usize),
        ("SF10", 133),
        ("SF7 (max przeplyw)", 222),
    ] {
        let verdict = if compact_sealed.len() <= limit {
            "MIESCI SIE"
        } else {
            "ZA DUZE"
        };
        println!("{name:<20} limit {limit:>3} B  -> {verdict}");
    }
}
