//! A key that arrives by hand.
//!
//! The fleet's administrator issues the key that signs manifests and hands its
//! public half to each vessel out of band; somebody aboard types it in
//! (`DECISIONS.md` D26). That makes it the one value in this protocol whose
//! encoding is a human problem rather than a machine one: it has to survive
//! being written on a card, read aloud over VHF, and typed once by somebody on
//! a moving deck.
//!
//! So: Crockford's base32, which leaves out `I`, `L`, `O` and `U`, reads `I`
//! and `L` back as `1` and `O` as `0`, and does not care about case. Hyphens
//! and spaces are ignored, so the grouping this prints is a convenience and
//! never part of the value. Four check symbols follow the key, and anything
//! that does not match them is refused rather than half-accepted.
//!
//! ```text
//! 8VQ4-3JX0- ... -Z9T2    56 symbols: 52 of key, 4 of check
//! ```
//!
//! # What the check symbols are worth, stated precisely
//!
//! They are the first twenty bits of the key's `BLAKE2s` digest, so a mistyped
//! key is refused with probability `1 - 2^-20`. That is a probability and not
//! a guarantee: unlike a deterministic check digit, nothing here *proves* that
//! every single mistype is caught.
//!
//! The tests are exhaustive over the error space and not over the key space,
//! which is worth saying both halves of. For every key they cover, they try
//! every single-symbol substitution at every position, every transposition of
//! neighbouring symbols and every swap of neighbouring groups, and none get
//! through. They cover a fixed set of deterministic keys, so that is a
//! regression suite rather than an algebraic guarantee.
//!
//! Crockford's own modulo-37 check symbol would give that guarantee for
//! single substitutions and neighbouring transpositions, and `DECISIONS.md`
//! D26 records why this does not use it: its check alphabet pulls in `U` and
//! `*~$=`, which is exactly what a key read aloud over VHF cannot afford.
//!
//! # This encoding is for one key, and not for keys in a manifest
//!
//! Reading `I` as `1` and `O` as `0` is what makes this survive a human, and
//! it means several different strings decode to the same bytes. Inside a body
//! that gets signed, that is disqualifying: a canonical form needs exactly one
//! representation per value. So this is the encoding for the single key a
//! person types at commissioning, and never for the member keys a signed
//! manifest carries, which use a machine encoding with one spelling each.
//!
//! # What none of this defends against
//!
//! Typing a key by hand says a human asserted it once. It does not keep the
//! file it lands in safe afterwards: whoever can rewrite a vessel's manifest
//! can usually rewrite whatever holds the key beside it (`THREAT-MODEL.md`
//! F21). The node printing this encoding back at every start makes a
//! substitution *detectable*, but only while three things hold: the
//! administrator's card is trusted independently, somebody actually compares
//! against it at every start, and the value on screen came from an unmodified
//! binary. An attacker who can rewrite the key file can usually rewrite the
//! program that prints it, and on an unattended vessel nobody is comparing, so
//! there this buys nothing at all.

use alloc::string::String;
use core::fmt;

use blake2::{Blake2s256, Digest};

extern crate alloc;

/// Crockford's alphabet: no `I`, `L`, `O` or `U`.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Bits a symbol carries.
const BITS: usize = 5;
/// Symbols of key: 256 bits at five bits each, rounded up.
const KEY_SYMBOLS: usize = 52;
/// Symbols of check, drawn from the key's digest.
const CHECK_SYMBOLS: usize = 4;
/// Symbols in the short form a human compares by eye.
const FINGERPRINT_SYMBOLS: usize = 16;
/// How many symbols are printed between hyphens.
const GROUP: usize = 4;
/// Every symbol of a written key: the key, then its check.
pub const SYMBOLS: usize = KEY_SYMBOLS + CHECK_SYMBOLS;

/// Why what somebody typed is not a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandError {
    /// A character that is not in the alphabet, after the confusable ones have
    /// been read the way Crockford says.
    UnknownSymbol {
        /// The offending character.
        found: char,
        /// Which symbol it was, counting from one and ignoring separators.
        at: usize,
    },
    /// The wrong number of symbols: something was dropped or doubled.
    WrongLength {
        /// How many symbols were typed.
        found: usize,
        /// How many there should be.
        expected: usize,
    },
    /// The check symbols do not match the key: something was mistyped.
    CheckFailed,
    /// The last symbol carries padding bits a 32-byte key cannot set, so this
    /// string did not come from [`encode`].
    NotCanonical,
}

impl fmt::Display for HandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSymbol { found, at } => {
                write!(f, "symbol {at} is {found:?}, which is not in the alphabet")
            }
            Self::WrongLength { found, expected } => {
                write!(f, "{found} symbols, expected {expected}")
            }
            Self::CheckFailed => {
                f.write_str("the check symbols do not match: something was mistyped")
            }
            Self::NotCanonical => f.write_str("the last symbol carries bits a key cannot"),
        }
    }
}

impl core::error::Error for HandError {}

/// The five bits that symbol `index` takes from `bytes`, zero past the end.
fn symbol_at(bytes: &[u8], index: usize) -> u8 {
    let mut value = 0;
    for bit in 0..BITS {
        let position = index * BITS + bit;
        let set = position < bytes.len() * 8 && bytes[position / 8] & (0x80 >> (position % 8)) != 0;
        value = (value << 1) | u8::from(set);
    }
    value
}

/// The check symbols a key should carry: the leading bits of its digest.
fn check_of(key: &[u8; 32]) -> [u8; CHECK_SYMBOLS] {
    let digest = Blake2s256::digest(key);
    let mut check = [0; CHECK_SYMBOLS];
    for (index, symbol) in check.iter_mut().enumerate() {
        *symbol = symbol_at(&digest, index);
    }
    check
}

/// Write `symbols` the way they go on a card: uppercase, in groups of four.
fn write(symbols: &[u8]) -> String {
    let mut out = String::with_capacity(symbols.len() + symbols.len() / GROUP);
    for (index, symbol) in symbols.iter().enumerate() {
        if index > 0 && index % GROUP == 0 {
            out.push('-');
        }
        out.push(char::from(ALPHABET[usize::from(*symbol)]));
    }
    out
}

/// Write a key out for somebody to read aloud or copy onto a card.
#[must_use]
pub fn encode(key: &[u8; 32]) -> String {
    let mut symbols = [0; SYMBOLS];
    for (index, symbol) in symbols.iter_mut().enumerate().take(KEY_SYMBOLS) {
        *symbol = symbol_at(key, index);
    }
    symbols[KEY_SYMBOLS..].copy_from_slice(&check_of(key));
    write(&symbols)
}

/// Read a key somebody typed. Case, hyphens and spaces do not matter, and
/// Crockford's confusable letters are read as the digits they look like.
///
/// # Errors
///
/// See [`HandError`]. A key that does not match its check symbols is refused
/// outright: a key that is nearly right is not a key.
pub fn decode(typed: &str) -> Result<[u8; 32], HandError> {
    let mut symbols = [0; SYMBOLS];
    let mut count = 0;
    for found in typed.chars() {
        if found == '-' || found.is_whitespace() {
            continue;
        }
        let value = value_of(found).ok_or(HandError::UnknownSymbol {
            found,
            at: count + 1,
        })?;
        if let Some(slot) = symbols.get_mut(count) {
            *slot = value;
        }
        count += 1;
    }
    if count != SYMBOLS {
        return Err(HandError::WrongLength {
            found: count,
            expected: SYMBOLS,
        });
    }

    // Fifty-two symbols hold 260 bits and a key is 256, so the last symbol
    // carries one bit of key and four of padding. Anything in those four is
    // not something `encode` could have written, and letting it through would
    // make three different strings decode to the same key.
    if symbols[KEY_SYMBOLS - 1] & 0x0F != 0 {
        return Err(HandError::NotCanonical);
    }
    let mut key = [0; 32];
    for (index, value) in symbols.iter().enumerate().take(KEY_SYMBOLS) {
        for bit in 0..BITS {
            let position = index * BITS + bit;
            if position < key.len() * 8 && value & (0x10 >> bit) != 0 {
                key[position / 8] |= 0x80 >> (position % 8);
            }
        }
    }
    if symbols[KEY_SYMBOLS..] != check_of(&key) {
        return Err(HandError::CheckFailed);
    }
    Ok(key)
}

/// One character's value, reading `I` and `L` as `1` and `O` as `0` the way
/// Crockford says, and refusing `U` rather than guessing at it.
fn value_of(found: char) -> Option<u8> {
    let upper = found.to_ascii_uppercase();
    match upper {
        'I' | 'L' => return Some(1),
        'O' => return Some(0),
        'U' => return None,
        _ => {}
    }
    let byte = u8::try_from(upper).ok()?;
    let position = ALPHABET.iter().position(|symbol| *symbol == byte)?;
    u8::try_from(position).ok()
}

/// A short form of a key, for somebody to compare against a card without
/// reading all of it back.
///
/// Eighty bits of the key's digest: enough that two keys an operator is
/// choosing between will not match, and not an identity — a manifest is
/// checked against the whole key, never against this.
#[must_use]
pub fn fingerprint(key: &[u8; 32]) -> String {
    let digest = Blake2s256::digest(key);
    let mut symbols = [0; FINGERPRINT_SYMBOLS];
    for (index, symbol) in symbols.iter_mut().enumerate() {
        *symbol = symbol_at(&digest, index);
    }
    write(&symbols)
}
