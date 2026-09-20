# Implementation roadmap

This is a milestone roadmap, not authorization to implement all stages. M1 has a task-level executable plan. Before each later milestone, prepare its detailed plan and resolve the listed design gate. Do not improvise the security-sensitive details inside an unrelated task.

## M1 — Pure policy and quorum arithmetic

Deliverable: installable Python package with deterministic count/competence thresholds and tests. Inputs are a validated fixed manifest policy and a set of trusted member IDs. No IO, signatures or transport.

Acceptance: 5/10/20/100-node examples; exact strict competence boundary; duplicates counted once; unknown IDs rejected; cap enforcement; exhaustive small-fleet intersection tests; explicit heavy-node blocking example. Plan: `superpowers/plans/2026-09-18-m1-quorum-policy.md`.

## M2 — Logical contracts and positive-only state machine

Proposed files: `src/lcq/contracts.py`, `src/lcq/clock.py`, `src/lcq/state.py`, `tests/test_contracts.py`, `tests/test_state.py`.

Gate: approve exact subject identity, revision/namespace encoding, timing authority, permitted clock error and behavior for nodes arriving after consultation cutoff. Recommended conservative behavior: a late node forwards valid messages but does not fabricate a completed independent phase.

Deliverable: typed subject, source-reference, model-profile, opinion, consultation request/result, binding support and status contracts; pure transitions accepting an injected clock. Distinguish observation time, protocol creation time and subject validity. Preserve first and second opinions; bind consultation result to a stable request ID.

Acceptance: opinions do not count as binding votes; stage isolation; no cross-version/hash aggregation; cutoff frozen once; one logical consultation; expiry first; conflicts cannot globally veto other subjects; no votes based only on unknown subject IDs. A healthy unanimous trace reaches endorsement.

## M3 — Durable identity, active state and outbox

Proposed files: `src/lcq/storage.py`, `tests/test_storage.py`, `tests/test_crash_recovery.py`.

Gate: CLEARED 2026-09-18. `LogJournal` implements the append-only journal; ordering, partial-record tails and recovery truncation are covered by `tests/log_journal.rs`, including an exhaustive truncation sweep and a SIGKILLed writer. SQLite was rejected for this role -- see `DECISIONS.md` D1. Still open: behaviour when the disk is full, and lock pruning for cases past their validity.

Deliverable: durable mission context, monotonically reserved sequence numbers, binding-vote locks, consultation state, received message identities, bounded replay state, active subjects and outgoing bytes. A transaction must reserve safety state before exposing a packet to transmission. Outbox consumers are idempotent.

Acceptance: crash before/after commit, before/after send and before/after acknowledgement; no nonce reuse or second logical vote; duplicates out of order; expired queued data removed; corrupted/missing safety state fails closed and requests a new key context. Transient failure must not silently reset counters.

## M4 — Cryptography and compact wire codec

Proposed files: `src/lcq/crypto.py`, `src/lcq/codec.py`, `tests/test_crypto.py`, `tests/test_codec.py`, `tests/vectors/README.md`.

Gate: a reviewed byte-level specification with integer widths, byte order, signed transcript/domain separation, full manifest binding, nonce construction, per-sender key separation if used, AEAD associated data, replay processing and error behavior. Include byte budgets for core/support/evidence under each transport. Select a maintained library and pin the tested dependency lockfile. Do not truncate signatures to save airtime.

Deliverable: encode/decode + seal/open using vetted primitives, with authenticated core and separately authenticated bounded supplements. No secret in a packet. A relay can forward original bytes without becoming the signer. Signing context separates stages/missions/protocols.

Acceptance: golden vectors, tamper every field, wrong sender/mission/epoch/profile, malformed lengths, unknown types, oversized frames, wrong signatures despite valid group encryption, replay after restart. A member with the group secret and its own signing key must not forge another member. Declare that full compromise of an honest member defeats its own key protection.

## M5 — Queues, fair scheduling and forwarding

Proposed files: `src/lcq/queue.py`, `src/lcq/transport.py`, `src/lcq/forwarding.py`, `tests/test_queue.py`, `tests/test_forwarding.py`.

Gate: choose bounded queue bytes/items, per-peer quotas, retry limits, hop policy and fairness weights with rationale. Dedup retention must cover the permitted delay/validity model. TTL is not a cryptographic guarantee against malicious relays.

Deliverable: transport interface over complete encoded frames; persisted store-and-forward; expiry, dedup, bounded priorities with progress opportunities for lower classes. Incoming claims of urgency cannot override local caps.

Acceptance: partition then reconnection, burst of high-priority traffic, slow peers, malicious flooding, retransmitted supplement, unknown subject buffering, crash recovery, queue-full behavior. All transport/control traffic must be accounted for; no unlimited hidden side channel.

## M6 — Virtual-time radio laboratory

Proposed files: `src/lcq/sim/clock.py`, `src/lcq/sim/channel.py`, `src/lcq/sim/scenario.py`, `src/lcq/sim/report.py`, `src/lcq/cli.py`, `scenarios/`, `tests/sim/`.

Gate: approve one concrete LoRa PHY profile, airtime calculation and its golden examples, regulatory duty model as a simulation constraint, collision/capture simplifications, directed topology, loss-burst process, clock-error bounds and random seeds. No claims about real maritime range from synthetic path loss.

Deliverable: deterministic event scheduler; N independent state machines using M2–M5; actual crypto; scripted analysis provider; reproducible scenario manifest/report. Radio occupancy is global simulation state, while participant views contain only local inputs and received packets. Avoid introducing Docker and MQTT into the fast event loop.

Acceptance: healthy trace plus N=5/10/20 matrices, one alert/10min and five-alert burst, heavy member outage, at most floor(0.4*N) Byzantine members, partition/reconnect, clock drift and expiry. Report count/weight support, delay, delivered fraction, airtime by packet type, duplicates, queue peaks, false decisions versus ground truth. Exhausted buffers and expired messages are observable outcomes, not silently discarded metrics.

## M7 — Wall-time protocol integration

Proposed files: `src/lcq/runtime.py`, `tests/integration/`, `examples/isolated-lab/`.

Gate: define least-privilege process/container layout and prove the only inter-node data path is the simulated radio. No Docker socket in participants; no shared participant state directories; no host-network bypass. Resource caps include the runtime and observer instrumentation.

Deliverable: multiple real processes running the same state machine with wall clocks and a per-node append-only journal; thin adapter to a channel emulator; restart/kill tests and machine-readable logs. Application MQTT/HTTP integration is exercised by a coordinated, separately authorized milestone in `morsik-lora`, not added as a core dependency here.

Acceptance: actual process crash/restart, unreachable local broker in the integration, repeated deliveries, bounded RSS/CPU/storage, demonstrable network isolation, no unintended IP inter-node path. Faster virtual tests alone do not satisfy this gate.

## M8 — Morsik integration, models, then physical qualification

This milestone spans other repositories and requires separate plans/authorization.

- `morsik-lora`: translate existing Morsik data to stable subjects and provenance; local Mosquitto events and HTTP state; idempotent consultation requests; retain local vs fleet status separation.
- `morsik-analysis`: expose assessment/reassessment contract without replacing deterministic scoring; provide exactly defined model profile metadata.
- Two local lightweight models: measure inference latency separately from radio/protocol delay. Do not use the two-member demo to claim 40% Byzantine availability.
- Physical adapter: choose supported frame size and firmware, then verify actual packet delivery and airtime. Run two radios first, five later. Use only lawful profiles and synthetic/public approved data; no jamming hardware.

## Coverage map

| Design requirement | Milestone |
|---|---|
| Fixed manifest competences/count thresholds | M1, M4 manifest authentication |
| Subject identity, opinions, consultation, two endorsement levels | M2, M8 provenance integration |
| Persistent locks, counters, outbox, restart behavior | M3 |
| Identity, encryption, signatures, replay, frame limits | M3–M4 |
| Fair bounded store-and-forward | M5 |
| Channel fidelity, virtual time, adversarial experiments | M6 |
| Real processes, clocks, isolation and resource measurement | M7 |
| MQTT/HTTP, actual inference and physical radios | M8 in coordinated projects |

Complete M1 does not mean complete protocol. Complete M6 does not mean validated hardware. Complete simulated attack tests do not constitute a security audit or proof.


Radio scheduling: slotted access by manifest index is the default design -- see `DECISIONS.md` D3. Random contention is retained as the measurement baseline and as a fallback where the trigger is not commonly heard.


BLOCKER before any slotted deployment: the slot schedule has no canonical anchor, so a compromised manifest member can equivocate on the trigger and split honest nodes onto colliding schedules. See `DECISIONS.md` D5. Random contention is unaffected and remains the default.
