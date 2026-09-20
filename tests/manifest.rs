//! Reading a fleet from a manifest file: what it accepts, and what it refuses
//! loudly enough for an operator to fix.

use lcq::application::{Manifest, ManifestError};
use lcq::domain::quorum::Competence;

const THREE: &str = "\
# Three vessels; the second and third defer to the first.
version 1
epoch 7

member 0 99 ship-alpha
member 1 66 ship-bravo
member 2 33 ship-charlie
";

#[test]
fn a_manifest_names_a_fleet_and_what_each_member_is_worth() {
    let manifest = Manifest::parse(THREE).expect("a valid manifest");
    assert_eq!(manifest.epoch(), 7);
    assert_eq!(manifest.size(), 3);
    assert_eq!(manifest.id_of(0), Some("ship-alpha"));
    assert_eq!(manifest.id_of(2), Some("ship-charlie"));
    assert_eq!(manifest.id_of(3), None);

    let policy = manifest.policy().expect("a lawful policy");
    assert_eq!(policy.total_competence(), 198);
    assert_eq!(
        policy.competence_of("ship-bravo").map(Competence::value),
        Some(66)
    );
    assert_eq!(policy.min_signers(), 3, "a fleet of three needs all three");
}

#[test]
fn comments_blank_lines_and_order_do_not_matter() {
    let shuffled = "\
version 1
epoch 7
member 2 33 ship-charlie   # the least sure of the three

member 0 99 ship-alpha
member 1 66 ship-bravo
";
    assert_eq!(
        Manifest::parse(shuffled).expect("valid"),
        Manifest::parse(THREE).expect("valid"),
        "a manifest is a set, however it is laid out"
    );
}

#[test]
fn a_fleet_may_be_uniform() {
    let text = "version 1\nepoch 1\nmember 0 100 a\nmember 1 100 b\nmember 2 100 c\n";
    let manifest = Manifest::parse(text).expect("valid");
    assert_eq!(manifest.policy().expect("policy").total_competence(), 300);
}

#[test]
fn an_index_that_names_nobody_is_refused() {
    // A frame names its author by index, so a gap would decode to nobody.
    let text = "version 1\nepoch 1\nmember 0 100 a\nmember 2 100 c\n";
    assert_eq!(
        Manifest::parse(text).unwrap_err(),
        ManifestError::IndicesNotContiguous {
            missing: 1,
            size: 2
        }
    );
}

#[test]
fn a_repeated_index_or_id_is_refused() {
    let repeated_index = "version 1\nepoch 1\nmember 0 100 a\nmember 0 100 b\n";
    assert_eq!(
        Manifest::parse(repeated_index).unwrap_err(),
        ManifestError::DuplicateIndex { line: 4, index: 0 }
    );
    let repeated_id = "version 1\nepoch 1\nmember 0 100 a\nmember 1 100 a\n";
    assert_eq!(
        Manifest::parse(repeated_id).unwrap_err(),
        ManifestError::DuplicateId {
            line: 4,
            id: "a".to_string()
        }
    );
}

#[test]
fn a_competence_off_the_scale_is_refused_at_the_door() {
    // Not at the first vote: a manifest that cannot be a policy is not a
    // manifest.
    let text = "version 1\nepoch 1\nmember 0 101 a\nmember 1 100 b\n";
    assert!(matches!(
        Manifest::parse(text).unwrap_err(),
        ManifestError::Policy(_)
    ));
    let spread = "version 1\nepoch 1\nmember 0 100 a\nmember 1 20 b\n";
    assert!(
        matches!(
            Manifest::parse(spread).unwrap_err(),
            ManifestError::Policy(_)
        ),
        "the ratio cap is part of being a fleet"
    );
}

#[test]
fn a_manifest_must_say_its_version_and_epoch_before_its_members() {
    assert_eq!(
        Manifest::parse("epoch 1\nmember 0 100 a\n").unwrap_err(),
        ManifestError::MissingVersion
    );
    assert_eq!(
        Manifest::parse("version 1\nmember 0 100 a\n").unwrap_err(),
        ManifestError::MissingEpoch
    );
    assert_eq!(
        Manifest::parse("version 1\nepoch 1\n").unwrap_err(),
        ManifestError::NoMembers
    );
    assert_eq!(
        Manifest::parse("").unwrap_err(),
        ManifestError::MissingVersion
    );
}

#[test]
fn a_version_this_build_does_not_understand_is_refused_by_name() {
    assert_eq!(
        Manifest::parse("version 2\nepoch 1\nmember 0 100 a\n").unwrap_err(),
        ManifestError::UnsupportedVersion {
            line: 1,
            version: 2
        }
    );
}

#[test]
fn every_complaint_names_its_line() {
    let cases: Vec<(&str, ManifestError)> = vec![
        (
            "version 1\nepoch 1\nfleet 3\n",
            ManifestError::UnknownDirective {
                line: 3,
                word: "fleet".to_string(),
            },
        ),
        (
            "version 1\nepoch 1\nmember 0 100\n",
            ManifestError::Malformed {
                line: 3,
                expected: "member <index> <competence> <id>",
            },
        ),
        (
            "version 1\nepoch 1\nmember 0 lots alpha\n",
            ManifestError::NotANumber {
                line: 3,
                field: "competence",
            },
        ),
        (
            "version 1\nepoch soon\nmember 0 100 a\n",
            ManifestError::NotANumber {
                line: 2,
                field: "epoch",
            },
        ),
        (
            "version 1\nversion 1\n",
            ManifestError::Repeated {
                line: 2,
                directive: "version",
            },
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(Manifest::parse(text).unwrap_err(), expected, "{text:?}");
    }
}

/// A manifest naming `size` members, all counting the same.
fn crowd(size: usize) -> String {
    use std::fmt::Write;

    let mut text = String::from("version 1\nepoch 1\n");
    for index in 0..size {
        writeln!(text, "member {index} 100 n{index}").expect("writing to a String");
    }
    text
}

#[test]
fn a_fleet_larger_than_a_frame_can_acknowledge_is_refused() {
    assert_eq!(
        Manifest::parse(&crowd(65)).unwrap_err(),
        ManifestError::FleetTooLarge { size: 65, max: 64 }
    );
    // Sixty-four is the most a frame's acknowledgement bitmap can name.
    assert_eq!(Manifest::parse(&crowd(64)).expect("valid").size(), 64);
}

#[test]
fn the_error_explains_itself_to_an_operator() {
    let text = "version 1\nepoch 1\nmember 0 100 a\nmember 2 100 c\n";
    let said = Manifest::parse(text).unwrap_err().to_string();
    assert!(said.contains('1'), "{said}");
    assert!(said.contains("missing"), "{said}");
}
