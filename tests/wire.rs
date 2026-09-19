//! Wire format and cryptography: what a hostile peer must not be able to do.

use lcq::domain::contracts::{Stage, Subject, Verdict};
use lcq::domain::time::Timestamp;
use lcq::wire::{Envelope, GroupKey, SigningKey, WireError, decode, encode, open, seal};

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

    let sealed = seal(&key, 3, 42, &plaintext).expect("seals");
    let opened = open(&key, 3, 42, &sealed).expect("opens");
    assert_eq!(opened, plaintext);
}

#[test]
fn a_wrong_sequence_fails_to_open() {
    // The sequence number is the nonce and is bound as associated data, so a
    // replayed frame cannot be passed off under a different sequence.
    let key = GroupKey::from_bytes([9; 32]);
    let sealed = seal(&key, 3, 42, b"payload").expect("seals");
    assert_eq!(
        open(&key, 3, 43, &sealed).unwrap_err(),
        WireError::DecryptionFailed
    );
}

#[test]
fn a_wrong_group_key_fails_to_open() {
    let sealed = seal(&GroupKey::from_bytes([9; 32]), 3, 1, b"payload").expect("seals");
    assert_eq!(
        open(&GroupKey::from_bytes([8; 32]), 3, 1, &sealed).unwrap_err(),
        WireError::DecryptionFailed
    );
}

#[test]
fn a_flipped_ciphertext_bit_fails_to_open() {
    let key = GroupKey::from_bytes([9; 32]);
    let mut sealed = seal(&key, 3, 1, b"payload").expect("seals");
    sealed[0] ^= 0x01;
    assert_eq!(
        open(&key, 3, 1, &sealed).unwrap_err(),
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
fn the_compact_form_costs_less_airtime_once_names_are_realistic() {
    // Strings are a luxury the radio cannot afford: the manifest already knows
    // every member and mission, so the wire carries indices into it.
    //
    // This used to assert a 133-byte "SF10 payload limit". That is another
    // LoRaWAN application cap, not a LoRa one -- the same false premise
    // `DECISIONS.md` D2 corrected elsewhere. Raw LoRa carries 255 bytes at every
    // spreading factor; what a frame cannot afford is the time.
    //
    // The comparison needs identifiers a real fleet would use. Against "n12"
    // the compact form saves nothing and, since it now also carries an eight
    // byte acknowledgement, costs slightly more -- which says only that the
    // test was measuring toy names.
    use lcq::domain::contracts::{Stage, Subject, Verdict};
    use lcq::sim::airtime_ms;
    use lcq::wire::{CompactEnvelope, Envelope, seal_frame};

    let signer = SigningKey::from_seed([1; 32]);
    let group = GroupKey::from_bytes([9; 32]);

    let named = Subject::new(
        "baltic-winter-patrol-2026",
        "grounding-risk-report-0417",
        0,
        [0x5A; 32],
        Timestamp::from_secs(1_700_000_000),
    )
    .expect("valid subject");
    let readable = Envelope::new(
        &named,
        "MV Stena Nordica",
        Stage::BindingSupport,
        Verdict::Support,
        4_242,
    );
    let readable = encode(&readable.sign(&signer)).expect("encodes");

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
    let compact = encode_compact(&compact.sign(&signer)).expect("encodes");

    let readable_air = airtime_ms(
        seal_frame(&group, 12, 4_242, &readable)
            .expect("seals")
            .len(),
    );
    let compact_air = airtime_ms(
        seal_frame(&group, 12, 4_242, &compact)
            .expect("seals")
            .len(),
    );

    assert!(
        compact_air < readable_air,
        "compact {compact_air} ms against readable {readable_air} ms"
    );
    // Every frame this protocol will ever send pays this, so the saving is
    // worth stating as a proportion rather than a byte count.
    assert!(
        compact_air * 5 < readable_air * 4,
        "the compact form should save at least a fifth of the airtime: \
         {compact_air} ms against {readable_air} ms"
    );
}

#[test]
fn a_compact_frame_fits_a_raw_lora_payload_at_any_spreading_factor() {
    // This test previously asserted the opposite, on the premise that SF12 is
    // limited to 51 bytes. That figure is LoRaWAN's DR0 application-payload cap,
    // not a LoRa PHY limit, and lcq is peer-to-peer raw LoRa. The frame fits
    // everywhere; what it cannot afford is the airtime. See `DECISIONS.md` D2
    // and `tests/spreading.rs`, which measure the cost instead of assuming it.
    use lcq::wire::CompactEnvelope;

    let signer = SigningKey::from_seed([1; 32]);
    let compact = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1);
    let bytes = encode_compact(&compact.sign(&signer)).expect("encodes");
    assert!(
        bytes.len() <= 255,
        "compact frame is {} bytes, over the largest LoRa payload",
        bytes.len()
    );
}

use lcq::wire::encode_compact;

#[test]
fn two_members_counting_from_zero_do_not_share_a_nonce() {
    // The group key is shared but each member counts its own sequence, so
    // without the author index in the nonce the first frame of any two members
    // would seal under identical keystream. The XOR of the ciphertexts would
    // then be the XOR of the plaintexts, which is total loss of confidentiality
    // and hands over the Poly1305 key as well.
    use lcq::wire::{GroupKey, seal};

    let group = GroupKey::from_bytes([0x5a; 32]);
    let a = seal(&group, 0, 0, b"aaaaaaaaaaaaaaaaaaaa").expect("seals");
    let b = seal(&group, 1, 0, b"bbbbbbbbbbbbbbbbbbbb").expect("seals");

    let cipher_xor: Vec<u8> = a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect();
    let plain_xor: Vec<u8> = b"aaaaaaaaaaaaaaaaaaaa"
        .iter()
        .zip(b"bbbbbbbbbbbbbbbbbbbb".iter())
        .map(|(x, y)| x ^ y)
        .collect();
    assert_ne!(cipher_xor[..20], plain_xor[..20]);
}

#[test]
fn a_frame_taken_off_the_air_can_be_opened_without_prior_knowledge() {
    // The nonce cannot live inside the thing it decrypts. A receiver holding
    // only the group key and the bytes must be able to open the frame.
    use lcq::wire::{GroupKey, open_frame, seal_frame};

    let group = GroupKey::from_bytes([0x5a; 32]);
    let frame = seal_frame(&group, 7, 12_345, b"binding support").expect("seals");
    let (author, sequence, plaintext) = open_frame(&group, &frame).expect("opens");
    assert_eq!(author, 7);
    assert_eq!(sequence, 12_345);
    assert_eq!(plaintext, b"binding support");
}

#[test]
fn altering_the_cleartext_header_breaks_the_frame() {
    // The header is associated data, so it is authenticated even though it is
    // readable. Rewriting it in flight destroys the frame rather than
    // redirecting it.
    use lcq::wire::{GroupKey, open_frame, seal_frame};

    let group = GroupKey::from_bytes([0x5a; 32]);
    let mut frame = seal_frame(&group, 7, 12_345, b"binding support").expect("seals");
    frame[1] ^= 0x01;
    assert!(open_frame(&group, &frame).is_err());
}

#[test]
fn a_frame_shorter_than_its_header_is_refused_not_guessed() {
    use lcq::wire::{FRAME_HEADER_BYTES, GroupKey, open_frame};

    let group = GroupKey::from_bytes([0x5a; 32]);
    assert!(open_frame(&group, &[0u8; FRAME_HEADER_BYTES]).is_err());
    assert!(open_frame(&group, &[]).is_err());
}

#[test]
fn an_acknowledgement_records_exactly_who_was_heard() {
    use lcq::wire::Heard;

    let mut heard = Heard::none();
    assert_eq!(heard.count(), 0);
    heard.heard_from(0);
    heard.heard_from(7);
    heard.heard_from(63);
    assert!(heard.contains(0) && heard.contains(7) && heard.contains(63));
    assert!(!heard.contains(1) && !heard.contains(62));
    assert_eq!(heard.count(), 3);
}

#[test]
fn an_index_past_the_capacity_is_dropped_rather_than_wrapped() {
    // Dropping costs a retransmission. Wrapping would silence the wrong member,
    // which is a correctness failure dressed as an optimisation.
    use lcq::wire::Heard;

    let mut heard = Heard::none();
    heard.heard_from(64);
    heard.heard_from(1_000);
    assert_eq!(heard.count(), 0);
    assert!(!heard.contains(0));
}

#[test]
fn the_acknowledgement_is_covered_by_the_signature() {
    // Otherwise anyone could rewrite it in flight and silence a member that had
    // not in fact been heard.
    use lcq::wire::{CompactEnvelope, Heard, decode_compact};

    let signer = SigningKey::from_seed([1; 32]);
    let base = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1);
    let mut heard = Heard::none();
    heard.heard_from(4);

    assert_ne!(
        base.transcript(),
        base.clone().acknowledging(heard).transcript(),
        "the bitmap must change what is signed"
    );
    // And a frame whose bitmap was rewritten after signing no longer verifies.
    let acknowledging = base.clone().acknowledging(heard).sign(&signer);
    let bytes = encode_compact(&acknowledging).expect("encodes");
    let decoded = decode_compact(&bytes).expect("decodes");
    assert!(decoded.verify(&signer.verifying_key()).is_ok());
    let forged = base.acknowledging(Heard::none()).sign(&signer);
    assert_ne!(
        encode_compact(&forged).expect("encodes"),
        bytes,
        "two different bitmaps must not encode identically"
    );
}

#[test]
fn an_acknowledgement_survives_the_round_trip() {
    use lcq::wire::{CompactEnvelope, Heard, decode_compact};

    let signer = SigningKey::from_seed([1; 32]);
    let mut heard = Heard::none();
    heard.heard_from(2);
    heard.heard_from(9);
    let envelope = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1).acknowledging(heard);
    let bytes = encode_compact(&envelope.sign(&signer)).expect("encodes");
    let back = decode_compact(&bytes).expect("decodes");
    assert_eq!(back.envelope().heard(), heard);
    assert!(back.verify(&signer.verifying_key()).is_ok());
}

#[test]
fn the_header_can_be_read_without_opening_the_frame() {
    use lcq::wire::{GroupKey, peek_frame_header, seal_frame};

    let group = GroupKey::from_bytes([0x5a; 32]);
    let frame = seal_frame(&group, 9, 777, b"anything").expect("seals");
    assert_eq!(peek_frame_header(&frame).expect("peeks"), (9, 777));
    assert!(peek_frame_header(&frame[..5]).is_err());
}

#[test]
fn a_round_is_named_by_who_opened_it_and_under_which_sequence() {
    use lcq::wire::{CompactEnvelope, RoundId, decode_compact};

    let signer = SigningKey::from_seed([1; 32]);
    let round = RoundId::new(3, 41);
    assert!(round.is_set());
    assert!(!RoundId::none().is_set());
    assert_ne!(
        round,
        RoundId::new(3, 42),
        "same opener, next sequence: a different round"
    );
    assert_ne!(
        round,
        RoundId::new(4, 41),
        "same sequence, other opener: a different round"
    );

    let labelled = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1).in_round(round);
    let bytes = encode_compact(&labelled.sign(&signer)).expect("encodes");
    let back = decode_compact(&bytes).expect("decodes");
    assert_eq!(back.envelope().round(), round);
    assert!(back.verify(&signer.verifying_key()).is_ok());
}

#[test]
fn the_round_is_covered_by_the_signature() {
    // Otherwise a vote cast in one round could be lifted into another.
    use lcq::wire::{CompactEnvelope, RoundId};

    let base = CompactEnvelope::new(1, 1, 0, [0; 32], 0, 1, 3, 1, 1);
    assert_ne!(
        base.transcript(),
        base.in_round(RoundId::new(0, 1)).transcript()
    );
}

#[test]
fn the_widest_possible_frame_fits_the_slot_it_is_sized_for() {
    // Every varint at its longest encoding, every acknowledgement bit set, a
    // round label at its maximum: nothing the protocol can legally send is
    // wider than this. The slot is sized to MAX_FRAME_BYTES, so this is the
    // frame that must fit -- and the constant must not be loose enough to hide
    // a field that quietly grew.
    use lcq::wire::{CompactEnvelope, GroupKey, Heard, MAX_FRAME_BYTES, RoundId, seal_frame};

    let signer = SigningKey::from_seed([1; 32]);
    let group = GroupKey::from_bytes([9; 32]);
    let mut heard = Heard::none();
    for i in 0..Heard::CAPACITY {
        heard.heard_from(i);
    }
    let widest = CompactEnvelope::new(
        u16::MAX,
        u32::MAX,
        u16::MAX,
        [0xFF; 32],
        u64::MAX,
        u16::MAX,
        u8::MAX,
        u8::MAX,
        u64::MAX,
    )
    .acknowledging(heard)
    .in_round(RoundId::new(u16::MAX - 1, u32::MAX));
    let bytes = encode_compact(&widest.sign(&signer)).expect("encodes");
    let on_air = seal_frame(&group, u16::MAX, u64::MAX, &bytes).expect("seals");

    assert!(
        on_air.len() <= MAX_FRAME_BYTES,
        "widest frame is {} B, over the {MAX_FRAME_BYTES} B slot",
        on_air.len()
    );
    assert!(
        on_air.len() + 8 >= MAX_FRAME_BYTES,
        "slot is {MAX_FRAME_BYTES} B for a {} B frame: too loose to catch growth",
        on_air.len()
    );
}
