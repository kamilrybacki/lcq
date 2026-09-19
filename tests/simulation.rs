//! End-to-end: does a fleet actually reach endorsement over a lossy radio?

use lcq::sim::{Scenario, Topology};

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

#[test]
fn an_unprompted_simultaneous_reply_loses_frames_to_collisions() {
    // Every member answers the same trigger on one channel. Without a spread of
    // start times they would talk over each other completely; with one, some
    // still overlap. The cost is visible as extra frames, and a run that sent
    // exactly one frame per member would mean the channel was not modelled.
    let report = Scenario::new(10).run();
    assert!(report.collided_frames > 0, "a contended round must collide");
    assert!(
        report.frames_sent > 10,
        "collided frames must be retransmitted, got {}",
        report.frames_sent
    );
}

#[test]
fn backoff_converges_despite_the_first_round_colliding() {
    // The point of doubling the window: a collided round must not reproduce its
    // own pile-up. Endorsement still lands.
    let report = Scenario::new(10).run();
    assert!(report.endorsed);
    assert!(report.binding_supporters >= report.min_signers);
}

#[test]
fn the_hourly_airtime_budget_can_block_a_quorum() {
    // 34 s of the 36 s allowance already spent on other traffic. The radio is
    // fine and the fleet is honest; the regulation is what stops the vote.
    // Blocking is the correct outcome — the alternative is transmitting
    // unlawfully or counting members who never spoke.
    let report = Scenario::new(10).with_prior_airtime_ms(34_000).run();
    assert!(report.duty_cycle_blocked > 0, "the budget must bite");
    assert!(report.binding_supporters < report.min_signers);
    assert!(!report.endorsed);
}

#[test]
fn a_fleet_with_budget_to_spare_is_not_blocked_by_duty_cycle() {
    let report = Scenario::new(10).run();
    assert_eq!(report.duty_cycle_blocked, 0);
}

#[test]
fn a_close_convoy_is_fully_audible() {
    let report = Scenario::new(10).with_spacing_m(500.0).with_seed(11).run();
    assert_eq!(report.too_weak_frames, 0, "500 m is a comfortable link");
    assert!(report.endorsed);
}

#[test]
fn distance_drops_members_out_of_the_quorum() {
    // The same fleet strung out far enough that its tail is past the radio
    // horizon. Those members transmit, pay the airtime and are never heard.
    let report = Scenario::new(10)
        .with_spacing_m(5_000.0)
        .with_seed(11)
        .run();
    assert!(report.too_weak_frames > 0, "the tail must be inaudible");
    assert!(
        report.binding_supporters < report.min_signers,
        "got {} supporters against a threshold of {}",
        report.binding_supporters,
        report.min_signers
    );
    assert!(!report.endorsed);
    assert!(
        report.airtime_ms > 0,
        "unheard members still spend their airtime; they get no acknowledgement"
    );
}

#[test]
fn geometry_lets_a_near_member_capture_over_a_distant_one() {
    use lcq::sim::{Link, Reception, TX_POWER_DBM, Transmission, receive, rssi_dbm};

    let near = Transmission::new(0, 1_500, rssi_dbm(&Link::new(1_000.0), TX_POWER_DBM));
    let far = Transmission::new(700, 1_500, rssi_dbm(&Link::new(15_000.0), TX_POWER_DBM));
    assert_eq!(
        receive(&near, &[far]),
        Reception::Decoded,
        "the near station is far more than the capture margin ahead"
    );
    assert_eq!(
        receive(&far, &[near]),
        Reception::NoLock,
        "the near frame was on the air over the far one's preamble"
    );
}

#[test]
fn the_physical_effects_are_deterministic_for_a_seed() {
    let run = || {
        Scenario::new(10)
            .with_loss(0.4)
            .with_spacing_m(3_000.0)
            .with_seed(99)
            .run()
    };
    assert_eq!(run(), run());
}

#[test]
fn capture_saves_frames_that_uniform_signal_strength_would_lose() {
    // The same fleet, the same seed, the same start times — the only difference
    // is that geometry gives the nodes different received strengths, so a near
    // member can be demodulated through a distant one's transmission. Without
    // the spread nothing can ever capture and every overlap destroys both.
    let flat = Scenario::new(10).with_seed(11).run();
    let spread = Scenario::new(10)
        .with_spacing_m(2_000.0)
        .with_seed(11)
        .run();
    assert_eq!(spread.too_weak_frames, 0, "2 km links are comfortable");
    assert!(
        spread.collided_frames < flat.collided_frames,
        "capture must save at least one frame: {} with geometry against {} without",
        spread.collided_frames,
        flat.collided_frames
    );
    assert!(spread.endorsed && flat.endorsed);
}
