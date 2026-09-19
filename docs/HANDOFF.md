# Handoff — Claude Code / next implementer

## State at 2026-09-18 (M1-M6 implemented, awaiting user review)

### The protocol runs end to end

`cargo run --example fleet` puts a fleet through endorsement over a lossy
channel. Verified signatures, group encryption, bounded queues, store-and-forward
retries and virtual time, all together:

| scenario | supporters | threshold | frames | rejected | endorsed |
|---|---|---|---|---|---|
| 5 nodes, no loss | 5 | 4 | 5 | 0 | yes |
| 10 nodes, 30 % loss | 10 | 8 | 14 | 0 | yes |
| 10 nodes, 60 % loss | 9 | 8 | 24 | 0 | yes |
| 10 nodes, partitioned | 5 | 8 | 30 | 0 | **no** |
| 10 nodes, 4 silent (the budget) | 6 | 8 | 6 | 0 | **no** |
| 5 nodes, 2 forgers | 3 | 4 | 5 | **2** | **no** |
| 20 nodes, 30 % loss | 20 | 15 | 29 | 0 | yes |

The three "no" rows are the protocol behaving correctly. Blocking is an
acceptable outcome; fabricating a quorum is not.

### Two findings the simulation produced, not assumptions

**Silence at the fault budget blocks liveness.** With N=10 the count threshold
is 8, so four silent members put endorsement permanently out of reach. The spec
warns of exactly this; a first version of the test asserted the opposite and was
wrong.

**A partition must be judged from one seat.** An early model counted a frame as
verified if *anyone* heard it, which made a partitioned fleet look unanimous to
nobody in particular — a fabricated quorum. Endorsement is now evaluated from a
single observing node.

### M5 and M6

M5 gate choices, all assumptions rather than approvals: queue caps are explicit
item and byte budgets, distress pre-empts routine but only in bursts of 8 so the
low class cannot starve, dedup memory is bounded at 1024 entries, and a peer's
claim of urgency never raises a local cap. M6 models loss as an independent
per-link probability with a seeded generator, and computes airtime from the
Semtech formula for SF10/125 kHz/CR 4/5.

**What M6 is not:** no collisions, no capture effect, no path loss, no fading,
no duty-cycle enforcement. Nothing here supports any claim about real maritime
range.

### The wire constraint that needs a decision

A core frame seals to 128 B in compact form and fits SF10, but **no form fits
SF12**. Its 51-byte payload cannot hold a 64-byte ed25519 signature, and the
design forbids truncating signatures for airtime. Maximum range therefore needs
a different signature scheme or an explicit bounded fragmentation design. That
is a product decision, not something to resolve quietly in code.

### Still absent

Real radio hardware, multi-process operation with wall clocks (the spreading-factor question is settled -- see `DECISIONS.md` D2), real radio hardware,
model-quality coefficients, consultation supplements and evidence provenance.

### Earlier milestones

### M2 — logical contracts and positive-only state machine

Implemented. The gate required approving five things; the design spec settles
four of them, and the fifth was chosen and is flagged as an assumption:

| gate item | resolution | source |
|---|---|---|
| subject identity | mission + event + revision + content hash | spec §7, plus subtlety 4: the upstream `Event.id` is a *local* identity |
| revision / namespace | revision scoped to the subject; no cross-revision or cross-hash aggregation | spec §6 |
| timing authority | every deadline derives from the shared `evaluation_started_at` carried with the subject, never from local arrival | spec §6 |
| clock error | **±30 s — assumption, not user-approved.** A tenth of the 5-minute cutoff, enforced by a compile-time assertion | spec says "bounded" without a number |
| late node | records nothing into a frozen consultation; never fabricates a completed independent phase | roadmap recommendation + spec §6 |

Files: `src/domain/time/`, `src/domain/contracts/`, `src/domain/state/`, and
`tests/time.rs`, `tests/contracts.rs`, `tests/state.rs`.

The roadmap's acceptance list is encoded directly as tests: opinions are not
votes, stages do not aggregate, the cutoff freezes exactly once, expiry is
checked before everything else, a conflict on one subject cannot veto another,
a node votes at most once, and a healthy unanimous trace reaches endorsement.

Two design points worth re-reading before M3:

1. **Uncertainty is a third answer.** `Clock` reports certainly-before,
   certainly-after, or neither. A node inside the skew band refuses a binding
   vote rather than guessing — and refusing to vote is not deciding there is no
   danger.
2. **A disputing binding vote is recorded but never counted.** It consumes the
   author's one vote so they cannot vote again, and contributes nothing to any
   threshold.

Still absent, unchanged: authentication, wire format, persistence, radio.

### M1 — quorum policy

M1 is implemented and verified. Two things in the original plan were changed by
the user, both engineering proposals rather than product decisions:

- **Language: Rust, not Python.** Chosen for the actual constraints rather than
  benchmark speed: a LoRa payload is 51-222 bytes so wire compactness decides
  whether a message fits at all; a GC pause inside a transmit window costs a
  frame; and `no_std` keeps the door open if a node moves from a Pi to a
  microcontroller. Toolchain: rustc/cargo 1.97, edition 2024, `proptest` as the
  only dev dependency, no runtime dependencies.
- **Layout: DDD.** `src/domain/quorum/` holds the rules; the domain layer may
  not import a transport, a store or a clock. Later milestones add application
  and infrastructure layers beside it rather than inside it.

All product decisions in this document are unchanged.

### Verified, with the commands actually run

| command | result |
|---|---|
| `cargo test` | 88 passed across 9 suites |
| `cargo clippy --all-targets -- -D warnings` | clean at `pedantic` |
| `cargo fmt --check` | clean |
| `cargo build --release` | builds |

Two real defects were caught by `clippy::pedantic` and fixed, not silenced:

1. `min_signers` computed `(size + max_faulty) / 2 + 1`, which can overflow.
   Overflow in a safety threshold would silently produce a bar far below the one
   the fleet agreed to. Now `usize::midpoint`.
2. Manifest validation and `max_faulty` could panic via `expect`. A panic in a
   node at sea is not an acceptable failure mode; both are now total.

The property-test oracle deliberately keeps the longhand arithmetic and carries
an `allow` for the same lint: an oracle that borrows the implementation's helper
inherits the implementation's bug instead of catching it.

### What M1 still is not

Unchanged from the plan, and worth repeating because the code now looks
finished: no authentication, no wire format, no persistence, no radio, no
consultation, no model weights. `evaluate` takes signer IDs entirely on trust.
Passing tests are an arithmetic oracle, not a Byzantine safety proof.

### Next gate

User review of the SF12 constraint and of the M5/M6 assumptions listed above,
then multi-process operation with wall clocks, or the model-integration stage. The append-only journal adapter is done (`DECISIONS.md` D1).

## Original state at 2026-09-18

- GitHub: https://github.com/kamilrybacki/lcq.git
- Local checkout used to prepare this handoff: `/home/kamil-rybacki/Code/lcq`.
- Separate integration repository: https://github.com/kamilrybacki/morsik-lora.git
- Original Morsik code inspected read-only: `/home/kamil-rybacki/Code/Baltic_Hackaton_26`.
- Product design and acceptance criteria were discussed and approved. User requested specifications and plans usable by Claude Code.
- Implementation has NOT started. No package, test suite, radio, model, broker or container has been launched for `lcq`.
- Documents contain all relevant product decisions. No need to recover the original chat.
- A written-spec review is still required for proposed technical choices, especially cryptography, exact wire layout, clock error budget and radio profiles.

## Next bounded action

Read `AGENTS.md`, the design spec, and the roadmap. Present the M1 scope to the user and, when asked to implement, execute:

`docs/superpowers/plans/2026-09-18-m1-quorum-policy.md`

M1 creates only a pure, tested quorum-policy calculator. It does not authenticate received messages and must not be presented as a secure protocol. This small boundary lets the user review correctness before persistence, crypto and network behavior are introduced.

## Defaults already agreed with the user

- Known membership, no runtime join; keys provisioned before a mission.
- No central runtime server, only local application infrastructure per node.
- Positive endorsements only; no quorum output meaning “safe/no danger”.
- Independent opinion → one consultation → optional binding support vote.
- Up to 40% compromised members in adversarial tests; blocking is acceptable.
- `f=floor(0.4*N)`, `q_count=floor((N+f)/2)+1`, plus STRICTLY more than 2/3 total manifest weight from the same signers.
- Raw model weight proportional to square root of parameter count, adjusted by tunable quantization coefficient; final max:min ≤3:1.
- Two levels: fleet endorsement; additionally at least two independent originating sources with adequate provenance.
- 5-minute consultation cutoff, 10-minute target, source expiry or synthetic default 30 minutes.
- One alert per 10 minutes plus a burst of five; N=5,10,20 initially.
- Store-and-forward, bounded fair priority scheduling, persistent active state/outbox.
- Complete core message plus bounded authenticated evidence supplements; no arbitrary unbounded fragmentation.
- Virtual-time simulator followed by wall-time integration; interchangeable transport.
- Group encryption plus individual signatures; no forward-secrecy guarantee in v1.
- Local Mosquitto + HTTP in the application integration; all LLM inference in `morsik-analysis`.

## Important subtleties

1. A 3:1 weight cap does not prevent a single heavy member from blocking the weight threshold. Report this explicitly.
2. The 40% figure does not promise liveness. Count-quorum intersection is not a whole BFT proof.
3. Real source independence cannot be established from a compromised member's signed assertion alone. Simulator ground truth must stay outside node-visible state.
4. `Event.id` and `Cluster.id` in existing Morsik are local identities, not fleet-wide event identities. The integration requires a stable upstream subject contract.
5. Hashing exact source text catches byte duplicates, not translations, paraphrases or shared origin.
6. Positive-only endorsements simplify finalization, but replay, revision scoping and durable vote locks still matter.
7. In M1 signers are trusted test inputs. Only a later authenticated, validated message pipeline may supply them in production.

## Verification at handoff

Only documentation checks and arithmetic checks are applicable at this stage. Preparation checks: 7 Markdown files with balanced code fences; 4 relative Markdown links resolved; count-threshold inequalities checked for N=1 through 100; Python examples in the M1 plan syntax-checked with `compile` (not executed as tests). No application tests have run. Do not infer successful application tests from the presence of test examples in a plan. Future implementers must run their own commands.

## Update template for the next session

Record milestone, approval scope, branch/commit, changed files, exact verification commands and outputs, known failures, unreviewed assumptions, and next action. Never record secrets. Do not mark manual acceptance yourself.


---

## State at the end of 2026-09-18

The crate is now `lcq` (LCQ Protocol, LoRa-based Confidence Quorum). 183 tests,
clippy pedantic clean. Fifteen commits ahead of `origin/main` and **not pushed**;
the GitHub remote is still named `lorai`.

### Done since the last handoff entry

* **M3 durable journal** — `LogJournal`, append-only, `DECISIONS.md` D1.
  Recovery tested at every byte offset a crash could land on, plus a SIGKILLed
  writer.
* **Radio model** — collisions, capture, two-ray path loss, Rician fading, duty
  cycle. `DECISIONS.md` D2 settles the spreading factor on measurement.
* **Slotted access** — D3, and D6 settles its anchor.
* **Faithful simulator** — `sim::Deliberation` drives the real state machine,
  journal, clock and group seal. `sim::Scenario` remains, and is **one round of
  one stage**; anything it reports is about the channel, not the protocol.

### Two defects fixed, both found by building the faithful simulator

* Cross-member nonce collision under the shared group key (critical).
* Frames that could not be opened, because the nonce lived inside the ciphertext.

### M7 done: the protocol across real processes

`lcq-node` and `lcq-hub` are real binaries; `tests/multiprocess.rs` runs five
node processes against a channel emulator, kills one and restarts it. See
`DECISIONS.md` D7 for the four findings that only separate processes could
produce, including the bound on how far the clock may be scaled.

### Reliability hardening done (D9)

Trigger admission is the full path, every frame is checked against the subject,
timing is on the anchor with the subject's deadlines on the local clock, any
member may open, splits are detected at a threshold of two members and answered
with randomised retries, late joiners derive the anchor. All measured in
containers: skew, dead opener, isolated halves, 30 % loss, twelve vessels.

### Adversaries and repair (D10)

Six adversary modes on `lcq-node`, one container test each. Receiver-driven
repair rounds after the scheduled vote, because per-link loss defeats a
"somebody heard me" stop rule. Loss test measured five of five after; it was
four of five before.

### What to pick up next

1. **Equivocation on the trigger** (D6) — the one open blocker. Slots must
   anchor on the trigger, so this cannot be dodged. Recommended treatment is
   written down: round identity from the trigger hash, hearing two as evidence,
   fall back to random contention on detection.
2. **Acknowledgement on the air** (D4) — worth roughly 4.5x the airtime. Nothing
   carries the fact today; `Journal::acknowledge` exists but no frame says it.
3. **Short case reference** (D4) — 23 % off every frame, no security traded.
4. Still unexercised: `RadioQueue` (priority, dedup, distress burst). The
   multi-process run does drive every member's own `Case`, so that gap is
   closed; what remains is the queue and any retry policy, which waits on the
   acknowledgement design.
5. From review: per-sender replay windows with retention limits, cheap rejection
   of senders outside the manifest.
