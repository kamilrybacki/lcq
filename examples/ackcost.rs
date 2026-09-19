//! What the piggybacked acknowledgement costs, per frame.
use lcq::sim::airtime_ms;
use lcq::wire::{CompactEnvelope, GroupKey, Heard, SigningKey, encode_compact, seal_frame};

fn main() {
    let key = SigningKey::from_seed([7u8; 32]);
    let group = GroupKey::from_bytes([9u8; 32]);
    let base = CompactEnvelope::new(1, 1, 0, [0x11; 32], 1_700_000_000, 3, 3, 1, 4_242);

    let mut heard = Heard::none();
    for i in 0..10 {
        heard.heard_from(i);
    }

    for (name, envelope) in [
        ("bez potwierdzenia", base.clone()),
        ("z bitmapa (10 slyszanych)", base.acknowledging(heard)),
    ] {
        let signed = encode_compact(&envelope.sign(&key)).expect("encodes");
        let on_air = seal_frame(&group, 3, 4_242, &signed).expect("seals");
        println!(
            "{name:<30}{:>5} B{:>8} ms",
            on_air.len(),
            airtime_ms(on_air.len())
        );
    }
}
