//! Identity of one vote lock: the exact case, plus who voted on it.

use alloc::string::String;

use crate::domain::contracts::Subject;

extern crate alloc;

/// Build the key a vote lock is stored under.
///
/// Derived from the full subject rather than the event ID alone, so a revision
/// or a change of content is a different lock — as it must be, since those are
/// different claims. The unit separator cannot appear in a mission, event or
/// voter name, so no two distinct cases can collide on one key.
#[must_use]
pub fn lock_key(subject: &Subject, voter: &str) -> String {
    // Hex by hand rather than `format!`: this runs on every lookup, and a
    // formatter allocation per byte is not worth paying on a constrained node.
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut key = String::new();
    key.push_str(subject.mission());
    key.push('\u{1f}');
    key.push_str(subject.event());
    key.push('\u{1f}');
    for byte in subject.content_hash() {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 0x0f) as usize] as char);
    }
    key.push('\u{1f}');
    let mut revision = subject.revision();
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    loop {
        index -= 1;
        digits[index] = b'0' + u8::try_from(revision % 10).unwrap_or(0);
        revision /= 10;
        if revision == 0 {
            break;
        }
    }
    for digit in &digits[index..] {
        key.push(*digit as char);
    }
    key.push('\u{1f}');
    key.push_str(voter);
    key
}
