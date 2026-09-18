//! Bounded queues, fairness, and what a hostile peer cannot take from us.

use lcq::application::{OutgoingFrame, Priority, QueueError, RadioQueue};
use lcq::domain::time::Timestamp;

fn frame(seq: u64, bytes: usize) -> OutgoingFrame {
    OutgoingFrame::new(vec![0u8; bytes], seq)
}

#[test]
fn a_full_queue_refuses_rather_than_growing() {
    // Memory on a node is finite and the radio is slow. Unbounded queues turn a
    // burst into an out-of-memory death rather than dropped traffic.
    let mut queue = RadioQueue::with_capacity(2, 10_000);
    queue.offer(frame(1, 10), Priority::Routine).expect("room");
    queue.offer(frame(2, 10), Priority::Routine).expect("room");
    assert_eq!(
        queue.offer(frame(3, 10), Priority::Routine).unwrap_err(),
        QueueError::Full
    );
}

#[test]
fn a_byte_budget_is_enforced_as_well_as_an_item_count() {
    let mut queue = RadioQueue::with_capacity(100, 250);
    queue.offer(frame(1, 200), Priority::Routine).expect("room");
    assert_eq!(
        queue.offer(frame(2, 200), Priority::Routine).unwrap_err(),
        QueueError::Full
    );
}

#[test]
fn distress_is_served_before_routine() {
    let mut queue = RadioQueue::with_capacity(10, 10_000);
    queue.offer(frame(1, 10), Priority::Routine).expect("room");
    queue.offer(frame(2, 10), Priority::Distress).expect("room");
    assert_eq!(queue.take_next().expect("something to send").sequence(), 2);
}

#[test]
fn routine_traffic_still_makes_progress_under_sustained_distress() {
    // Strict priority starves the low class forever, and a node that never
    // sends its routine traffic is a node that silently stops participating.
    let mut queue = RadioQueue::with_capacity(100, 100_000);
    for seq in 0..20 {
        queue
            .offer(frame(seq, 10), Priority::Distress)
            .expect("room");
    }
    queue
        .offer(frame(999, 10), Priority::Routine)
        .expect("room");

    let mut served_routine = false;
    for _ in 0..12 {
        if queue.take_next().is_some_and(|f| f.sequence() == 999) {
            served_routine = true;
            break;
        }
    }
    assert!(served_routine, "routine traffic was starved");
}

#[test]
fn a_peers_claim_of_urgency_cannot_exceed_our_own_caps() {
    // Priority is assigned locally. A neighbour flooding frames marked distress
    // must not be able to consume more of our queue than we allotted.
    let mut queue = RadioQueue::with_capacity(4, 10_000);
    for seq in 0..4 {
        queue
            .offer(frame(seq, 10), Priority::Distress)
            .expect("room");
    }
    assert_eq!(
        queue.offer(frame(99, 10), Priority::Distress).unwrap_err(),
        QueueError::Full,
        "an urgency claim does not raise the cap"
    );
}

#[test]
fn a_duplicate_frame_is_not_queued_twice() {
    // Store-and-forward means the same frame arrives from several neighbours.
    let mut queue = RadioQueue::with_capacity(10, 10_000);
    queue.offer(frame(7, 10), Priority::Routine).expect("room");
    assert_eq!(
        queue.offer(frame(7, 10), Priority::Routine).unwrap_err(),
        QueueError::Duplicate
    );
    assert_eq!(queue.len(), 1);
}

#[test]
fn expired_frames_are_dropped_rather_than_sent_late() {
    let mut queue = RadioQueue::with_capacity(10, 10_000);
    queue
        .offer(
            frame(1, 10).expiring_at(Timestamp::from_secs(100)),
            Priority::Routine,
        )
        .expect("room");
    queue.drop_expired(Timestamp::from_secs(200));
    assert_eq!(queue.len(), 0);
}

#[test]
fn dedup_memory_is_bounded_too() {
    // An unbounded dedup set is just a slower memory leak.
    let mut queue = RadioQueue::with_capacity(4, 10_000);
    for seq in 0..1_000 {
        let _ = queue.offer(frame(seq, 10), Priority::Routine);
        let _ = queue.take_next();
    }
    assert!(queue.seen_count() <= RadioQueue::MAX_DEDUP_ENTRIES);
}

#[test]
fn draining_an_empty_queue_yields_nothing() {
    let mut queue = RadioQueue::with_capacity(4, 10_000);
    assert!(queue.take_next().is_none());
}
