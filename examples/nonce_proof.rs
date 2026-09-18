//! The nonce collision that once existed between two members of one group.
//!
//! Kept as a regression witness: it demonstrates the failure the author index
//! in the nonce prevents, and asserts that it no longer happens.

use lcq::wire::{GroupKey, seal};

fn main() {
    let group = GroupKey::from_bytes([0x5a; 32]);
    // Two different members, each with its own journal, each counting from zero.
    let a = seal(&group, 0, 0, b"node a says support").expect("seals");
    let b = seal(&group, 1, 0, b"node b says dispute").expect("seals");

    let leaked: Vec<u8> = a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect();
    let plaintext_xor: Vec<u8> = b"node a says support"
        .iter()
        .zip(b"node b says dispute".iter())
        .map(|(x, y)| x ^ y)
        .collect();

    println!("Dwaj czlonkowie, ten sam klucz grupy, obaj z sequence = 0.");
    println!();
    println!("xor szyfrogramow : {:02x?}", &leaked[..19]);
    println!("xor tekstow jawn.: {:02x?}", &plaintext_xor[..19]);
    println!();
    assert_ne!(
        leaked[..19],
        plaintext_xor[..19],
        "keystream reuse across members has come back"
    );
    println!("OK: strumienie klucza sa rozne, bo indeks autora wchodzi w nonce.");

    // And the same member reusing a sequence is still catastrophic, which is
    // what the journal's never-rewinding counter exists to prevent.
    let first = seal(&group, 0, 7, b"first payload here!").expect("seals");
    let again = seal(&group, 0, 7, b"second payload here").expect("seals");
    let reused: Vec<u8> = first.iter().zip(again.iter()).map(|(x, y)| x ^ y).collect();
    let expected: Vec<u8> = b"first payload here!"
        .iter()
        .zip(b"second payload here".iter())
        .map(|(x, y)| x ^ y)
        .collect();
    assert_eq!(reused[..19], expected[..19]);
    println!("Ten sam czlonek z powtorzonym sequence nadal wycieka — dlatego dziennik");
    println!("nigdy nie cofa licznika, a `reserve_sequence` jest trwale przed zwrotem.");
}
