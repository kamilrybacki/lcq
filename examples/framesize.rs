//! What actually goes on the air, measured at every stage.

use lcq::sim::airtime_ms;
use lcq::wire::{
    CompactEnvelope, FRAME_HEADER_BYTES, GroupKey, SigningKey, encode_compact, seal, seal_frame,
};

fn main() {
    let key = SigningKey::from_seed([7u8; 32]);
    let group = GroupKey::from_bytes([9u8; 32]);
    let envelope = CompactEnvelope::new(1, 1, 0, [0x11; 32], 1_700_000_000, 3, 3, 1, 4_242);
    let signed = encode_compact(&envelope.sign(&key, &[0x11; 32])).expect("encodes");
    let sealed = seal(&group, 3, 4_242, &signed).expect("seals");
    let on_air = seal_frame(&group, 3, 4_242, &signed).expect("seals");

    println!("{:<38}{:>7}{:>12}", "postac", "bajty", "antena[ms]");
    println!("{}", "-".repeat(57));
    for (name, bytes) in [
        ("podpisana, niezaszyfrowana", signed.len()),
        ("zapieczetowana", sealed.len()),
        ("z naglowkiem nonce (NA ANTENIE)", on_air.len()),
    ] {
        println!("{name:<38}{bytes:>7}{:>12}", airtime_ms(bytes));
    }

    println!();
    println!(
        "naglowek jawny: {FRAME_HEADER_BYTES} B (indeks autora + sekwencja), \
         pieczec: {} B",
        sealed.len() - signed.len()
    );
    let extra = airtime_ms(on_air.len()).saturating_sub(airtime_ms(signed.len()));
    #[allow(clippy::cast_precision_loss)]
    let pct = 100.0 * extra as f64 / airtime_ms(signed.len()) as f64;
    println!("razem {extra} ms wiecej na kazda ramke, czyli {pct:.0}% ponad sama podpisana postac");
    println!();
    println!("Naglowek musi byc jawny: nonce nie moze lezec w tym, co odszyfrowuje.");
    println!("Jest jednoczesnie AAD, wiec jego podmiana psuje ramke, a nie przekierowuje.");
}
