//! The whole protocol over the simulated radio, not just its signatures.
//!
//! `tests/simulation.rs` exercises a radio-only model that never touches the
//! state machine, the journal, the clock or the group seal. These tests drive
//! the real objects, so they are the ones entitled to say the protocol works.

use lcq::domain::contracts::CONSULTATION_CUTOFF_SECONDS;
use lcq::domain::time::MAX_CLOCK_SKEW_SECONDS;
use lcq::sim::{Deliberation, Topology};

const GUARD_MS: u64 = 200;

fn healthy() -> Deliberation {
    Deliberation::new(10).with_slots(GUARD_MS)
}

#[test]
fn a_healthy_fleet_endorses_through_all_three_stages() {
    let report = healthy().run();
    assert!(report.endorsed);
    assert_eq!(report.binding_supporters, 10);
    for stage in &report.stages {
        assert_eq!(stage.admitted, 10, "every stage must be heard in full");
        assert_eq!(stage.refused, 0);
    }
    assert_eq!(report.refusals.total(), 0);
}

#[test]
fn no_endorsement_can_complete_before_the_consultation_cutoff() {
    // The finding that reframes every latency figure in this crate. A binding
    // vote cannot be admitted until consultation is closed, and consultation
    // cannot close until the clock is CERTAINLY past the cutoff. The radio has
    // no say in this at all.
    let floor = CONSULTATION_CUTOFF_SECONDS + MAX_CLOCK_SKEW_SECONDS;
    for fleet in [5usize, 10, 20] {
        let report = Deliberation::new(fleet).with_slots(GUARD_MS).run();
        assert!(
            report.elapsed_s > floor,
            "{fleet} members finished in {} s, under the {floor} s floor",
            report.elapsed_s
        );
    }
}

#[test]
fn the_radio_is_a_small_part_of_the_elapsed_time() {
    // Which is the honest frame for the scheduling work: slots make the radio
    // stop overrunning the protocol's own deadlines, not make endorsement fast.
    let report = healthy().run();
    assert!(
        report.radio_share() < 0.25,
        "radio was {:.0} % of the elapsed time",
        report.radio_share() * 100.0
    );
}

#[test]
fn the_frame_on_the_air_is_the_sealed_one() {
    // Modelling the signed-but-unsealed frame understates every airtime figure
    // by the seal's overhead and the cleartext nonce header.
    let report = healthy().run();
    assert!(
        report.frame_bytes > 120,
        "a sealed frame with its header should exceed 120 B, got {}",
        report.frame_bytes
    );
}

#[test]
fn a_collided_vote_is_retransmitted_from_the_outbox_not_rebuilt() {
    // Regression. Rebuilding the frame asks the journal for a second vote lock
    // on the same case, which it rightly refuses -- and the member then never
    // retransmits, losing its vote to the first collision without any error
    // being visible. Store and forward means the SAME bytes go out again.
    let report = Deliberation::new(10).run();
    let binding = &report.stages[2];
    assert!(binding.collided > 0, "random contention must collide");
    assert!(
        binding.frames_sent > 10,
        "collided votes must be sent again: {} frames for 10 members",
        binding.frames_sent
    );
    assert_eq!(report.binding_supporters, 10, "and all of them must land");
    assert_eq!(
        report.refusals.already_voted, 0,
        "a retransmission must never look like a second vote"
    );
}

#[test]
fn losses_are_survived_by_retransmission() {
    let report = healthy().with_loss(0.3).with_seed(7).run();
    assert!(report.endorsed);
    assert!(
        report.stages[2].lost > 0,
        "the scenario must actually lose frames"
    );
}

#[test]
fn a_partition_blocks_rather_than_fabricating_a_quorum() {
    let report = healthy().with_topology(Topology::Partitioned).run();
    assert!(!report.endorsed);
    assert!(report.binding_supporters < report.min_signers);
}

#[test]
fn silence_at_the_fault_budget_blocks() {
    let report = healthy().with_silent(4).run();
    assert_eq!(report.min_signers, 8);
    assert_eq!(report.binding_supporters, 6);
    assert!(!report.endorsed);
}

#[test]
fn distance_removes_members_from_the_quorum() {
    let report = healthy().with_spacing_m(5_000.0).with_seed(11).run();
    assert!(!report.endorsed, "the tail is past the radio horizon");
    assert!(report.binding_supporters < report.min_signers);
}

#[test]
fn slots_cut_the_radio_time_but_not_the_elapsed_time() {
    // Both halves matter. The scheduling win is real and it is bounded by the
    // deliberation window, which no amount of radio engineering shortens.
    let random = Deliberation::new(10).run();
    let slotted = healthy().run();

    assert!(
        slotted.radio_ms * 2 < random.radio_ms,
        "slots should at least halve the radio time: {} against {}",
        slotted.radio_ms,
        random.radio_ms
    );
    assert!(
        slotted.elapsed_s * 2 > random.elapsed_s,
        "but the elapsed time is dominated by the cutoff, not the radio"
    );
    assert!(random.endorsed && slotted.endorsed);
}

#[test]
fn slots_also_cut_the_airtime_the_duty_cycle_meters() {
    let random = Deliberation::new(10).run();
    let slotted = healthy().run();
    assert!(
        slotted.airtime_ms < random.airtime_ms,
        "collisions are paid for in airtime: {} against {}",
        slotted.airtime_ms,
        random.airtime_ms
    );
}

#[test]
fn the_deliberation_is_deterministic_for_a_seed() {
    let run = || healthy().with_loss(0.4).with_seed(99).run();
    assert_eq!(run(), run());
}

#[test]
fn a_dispute_is_recorded_but_never_counted() {
    // There is no fleet verdict meaning "no danger". A dispute stops its author
    // voting again and contributes nothing to the threshold, so three disputers
    // out of ten leave seven supporters against a threshold of eight.
    let report = healthy().with_disputers(3).run();
    assert_eq!(report.binding_supporters, 7);
    assert_eq!(report.min_signers, 8);
    assert!(!report.endorsed);
    assert_eq!(
        report.refusals.total(),
        0,
        "a dispute is admitted, not refused"
    );
}

#[test]
fn one_dispute_still_leaves_endorsement_reachable() {
    let report = healthy().with_disputers(1).run();
    assert_eq!(report.binding_supporters, 9);
    assert!(report.endorsed);
}

#[test]
fn a_forged_signature_never_reaches_the_state_machine() {
    // Holding the group key gets a frame decrypted. It does not make its author
    // anybody else, and the quorum counts signatures.
    let report = healthy().with_forgers(2).run();
    assert_eq!(report.binding_supporters, 8);
    assert!(report.stages[2].refused >= 2, "forgeries must be refused");
    assert!(
        report.endorsed,
        "eight honest members still clear the threshold"
    );
}

#[test]
fn enough_forgeries_block_rather_than_fabricate() {
    let report = healthy().with_forgers(4).run();
    assert!(report.binding_supporters < report.min_signers);
    assert!(!report.endorsed);
}

#[test]
fn a_case_that_expires_before_the_vote_admits_nothing() {
    // Expiry is checked before anything else: a case past its validity is not a
    // case, whatever phase it is in. Validity here ends before the consultation
    // cutoff can even be reached.
    let report = healthy().with_validity_s(100).run();
    assert_eq!(report.binding_supporters, 0);
    assert!(!report.endorsed);
    assert!(
        report.refusals.expired > 0,
        "the refusals must say it expired, got {:?}",
        report.refusals
    );
}

#[test]
fn the_receiver_reads_the_stage_from_the_frame_not_from_its_own_phase() {
    // If the receiver assumed the stage, the domain separation that stops an
    // independent opinion being replayed as a binding vote would never be
    // exercised, and a dispute could not be told from support. The healthy run
    // admitting exactly ten per stage with no refusals is that check passing.
    let report = healthy().run();
    for stage in &report.stages {
        assert_eq!(stage.admitted, 10);
    }
    assert_eq!(report.refusals.wrong_stage_for_phase, 0);
}
