//! Wire format and cryptography: what a hostile peer must not be able to do.

use lorai::domain::contracts::{Stage, Subject, Verdict};
use lorai::domain::time::Timestamp;
use lorai::wire::{Envelope, GroupKey, SigningKey, WireError, decode, encode, open, seal};

fn subject() -> Subject {
    Subject::new("m1", "e1", 0, [0x5A; 32], Timestamp::from_secs(1_000)).expect("valid")
}

fn envelope(author: &str) -> Envelope {
    Envelope::new(
        &subject(),
        author,
        Stage::BindingSupport,
        Verdict::Support,
        1,
    )
}

#[test]
fn a_round_trip_preserves_every_field() {
    let signer = SigningKey::from_seed([1; 32]);
    let frame = envelope("node-a").sign(&signer);
    let bytes = encode(&frame).expect("encodes");
    let back = decode(&bytes).expect("decodes");
    assert_eq!(frame, back);
}

#[test]
fn a_core_frame_fits_a_lora_payload() {
    // The smallest useful LoRa payload is 51 bytes at SF12. A core vote that
    // does not fit there cannot be sent at long range, which is the whole point
    // of the radio.
    let signer = SigningKey::from_seed([1; 32]);
    let bytes = encode(&envelope("n1").sign(&signer)).expect("encodes");
    assert!(
        bytes.len() <= 222,
        "core frame is {} bytes, over the largest LoRa payload",
        bytes.len()
    );
}

#[test]
fn a_valid_signature_verifies() {
    let signer = SigningKey::from_seed([1; 32]);
    let frame = envelope("node-a").sign(&signer);
    assert!(frame.verify(&signer.verifying_key()).is_ok());
}

#[test]
fn tampering_with_the_verdict_breaks_the_signature() {
    let signer = SigningKey::from_seed([1; 32]);
    let mut frame = envelope("node-a").sign(&signer);
    frame.tamper_verdict_for_test(Verdict::Dispute);
    assert_eq!(
        frame.verify(&signer.verifying_key()).unwrap_err(),
        WireError::BadSignature
    );
}

#[test]
fn tampering_with_the_subject_breaks_the_signature() {
    let signer = SigningKey::from_seed([1; 32]);
    let mut frame = envelope("node-a").sign(&signer);
    frame.tamper_revision_for_test(9);
    assert_eq!(
        frame.verify(&signer.verifying_key()).unwrap_err(),
        WireError::BadSignature
    );
}

#[test]
fn another_members_key_does_not_verify() {
    // Group encryption proves membership, never authorship. A member holding
    // the group key must not be able to pass as a different member.
    let author = SigningKey::from_seed([1; 32]);
    let impostor = SigningKey::from_seed([2; 32]);
    let frame = envelope("node-a").sign(&author);
    assert_eq!(
        frame.verify(&impostor.verifying_key()).unwrap_err(),
        WireError::BadSignature
    );
}

#[test]
fn group_encryption_round_trips() {
    let key = GroupKey::from_bytes([9; 32]);
    let signer = SigningKey::from_seed([1; 32]);
    let plaintext = encode(&envelope("node-a").sign(&signer)).expect("encodes");

    let sealed = seal(&key, 42, &plaintext).expect("seals");
    let opened = open(&key, 42, &sealed).expect("opens");
    assert_eq!(opened, plaintext);
}

#[test]
fn a_wrong_sequence_fails_to_open() {
    // The sequence number is the nonce and is bound as associated data, so a
    // replayed frame cannot be passed off under a different sequence.
    let key = GroupKey::from_bytes([9; 32]);
    let sealed = seal(&key, 42, b"payload").expect("seals");
    assert_eq!(
        open(&key, 43, &sealed).unwrap_err(),
        WireError::DecryptionFailed
    );
}

#[test]
fn a_wrong_group_key_fails_to_open() {
    let sealed = seal(&GroupKey::from_bytes([9; 32]), 1, b"payload").expect("seals");
    assert_eq!(
        open(&GroupKey::from_bytes([8; 32]), 1, &sealed).unwrap_err(),
        WireError::DecryptionFailed
    );
}

#[test]
fn a_flipped_ciphertext_bit_fails_to_open() {
    let key = GroupKey::from_bytes([9; 32]);
    let mut sealed = seal(&key, 1, b"payload").expect("seals");
    sealed[0] ^= 0x01;
    assert_eq!(
        open(&key, 1, &sealed).unwrap_err(),
        WireError::DecryptionFailed
    );
}

#[test]
fn truncated_bytes_are_rejected_not_guessed() {
    let signer = SigningKey::from_seed([1; 32]);
    let bytes = encode(&envelope("node-a").sign(&signer)).expect("encodes");
    assert!(decode(&bytes[..bytes.len() / 2]).is_err());
}

#[test]
fn the_signature_covers_a_domain_separated_transcript() {
    // The same fields at a different stage must produce a different signature,
    // or an independent opinion could be replayed as a binding vote.
    let signer = SigningKey::from_seed([1; 32]);
    let opinion =
        Envelope::new(&subject(), "n", Stage::Independent, Verdict::Support, 1).sign(&signer);
    let vote =
        Envelope::new(&subject(), "n", Stage::BindingSupport, Verdict::Support, 1).sign(&signer);
    assert_ne!(opinion.signature(), vote.signature());
}

#[test]
fn a_compact_frame_reaches_the_mid_range_spreading_factor() {
    // Strings are a luxury the radio cannot afford: the manifest already knows
    // every member and mission, so the wire carries indices into it. This is
    // what gets a core vote under the SF10 payload limit of 133 bytes.
    use lorai::wire::CompactEnvelope;

    let signer = SigningKey::from_seed([1; 32]);
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
    );
    let bytes = encode_compact(&compact.sign(&signer)).expect("encodes");
    let sealed = seal(&GroupKey::from_bytes([9; 32]), 4_242, &bytes).expect("seals");
    assert!(
        sealed.len() <= 133,
        "compact sealed frame is {} B, over the SF10 limit",
        sealed.len()
    );
}

#[test]
fn a_compact_frame_fits_a_raw_lora_payload_at_any_spreading_factor() {
    // This test previously asserted the opposite, on the premise that SF12 is
    // limited to 51 bytes. That figure is LoRaWAN's DR0 application-payload cap,
    // not a LoRa PHY limit, and lorai is peer-to-peer raw LoRa. The frame fits
    // everywhere; what it cannot afford is the airtime. See `DECISIONS.md` D2
    // and `tests/spreading.rs`, which measure the cost instead of assuming it.
    use lorai::wire::CompactEnvelope;

    let signer = SigningKey::from_seed([1; 32]);
    let compact = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1);
    let bytes = encode_compact(&compact.sign(&signer)).expect("encodes");
    assert!(
        bytes.len() <= 255,
        "compact frame is {} bytes, over the largest LoRa payload",
        bytes.len()
    );
}

use lorai::wire::encode_compact;
