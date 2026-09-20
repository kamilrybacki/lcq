//! Typing a key in by hand: the mistakes a person makes on a moving deck, and
//! whether any of them get through.

use lcq::wire::hand::{self, HandError};

/// Deterministic keys, so a failure names one rather than a lucky seed.
fn key_from(seed: u64) -> [u8; 32] {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut key = [0; 32];
    for byte in &mut key {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *byte = state.to_be_bytes()[0];
    }
    key
}

/// The symbols of a written key, with the grouping taken back off.
fn symbols_of(key: &[u8; 32]) -> Vec<char> {
    hand::encode(key).chars().filter(|c| *c != '-').collect()
}

const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[test]
fn a_key_survives_being_written_down_and_typed_back() {
    for seed in 0..64 {
        let key = key_from(seed);
        let written = hand::encode(&key);
        assert_eq!(
            hand::decode(&written),
            Ok(key),
            "seed {seed} did not come back"
        );
    }
}

#[test]
fn a_written_key_is_fifty_six_symbols_in_groups_of_four() {
    let written = hand::encode(&key_from(1));
    let groups: Vec<&str> = written.split('-').collect();
    assert_eq!(groups.len(), 14, "{written}");
    assert!(groups.iter().all(|group| group.len() == 4), "{written}");
    assert_eq!(written.len(), hand::SYMBOLS + 13);
}

#[test]
fn nothing_written_can_be_confused_when_read_aloud() {
    for seed in 0..256 {
        let written = hand::encode(&key_from(seed));
        assert!(
            !written.contains(['I', 'L', 'O', 'U']),
            "seed {seed} wrote a confusable letter: {written}"
        );
        assert!(
            written.chars().all(|c| c == '-' || ALPHABET.contains(c)),
            "seed {seed} wrote something outside the alphabet: {written}"
        );
    }
}

#[test]
fn every_single_mistyped_symbol_is_refused() {
    for seed in 0..8 {
        let key = key_from(seed);
        let symbols = symbols_of(&key);
        for position in 0..symbols.len() {
            for wrong in ALPHABET.chars() {
                if wrong == symbols[position] {
                    continue;
                }
                let mut mistyped = symbols.clone();
                mistyped[position] = wrong;
                let typed: String = mistyped.into_iter().collect();
                assert!(
                    hand::decode(&typed).is_err(),
                    "seed {seed}: {wrong} at symbol {} was accepted",
                    position + 1
                );
            }
        }
    }
}

#[test]
fn every_swap_of_neighbouring_symbols_is_refused() {
    for seed in 0..32 {
        let key = key_from(seed);
        let symbols = symbols_of(&key);
        for position in 0..symbols.len() - 1 {
            if symbols[position] == symbols[position + 1] {
                continue;
            }
            let mut swapped = symbols.clone();
            swapped.swap(position, position + 1);
            let typed: String = swapped.into_iter().collect();
            assert!(
                hand::decode(&typed).is_err(),
                "seed {seed}: swapping symbols {} and {} was accepted",
                position + 1,
                position + 2
            );
        }
    }
}

#[test]
fn every_swap_of_neighbouring_groups_is_refused() {
    for seed in 0..32 {
        let key = key_from(seed);
        let symbols = symbols_of(&key);
        for group in 0..13 {
            let mut swapped = symbols.clone();
            for offset in 0..4 {
                swapped.swap(group * 4 + offset, (group + 1) * 4 + offset);
            }
            if swapped == symbols {
                continue;
            }
            let typed: String = swapped.into_iter().collect();
            assert!(
                hand::decode(&typed).is_err(),
                "seed {seed}: swapping groups {group} and {} was accepted",
                group + 1
            );
        }
    }
}

#[test]
fn case_and_separators_do_not_matter() {
    let key = key_from(7);
    let written = hand::encode(&key);
    let bare: String = written.chars().filter(|c| *c != '-').collect();
    assert_eq!(hand::decode(&written.to_lowercase()), Ok(key));
    assert_eq!(hand::decode(&bare), Ok(key));
    assert_eq!(hand::decode(&bare.to_lowercase()), Ok(key));

    // Somebody who regroups it their own way, or types it over two lines.
    let spaced: String = bare
        .chars()
        .enumerate()
        .flat_map(|(index, c)| {
            let separator = match index % 7 {
                0 => " ",
                3 => "\n",
                _ => "",
            };
            separator.chars().chain(core::iter::once(c))
        })
        .collect();
    assert_eq!(hand::decode(&spaced), Ok(key));
}

#[test]
fn crockford_reads_the_confusable_letters_as_the_digits_they_look_like() {
    let key = key_from(11);
    let written: String = hand::encode(&key).chars().filter(|c| *c != '-').collect();
    for (digit, letters) in [('1', ['I', 'i', 'L', 'l']), ('0', ['O', 'o', 'O', 'o'])] {
        for letter in letters {
            let confused: String = written
                .chars()
                .map(|c| if c == digit { letter } else { c })
                .collect();
            assert_eq!(
                hand::decode(&confused),
                Ok(key),
                "{letter} was not read as {digit}"
            );
        }
    }
}

#[test]
fn the_letter_u_is_refused_rather_than_guessed_at() {
    let written: String = hand::encode(&key_from(3))
        .chars()
        .filter(|c| *c != '-')
        .collect();
    let mut typed: Vec<char> = written.chars().collect();
    typed[5] = 'U';
    let typed: String = typed.into_iter().collect();
    assert_eq!(
        hand::decode(&typed),
        Err(HandError::UnknownSymbol { found: 'U', at: 6 })
    );
}

#[test]
fn a_character_outside_the_alphabet_names_where_it_is() {
    let written = hand::encode(&key_from(4));
    let mut typed: Vec<char> = written.chars().collect();
    // Symbol 12 is the last of the third group, so it sits after two hyphens:
    // the position reported counts symbols, not characters.
    typed[13] = '?';
    let typed: String = typed.into_iter().collect();
    assert_eq!(
        hand::decode(&typed),
        Err(HandError::UnknownSymbol { found: '?', at: 12 })
    );
}

#[test]
fn a_dropped_or_doubled_symbol_is_refused_by_length() {
    let written: String = hand::encode(&key_from(5))
        .chars()
        .filter(|c| *c != '-')
        .collect();
    assert_eq!(
        hand::decode(&written[..written.len() - 1]),
        Err(HandError::WrongLength {
            found: hand::SYMBOLS - 1,
            expected: hand::SYMBOLS
        })
    );
    let doubled = format!("{written}7");
    assert_eq!(
        hand::decode(&doubled),
        Err(HandError::WrongLength {
            found: hand::SYMBOLS + 1,
            expected: hand::SYMBOLS
        })
    );
    assert_eq!(
        hand::decode(""),
        Err(HandError::WrongLength {
            found: 0,
            expected: hand::SYMBOLS
        })
    );
}

#[test]
fn a_last_symbol_carrying_bits_a_key_cannot_is_not_canonical() {
    let mut symbols = symbols_of(&key_from(6));
    // Symbol 52 holds one bit of key and four of padding, so only two of the
    // thirty-two symbols can stand there. Any of the other thirty is proof the
    // string did not come from `encode`.
    let padded = ALPHABET
        .chars()
        .enumerate()
        .find(|(value, _)| value % 16 != 0)
        .map(|(_, symbol)| symbol)
        .expect("a symbol with padding bits set");
    symbols[51] = padded;
    let typed: String = symbols.into_iter().collect();
    assert_eq!(hand::decode(&typed), Err(HandError::NotCanonical));
}

#[test]
fn the_fingerprint_is_short_stable_and_tells_two_keys_apart() {
    let key = key_from(9);
    let printed = hand::fingerprint(&key);
    assert_eq!(printed, hand::fingerprint(&key));
    assert_eq!(printed.split('-').count(), 4);
    assert_eq!(printed.len(), 19);

    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..512 {
        assert!(
            seen.insert(hand::fingerprint(&key_from(seed))),
            "seed {seed} printed a fingerprint already seen"
        );
    }
}

#[test]
fn a_refusal_says_what_went_wrong_in_words() {
    let message = HandError::CheckFailed.to_string();
    assert!(message.contains("mistyped"), "{message}");
    let message = HandError::UnknownSymbol { found: 'U', at: 6 }.to_string();
    assert!(message.contains("symbol 6"), "{message}");
}
