//! Per-sender replay windows: seen once is admitted, seen twice is a replay,
//! older than the window is a replay, and a late genuine frame inside it is
//! not.

use lcq::application::{REPLAY_WINDOW, ReplayWindow};

#[test]
fn a_fresh_window_has_seen_nothing() {
    let window = ReplayWindow::new();
    assert!(!window.seen(0));
    assert!(!window.seen(u64::MAX));
    assert_eq!(window.highest(), None);
}

#[test]
fn a_marked_sequence_is_a_replay_and_its_neighbours_are_not() {
    let mut window = ReplayWindow::new();
    window.mark(5);
    assert!(window.seen(5));
    assert!(
        !window.seen(4),
        "older but never seen: a late frame, not a replay"
    );
    assert!(!window.seen(6), "newer: not yet seen");
    assert_eq!(window.highest(), Some(5));
}

#[test]
fn an_older_frame_inside_the_window_is_admitted_exactly_once() {
    let mut window = ReplayWindow::new();
    window.mark(100);
    let oldest_inside = 100 - u64::from(REPLAY_WINDOW) + 1;
    assert!(!window.seen(oldest_inside));
    window.mark(oldest_inside);
    assert!(window.seen(oldest_inside));
    assert_eq!(
        window.highest(),
        Some(100),
        "marking an older one does not move the top"
    );
}

#[test]
fn anything_older_than_the_window_is_a_replay() {
    let mut window = ReplayWindow::new();
    window.mark(100);
    let just_outside = 100 - u64::from(REPLAY_WINDOW);
    assert!(window.seen(just_outside));
    assert!(window.seen(0));
}

#[test]
fn advancing_the_top_slides_the_window() {
    let mut window = ReplayWindow::new();
    window.mark(10);
    window.mark(12);
    assert!(window.seen(10));
    assert!(window.seen(12));
    assert!(!window.seen(11));
    // A leap past the whole window forgets everything behind it.
    window.mark(12 + u64::from(REPLAY_WINDOW) + 5);
    assert!(window.seen(12), "too old now");
    assert!(window.seen(10));
    assert!(!window.seen(12 + u64::from(REPLAY_WINDOW)));
}

#[test]
fn a_leap_of_exactly_the_window_keeps_nothing_but_still_answers() {
    let mut window = ReplayWindow::new();
    window.mark(0);
    window.mark(u64::from(REPLAY_WINDOW));
    assert!(
        window.seen(0),
        "exactly a window back: out of memory, so a replay"
    );
    assert!(!window.seen(1));
    assert!(window.seen(u64::from(REPLAY_WINDOW)));
}

#[test]
fn sequence_zero_is_a_sequence_like_any_other() {
    let mut window = ReplayWindow::new();
    window.mark(0);
    assert!(window.seen(0));
    assert!(!window.seen(1));
    window.mark(1);
    assert!(window.seen(0));
    assert!(window.seen(1));
}

#[test]
fn a_window_resumed_from_a_journal_refuses_everything_it_covers() {
    // A restart reads back one number: the highest sequence this sender
    // reached. Marking that number would set one bit and leave the sixty-three
    // below it looking new, and a member's stage frames are consecutive
    // sequences -- so a restart would reopen the window on exactly the frames
    // most likely to be replayed.
    let resumed = ReplayWindow::resumed(100);
    assert_eq!(resumed.highest(), Some(100));
    for sequence in 37..=100u64 {
        assert!(
            resumed.seen(sequence),
            "sequence {sequence} read as new after a restart"
        );
    }
    assert!(resumed.seen(0), "and anything older still reads as seen");
    assert!(!resumed.seen(101), "while the future is still open");

    // What `mark` does instead, which is what this replaces.
    let mut marked = ReplayWindow::new();
    marked.mark(100);
    assert!(
        !marked.seen(99),
        "this is the hole the resumed window closes"
    );
}

#[test]
fn a_resumed_window_still_advances() {
    let mut window = ReplayWindow::resumed(100);
    assert!(!window.seen(101));
    window.mark(101);
    assert!(window.seen(101));
    assert!(window.seen(100), "and does not forget what it resumed with");
}
