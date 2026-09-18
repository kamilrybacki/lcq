//! End-to-end: does a fleet actually reach endorsement over a lossy radio?

use lorai::sim::{Scenario, Topology};

#[test]
fn a_healthy_five_node_fleet_reaches_endorsement() {
    let report = Scenario::new(5)
        .with_loss(0.0)
        .with_topology(Topology::FullyConnected)
        .run();
    assert!(report.endorsed, "unanimous honest fleet must endorse");
    assert!(report.binding_supporters >= report.min_signers);
}

#[test]
fn moderate_loss_still_endorses_because_frames_are_reforwarded() {
    let report = Scenario::new(10).with_loss(0.3).with_seed(7).run();
    assert!(report.endorsed, "store-and-forward must survive 30% loss");
}

#[test]
fn a_partition_blocks_endorsement_rather_than_faking_it() {
    // Blocking is acceptable; inventing a quorum is not.
    let report = Scenario::new(10).with_topology(Topology::Partitioned).run();
    assert!(!report.endorsed);
    assert!(
        report.binding_supporters < report.min_signers,
        "a partitioned fleet must not reach the count threshold"
    );
}

#[test]
fn silence_at_the_fault_budget_blocks_rather_than_endorsing() {
    // The design is explicit that the 40% budget buys safety, not liveness, and
    // the simulation shows exactly that: with N=10 the count threshold is 8, so
    // four silent members put endorsement out of reach. Blocking is the correct
    // outcome; a fleet that endorsed here would have counted absent members.
    let report = Scenario::new(10).with_silent(4).run();
    assert_eq!(report.min_signers, 8);
    assert_eq!(report.binding_supporters, 6);
    assert!(!report.endorsed);
}

#[test]
fn one_silent_member_still_leaves_endorsement_reachable() {
    let report = Scenario::new(10).with_silent(1).run();
    assert!(report.binding_supporters >= report.min_signers);
    assert!(report.endorsed);
}

#[test]
fn a_forged_signature_never_counts() {
    // A member holding the group key but signing as somebody else contributes
    // nothing: the signature is what a quorum counts.
    let report = Scenario::new(5).with_forgers(2).run();
    assert!(report.rejected_frames >= 2, "forgeries must be detected");
    assert_eq!(
        report.binding_supporters, 3,
        "only genuine signatures count"
    );
}

#[test]
fn the_simulation_is_deterministic_for_a_seed() {
    let a = Scenario::new(10).with_loss(0.4).with_seed(99).run();
    let b = Scenario::new(10).with_loss(0.4).with_seed(99).run();
    assert_eq!(a.endorsed, b.endorsed);
    assert_eq!(a.binding_supporters, b.binding_supporters);
    assert_eq!(a.frames_sent, b.frames_sent);
}

#[test]
fn airtime_is_accounted_for_every_frame() {
    // A protocol that ignores airtime is a protocol that ignores duty cycle.
    let report = Scenario::new(5).run();
    assert!(report.airtime_ms > 0);
    assert!(report.frames_sent > 0);
}
