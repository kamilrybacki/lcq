//! Safety state that must survive a restart, and the order it must be written in.

use lorai::application::{Journal, JournalError, OutgoingFrame};
use lorai::domain::contracts::Subject;
use lorai::domain::time::Timestamp;
use lorai::infrastructure::MemoryJournal;

fn subject(rev: u32) -> Subject {
    Subject::new("m1", "e1", rev, [3; 32], Timestamp::from_secs(1_000)).expect("valid")
}

#[test]
fn a_vote_lock_is_taken_before_the_frame_is_exposed() {
    // The ordering is the whole point: if the frame were queued first and the
    // node died, it could vote again after restart while its first vote was
    // already on the air.
    let mut journal = MemoryJournal::default();
    let frame = OutgoingFrame::new(b"vote-bytes".to_vec(), 1);

    journal
        .commit_vote(&subject(0), "self", frame)
        .expect("first vote");

    assert!(journal.has_voted(&subject(0), "self"));
    assert_eq!(journal.pending().count(), 1);
}

#[test]
fn a_second_vote_on_the_same_case_is_refused_and_queues_nothing() {
    let mut journal = MemoryJournal::default();
    journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"a".to_vec(), 1))
        .expect("first vote");

    let err = journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"b".to_vec(), 2))
        .unwrap_err();

    assert_eq!(err, JournalError::AlreadyVoted);
    assert_eq!(
        journal.pending().count(),
        1,
        "refused vote must not enqueue"
    );
}

#[test]
fn a_different_revision_is_a_different_lock() {
    let mut journal = MemoryJournal::default();
    journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"a".to_vec(), 1))
        .expect("revision 0");
    journal
        .commit_vote(&subject(1), "self", OutgoingFrame::new(b"b".to_vec(), 2))
        .expect("revision 1 is a separate case");
    assert_eq!(journal.pending().count(), 2);
}

#[test]
fn sequence_numbers_are_monotonic_and_never_reused() {
    let mut journal = MemoryJournal::default();
    let first = journal.reserve_sequence();
    let second = journal.reserve_sequence();
    assert!(second > first);

    // Surviving a restart must not rewind the counter: a reused sequence number
    // under an existing key is a nonce reuse waiting to happen.
    let restored = MemoryJournal::restored(journal.snapshot());
    assert!(restored.next_sequence() > second);
}

#[test]
fn a_restart_preserves_vote_locks() {
    let mut journal = MemoryJournal::default();
    journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"a".to_vec(), 1))
        .expect("first vote");

    let mut restored = MemoryJournal::restored(journal.snapshot());
    assert!(restored.has_voted(&subject(0), "self"));
    assert_eq!(
        restored
            .commit_vote(&subject(0), "self", OutgoingFrame::new(b"b".to_vec(), 2))
            .unwrap_err(),
        JournalError::AlreadyVoted,
        "a crash must not hand back a second vote"
    );
}

#[test]
fn acknowledging_is_idempotent() {
    // An outbox consumer may be interrupted between sending and acknowledging,
    // so it will re-acknowledge after restart. That must be harmless.
    let mut journal = MemoryJournal::default();
    journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"a".to_vec(), 7))
        .expect("vote");

    assert!(journal.acknowledge(7));
    assert!(
        !journal.acknowledge(7),
        "second acknowledgement changes nothing"
    );
    assert_eq!(journal.pending().count(), 0);
}

#[test]
fn an_unacknowledged_frame_survives_a_restart() {
    let mut journal = MemoryJournal::default();
    journal
        .commit_vote(&subject(0), "self", OutgoingFrame::new(b"a".to_vec(), 7))
        .expect("vote");

    let restored = MemoryJournal::restored(journal.snapshot());
    assert_eq!(
        restored.pending().count(),
        1,
        "store-and-forward must resume"
    );
}

#[test]
fn expired_frames_are_dropped_rather_than_sent_late() {
    let mut journal = MemoryJournal::default();
    let frame = OutgoingFrame::new(b"a".to_vec(), 1).expiring_at(Timestamp::from_secs(2_000));
    journal
        .commit_vote(&subject(0), "self", frame)
        .expect("vote");

    journal.drop_expired(Timestamp::from_secs(2_100));
    assert_eq!(journal.pending().count(), 0);
    assert!(
        journal.has_voted(&subject(0), "self"),
        "dropping an unsent frame must not release the vote lock"
    );
}
