//! Reading a fleet from a signed manifest: what it accepts, what it refuses
//! loudly enough for an operator to fix, and what a signature actually covers.

use std::fmt::Write as _;

use lcq::application::{MAX_LABEL_BYTES, Manifest, ManifestError};
use lcq::domain::quorum::Competence;
use lcq::wire::{SigningKey, VerifyingKey, hand};

/// The administrator's key for these tests. An insecure fixture: a real one
/// never leaves the person who issues manifests.
fn admin() -> SigningKey {
    SigningKey::from_seed([7; 32])
}

/// Another card entirely, for the tests about signing under the wrong one.
fn impostor() -> SigningKey {
    SigningKey::from_seed([9; 32])
}

fn member_key(seed: u8) -> SigningKey {
    SigningKey::from_seed([seed; 32])
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("a string never fails to grow");
    }
    out
}

fn key_hex(seed: u8) -> String {
    hex(&member_key(seed).verifying_key().to_bytes())
}

const FROM: u64 = 1_789_000_000;
const UNTIL: u64 = 1_792_000_000;
/// Somewhere inside the window.
const NOW: u64 = 1_790_000_000;

/// A manifest body without the issuer and signature lines.
fn body(members: &str) -> String {
    format!(
        "version 2
epoch 7
valid-from {FROM}
valid-until {UNTIL}
group-key {group}
byzantine-bps 4000
max-competence-ratio 3
phy-profile eu868-sf10-v1
heard-capacity 64
{members}",
        group = hex(&[0x3f; 32]),
    )
}

/// Three vessels, the second and third deferring to the first.
fn three() -> String {
    body(&format!(
        "member 0 100 {a} ship-alpha
member 1 66 {b} ship-bravo
member 2 34 {c} ship-charlie
",
        a = key_hex(1),
        b = key_hex(2),
        c = key_hex(3),
    ))
}

fn signed(text: &str) -> String {
    Manifest::sign_text(text, &admin()).expect("a body that only wants a signature")
}

fn parse_signed(text: &str) -> Manifest {
    Manifest::parse(&signed(text)).expect("a signed manifest")
}

#[test]
fn a_signed_manifest_names_a_fleet_and_what_each_member_is_worth() {
    let manifest = parse_signed(&three());
    assert_eq!(manifest.epoch(), 7);
    assert_eq!(manifest.size(), 3);
    assert_eq!(manifest.valid_from(), FROM);
    assert_eq!(manifest.valid_until(), UNTIL);
    assert_eq!(manifest.group_key_id(), &[0x3f; 32]);
    assert_eq!(manifest.phy_profile(), "eu868-sf10-v1");
    assert_eq!(manifest.id_of(1), Some("ship-bravo"));
    assert_eq!(
        manifest.key_of(1),
        Some(&member_key(2).verifying_key()),
        "index 1 must name the key seeded for ship-bravo"
    );

    manifest
        .verify(&admin().verifying_key(), NOW)
        .expect("signed by the administrator, inside its window");

    let policy = manifest.policy().expect("a lawful fleet");
    assert_eq!(policy.total_competence(), 200);
    assert_eq!(
        policy.competence_of("ship-alpha"),
        Competence::try_new(100).ok()
    );
    assert_eq!(policy.byzantine_bps(), 4000);
    assert_eq!(policy.max_competence_ratio(), 3);
}

#[test]
fn the_issuer_line_says_which_card_without_deciding_anything() {
    let manifest = parse_signed(&three());
    assert_eq!(
        manifest.issuer_fingerprint(),
        hand::fingerprint(&admin().verifying_key().to_bytes()),
        "the issuer line is the administrator's fingerprint"
    );

    // And it decides nothing: a manifest whose issuer line names the right
    // card still fails under the wrong key.
    assert_eq!(
        manifest.verify(&impostor().verifying_key(), NOW),
        Err(ManifestError::BadSignature)
    );
}

#[test]
fn two_spellings_of_one_fleet_are_one_manifest() {
    let plain = parse_signed(&three());

    // The same fleet, written by somebody else: comments, blank lines, other
    // spacing, and the members in another order. The signature is over the
    // canonical form, so none of that can change what this says.
    let rearranged = body(&format!(
        "# The fast one first, because it is the one we trust most.

member 2 34 {c} ship-charlie

member 0   100   {a}   ship-alpha   # flagship
member 1 66 {b} ship-bravo
",
        a = key_hex(1),
        b = key_hex(2),
        c = key_hex(3),
    ));
    let other = parse_signed(&rearranged);

    assert_eq!(plain.digest(), other.digest());
    assert_eq!(plain.canonical_bytes(), other.canonical_bytes());
    assert_eq!(plain.members(), other.members());
}

#[test]
fn changing_anything_that_matters_breaks_the_signature() {
    let original = signed(&three());
    let admin = admin().verifying_key();

    // Each of these is a field the fleet's behaviour depends on. An attacker
    // who can write the file can still write any of them; what they cannot do
    // is have the result verify.
    for (was, now, what) in [
        ("epoch 7", "epoch 8", "the mission epoch"),
        ("member 1 66 ", "member 1 99 ", "a member's competence"),
        (
            "byzantine-bps 4000",
            "byzantine-bps 3000",
            "the fault budget",
        ),
        (
            "max-competence-ratio 3",
            "max-competence-ratio 9",
            "the ratio cap",
        ),
        (
            "valid-until 1792000000",
            "valid-until 1892000000",
            "the window",
        ),
        ("ship-bravo", "ship-bravery", "a member's label"),
    ] {
        let tampered = original.replace(was, now);
        assert_ne!(tampered, original, "the test did not change {what}");
        let manifest = Manifest::parse(&tampered).expect("still structurally a manifest");
        assert_eq!(
            manifest.verify(&admin, NOW),
            Err(ManifestError::BadSignature),
            "changing {what} was accepted"
        );
    }
}

#[test]
fn swapping_a_members_key_breaks_the_signature() {
    let original = signed(&three());
    let manifest = Manifest::parse(&original.replace(&key_hex(2), &key_hex(4)))
        .expect("still structurally a manifest");
    assert_eq!(
        manifest.verify(&admin().verifying_key(), NOW),
        Err(ManifestError::BadSignature)
    );
}

#[test]
fn a_manifest_signed_by_another_card_is_refused() {
    let text = Manifest::sign_text(&three(), &impostor()).expect("signs under any key");
    let manifest = Manifest::parse(&text).expect("structurally fine");
    assert_eq!(
        manifest.verify(&admin().verifying_key(), NOW),
        Err(ManifestError::BadSignature)
    );
    manifest
        .verify(&impostor().verifying_key(), NOW)
        .expect("it does verify under the card that signed it");
}

#[test]
fn a_flipped_bit_in_the_signature_is_refused() {
    let text = signed(&three());
    let line = text
        .lines()
        .find(|line| line.starts_with("signature "))
        .expect("a signature line");
    let mut digits: Vec<char> = line["signature ".len()..].chars().collect();
    digits[0] = if digits[0] == '0' { '1' } else { '0' };
    let flipped: String = digits.into_iter().collect();
    let manifest = Manifest::parse(&text.replace(line, &format!("signature {flipped}")))
        .expect("still 64 bytes of hexadecimal");
    assert_eq!(
        manifest.verify(&admin().verifying_key(), NOW),
        Err(ManifestError::BadSignature)
    );
}

#[test]
fn the_window_is_half_open_so_two_manifests_leave_no_gap() {
    let manifest = parse_signed(&three());
    let admin = admin().verifying_key();

    assert_eq!(
        manifest.verify(&admin, FROM - 1),
        Err(ManifestError::NotYetValid {
            from: FROM,
            now: FROM - 1
        })
    );
    manifest
        .verify(&admin, FROM)
        .expect("the first instant is inside");
    manifest
        .verify(&admin, UNTIL - 1)
        .expect("the last instant is inside");
    assert_eq!(
        manifest.verify(&admin, UNTIL),
        Err(ManifestError::Expired {
            until: UNTIL,
            now: UNTIL
        }),
        "the instant it names is the first it is not good for"
    );
}

#[test]
fn a_window_that_closes_before_it_opens_is_not_a_window() {
    let text = three().replace("valid-until 1792000000", "valid-until 1789000000");
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::ValidityInverted {
            from: FROM,
            until: FROM
        }),
        "an empty window is refused before anything signs it"
    );
}

#[test]
fn the_unsigned_format_is_gone_and_says_so() {
    let old = "version 1
epoch 7
member 0 99 ship-alpha
";
    assert_eq!(
        Manifest::parse(old),
        Err(ManifestError::UnsupportedVersion {
            line: 1,
            version: 1
        })
    );
}

#[test]
fn a_manifest_without_a_signature_is_not_a_manifest() {
    assert_eq!(
        Manifest::parse(&three()),
        Err(ManifestError::MissingDirective {
            directive: "signature"
        })
    );
}

#[test]
fn every_header_the_fleet_depends_on_must_be_there() {
    for (line, directive) in [
        ("epoch 7", "epoch"),
        ("valid-from 1789000000", "valid-from"),
        ("valid-until 1792000000", "valid-until"),
        ("byzantine-bps 4000", "byzantine-bps"),
        ("max-competence-ratio 3", "max-competence-ratio"),
        ("phy-profile eu868-sf10-v1", "phy-profile"),
        ("heard-capacity 64", "heard-capacity"),
    ] {
        let without = three().replace(&format!("{line}\n"), "");
        assert_eq!(
            Manifest::sign_text(&without, &admin()),
            Err(ManifestError::MissingDirective { directive }),
            "a manifest with no {directive} was accepted"
        );
    }
}

#[test]
fn a_header_after_the_members_is_a_file_somebody_appended_to() {
    let text = format!("{}epoch 9\n", three());
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::OutOfOrder {
            line: 13,
            directive: "a header directive"
        })
    );
}

#[test]
fn nothing_follows_the_signature() {
    let text = format!(
        "{}member 3 50 {}, ship-delta\n",
        signed(&three()),
        key_hex(5)
    );
    assert_eq!(
        Manifest::parse(&text),
        Err(ManifestError::OutOfOrder {
            line: 15,
            directive: "signature"
        })
    );
}

#[test]
fn a_directive_that_must_appear_once_may_not_appear_twice() {
    let text = three().replace("epoch 7\n", "epoch 7\nepoch 8\n");
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::Repeated {
            line: 3,
            directive: "epoch"
        })
    );
}

#[test]
fn two_members_may_not_share_an_index_a_label_or_a_key() {
    let shared_index = body(&format!(
        "member 0 100 {a} ship-alpha
member 0 66 {b} ship-bravo
",
        a = key_hex(1),
        b = key_hex(2),
    ));
    assert_eq!(
        Manifest::sign_text(&shared_index, &admin()),
        Err(ManifestError::DuplicateIndex { line: 11, index: 0 })
    );

    let shared_label = body(&format!(
        "member 0 100 {a} ship-alpha
member 1 66 {b} ship-alpha
",
        a = key_hex(1),
        b = key_hex(2),
    ));
    assert_eq!(
        Manifest::sign_text(&shared_label, &admin()),
        Err(ManifestError::DuplicateId {
            line: 11,
            id: "ship-alpha".to_string()
        })
    );

    // One principal, one seat. Two seats behind one key would let a single
    // holder sign twice and look like two signers to a quorum.
    let shared_key = body(&format!(
        "member 0 100 {a} ship-alpha
member 1 66 {a} ship-bravo
",
        a = key_hex(1),
    ));
    assert_eq!(
        Manifest::sign_text(&shared_key, &admin()),
        Err(ManifestError::DuplicateKey { line: 11, index: 1 })
    );
}

#[test]
fn indices_must_cover_the_fleet_with_no_gap() {
    let text = body(&format!(
        "member 0 100 {a} ship-alpha
member 2 66 {b} ship-bravo
",
        a = key_hex(1),
        b = key_hex(2),
    ));
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::IndicesNotContiguous {
            missing: 1,
            size: 2
        })
    );
}

#[test]
fn a_label_is_an_operator_label_and_is_held_to_it() {
    let odd = body(&format!("member 0 100 {a} ship alpha\n", a = key_hex(1)));
    assert!(
        matches!(
            Manifest::sign_text(&odd, &admin()),
            Err(ManifestError::Malformed { line: 10, .. })
        ),
        "a space in a label makes it two words"
    );

    let punctuated = body(&format!("member 0 100 {a} ship/alpha\n", a = key_hex(1)));
    assert_eq!(
        Manifest::sign_text(&punctuated, &admin()),
        Err(ManifestError::LabelCharset {
            line: 10,
            id: "ship/alpha".to_string()
        })
    );

    let long = "a".repeat(MAX_LABEL_BYTES + 1);
    let too_long = body(&format!("member 0 100 {a} {long}\n", a = key_hex(1)));
    assert_eq!(
        Manifest::sign_text(&too_long, &admin()),
        Err(ManifestError::LabelTooLong {
            line: 10,
            found: MAX_LABEL_BYTES + 1
        })
    );
}

#[test]
fn one_number_has_one_spelling() {
    // A sign or a leading zero would let one value be written two ways, and a
    // signature over one of them would say nothing about the other.
    for (was, now) in [
        ("epoch 7", "epoch 07"),
        ("epoch 7", "epoch +7"),
        ("byzantine-bps 4000", "byzantine-bps 04000"),
        ("member 0 100 ", "member 00 100 "),
    ] {
        let text = three().replace(was, now);
        assert!(
            matches!(
                Manifest::sign_text(&text, &admin()),
                Err(ManifestError::NotCanonicalNumber { .. })
            ),
            "{now:?} was accepted"
        );
    }
}

#[test]
fn hexadecimal_has_one_spelling_too() {
    let uppercase = three().replace(&key_hex(1), &key_hex(1).to_uppercase());
    assert_eq!(
        Manifest::sign_text(&uppercase, &admin()),
        Err(ManifestError::NotHex {
            line: 10,
            field: "member key",
            bytes: 32
        }),
        "uppercase hexadecimal is a second spelling of one value"
    );

    let short = three().replace(&key_hex(1), &key_hex(1)[..62]);
    assert!(matches!(
        Manifest::sign_text(&short, &admin()),
        Err(ManifestError::NotHex { bytes: 32, .. })
    ));
}

#[test]
fn thirty_two_bytes_that_are_not_a_public_key_are_refused() {
    // Not every 32 bytes are a point on the curve. Rather than hard-coding one
    // that is not -- which would pin this test to one curve implementation --
    // look for the first pattern this build itself rejects, then check that a
    // manifest naming it is refused at the door rather than at the first frame
    // that fails to verify.
    let mut candidate = [0u8; 32];
    let bad = (0..=u16::MAX).find_map(|counter| {
        candidate[..2].copy_from_slice(&counter.to_be_bytes());
        VerifyingKey::from_bytes(&candidate)
            .is_err()
            .then_some(candidate)
    });
    let Some(bad) = bad else {
        // Nothing to test on a build whose keys accept anything.
        return;
    };

    let text = three().replace(&key_hex(1), &hex(&bad));
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::NotAKey { line: 10, index: 0 }),
        "a manifest naming a key that cannot exist must be refused at the door"
    );
}

#[test]
fn a_fleet_must_fly_the_profile_this_build_flies() {
    let text = three().replace("phy-profile eu868-sf10-v1", "phy-profile eu433-sf7-v1");
    assert_eq!(
        Manifest::sign_text(&text, &admin()),
        Err(ManifestError::PhyProfileMismatch {
            line: 8,
            found: "eu433-sf7-v1".to_string(),
            expected: "eu868-sf10-v1"
        })
    );

    let capacity = three().replace("heard-capacity 64", "heard-capacity 32");
    assert_eq!(
        Manifest::sign_text(&capacity, &admin()),
        Err(ManifestError::HeardCapacityMismatch {
            line: 9,
            found: 32,
            expected: 64
        })
    );
}

#[test]
fn a_fleet_larger_than_a_frame_can_acknowledge_is_refused() {
    let mut members = String::new();
    for index in 0..65u16 {
        let seed = u8::try_from(index).expect("under 65");
        writeln!(
            members,
            "member {index} 100 {key} ship-{index}",
            key = key_hex(seed + 10)
        )
        .expect("a string never fails to grow");
    }
    assert_eq!(
        Manifest::sign_text(&body(&members), &admin()),
        Err(ManifestError::FleetTooLarge { size: 65, max: 64 })
    );
}

#[test]
fn a_manifest_that_parses_but_cannot_be_a_policy_is_refused_at_the_door() {
    // 100 against 20 is past the 3:1 cap, so this is not a lawful fleet even
    // though every line of it reads.
    let text = body(&format!(
        "member 0 100 {a} ship-alpha
member 1 20 {b} ship-bravo
",
        a = key_hex(1),
        b = key_hex(2),
    ));
    let signed = Manifest::sign_text(&text, &admin()).expect("signing does not judge a policy");
    assert!(matches!(
        Manifest::parse(&signed),
        Err(ManifestError::Policy(_))
    ));
}

#[test]
fn signing_an_already_signed_manifest_replaces_the_signature() {
    let once = signed(&three());
    let twice = Manifest::sign_text(&once, &admin()).expect("re-signing an edited manifest");
    assert_eq!(
        twice.matches("signature ").count(),
        1,
        "the old signature line must go, not accumulate"
    );
    assert_eq!(twice.matches("issuer ").count(), 1);
    Manifest::parse(&twice)
        .expect("still a manifest")
        .verify(&admin().verifying_key(), NOW)
        .expect("and it still verifies");
}

#[test]
fn the_identity_is_the_digest_of_what_was_signed() {
    let manifest = parse_signed(&three());
    let expected: [u8; 32] = {
        use blake2::{Blake2s256, Digest};
        Blake2s256::digest(manifest.canonical_bytes()).into()
    };
    assert_eq!(manifest.digest(), expected);

    // And a different fleet is a different manifest.
    let other = parse_signed(&three().replace("epoch 7", "epoch 8"));
    assert_ne!(manifest.digest(), other.digest());
}

#[test]
fn the_manifest_the_documents_ship_is_the_one_this_build_reads() {
    let text = std::fs::read_to_string("docs/three-vessels.manifest")
        .expect("the example manifest ships with the documents");
    let manifest = Manifest::parse(&text).expect("the shipped example must parse");
    assert_eq!(manifest.size(), 3);
    assert_eq!(
        manifest.phy_profile(),
        "eu868-sf10-v1",
        "the example must fly the profile this build flies"
    );
    manifest.policy().expect("and be a lawful fleet");
}

#[test]
fn how_a_file_was_written_cannot_change_what_it_says() {
    // The canonical form is built after parsing, so nothing about the file's
    // typography may reach the signature. This is the class of gap worth
    // hunting: two files that a person would call the same manifest must sign
    // to the same bytes, and two that differ in substance must not.
    let plain = signed(&three());
    let reference = Manifest::parse(&plain).expect("a signed manifest");

    for (what, rewritten) in [
        ("carriage returns", plain.replace('\n', "\r\n")),
        ("no trailing newline", plain.trim_end().to_string()),
        ("blank lines throughout", plain.replace('\n', "\n\n")),
        ("leading blank lines", format!("\n\n{plain}")),
        ("trailing spaces", plain.replace('\n', "   \n")),
        (
            "tabs for spaces",
            plain.replace("member 0 100", "member\t0\t100"),
        ),
        (
            "a comment on every line",
            plain
                .lines()
                .map(|line| format!("{line} # written by hand\n"))
                .collect(),
        ),
    ] {
        let other = Manifest::parse(&rewritten)
            .unwrap_or_else(|error| panic!("{what} stopped it parsing: {error}"));
        assert_eq!(
            other.canonical_bytes(),
            reference.canonical_bytes(),
            "{what} changed what the manifest says"
        );
        assert_eq!(
            other.digest(),
            reference.digest(),
            "{what} changed its identity"
        );
        other
            .verify(&admin().verifying_key(), NOW)
            .unwrap_or_else(|error| panic!("{what} broke the signature: {error}"));
    }
}

#[test]
fn a_second_signature_line_is_not_a_second_chance() {
    // Appending is the cheapest attack on a line-oriented format: leave the
    // real manifest alone and add a line that a sloppy parser reads last.
    let plain = signed(&three());
    let forged = Manifest::sign_text(&three().replace("epoch 7", "epoch 8"), &impostor())
        .expect("the impostor signs their own");
    let appended = format!(
        "{plain}{}\n",
        forged
            .lines()
            .find(|line| line.starts_with("signature "))
            .expect("a signature line")
    );
    assert_eq!(
        Manifest::parse(&appended),
        Err(ManifestError::OutOfOrder {
            line: 15,
            directive: "signature"
        }),
        "a manifest with two signatures is not a manifest"
    );
}
