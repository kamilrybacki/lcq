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

### Killed inside the round (D11)

Witnessed binding votes are journalled as frames and re-verified on restart. A
vessel killed after voting and refloated into the live round finishes it, five
of five. Four defects fixed on the way, including the emulator refusing
reconnections -- which the earlier refloat test never noticed.

### Airtime, model tests, two adversaries, geometry (D12-D14)

The node meters its own airtime. The state machine agrees with a reference
model over 512 random histories. Two faulty members of five -- the whole budget
-- leave safety intact. The emulator decides reception per receiver from the
propagation model, so a line of vessels has real geometry. Flake rate measured:
54 of 54 container tests in three isolated runs.

### Relaying (D15)

On a repair request the middle of a line carries each end's vote to the other,
unchanged, in its own slot. Geometry test tally went from [4, 5, 5, 5, 4] to
[5, 5, 5, 5, 5]. This is the range-by-relaying D2 chose over SF12, working.

### Radio seam and virtual SX1262 (D16)

The node talks to a radio through one seam, `lcq::application::Radio`:
`transmit(bytes)`, and `poll()` for received frames with RSSI and SNR, CRC
failures and adapter diagnostics. `HubRadio` is the emulator socket as before.
`Sx126xRadio` is a virtual SX1262: a behavioural model of the chip with a clock
(`infrastructure::sx126x`), driven by the *unmodified* `lora-phy` `Sx126x`
driver over a virtual SPI bus and virtual BUSY/DIO1/reset lines, attached to the
same hub. The hub now hands each receiver a `Delivery` -- RSSI, SNR, airtime,
CRC flag -- and a collided frame arrives as a CRC failure with garbled bytes
rather than as silence. `lcq-node --radio sx1262` selects the virtual chip;
`LCQ_RADIO=sx1262` points the whole container suite at it.

Measured: the baseline fleet endorses 5/5 on both radios, and the virtual chips
missed no frames -- the slot schedule keeps members out of each other's
airtime, so half-duplex deafness had nothing to bite. Nine bench tests
(`tests/sx126x.rs`) drive the real driver against the chip model with channels
for a medium: bytes and airtime of a transmission, reception with the signal
report decoded as the driver decodes it, deaf in standby and during TX, a late
listener misses, a collided frame raises `RxDone` with `CrcErr`, sleep and wake.
Full container suite on the virtual chip: 20 of 20 tests in 638 s, no container left behind, geometry tally still [5, 5, 5, 5, 5] with relaying through the virtual chips.

One thing the driver does that is worth knowing: `lora-phy` hands a CRC-failed
frame up as a reception -- the payload is whatever arrived. The protocol's seal
is what rejects it. Pinned by a test so a change upstream is noticed.

Found on the way: the replay-adversary container test compared drops with
replays *sent*, and on that channel most replays collide and reach nobody, so
the assertion sat at its own expected value (26 replays, 27 drops in one run,
24 in the next). The emulator now names the author and sequence of every frame
it delivers, and the test compares drops with replays *delivered* -- the
property it always meant: no receiver verifies a frame it already had.

### The medium judged in time, and what the meters caught (D17–D19)

Replay windows per sender, a frozen and versioned PHY profile with contract
vectors, meters in the node's final report, and the medium judged in time:
`sim::medium::judge` tells no-lock from header loss from payload loss, the
hub delivers accordingly (a header error as `HeaderErr` with no bytes, a
payload error as garbled bytes with `CrcErr`, no lock as silence), and the
virtual chip raises exactly those IRQs. GitHub Actions runs fmt, clippy and
every non-container test on every push.

The meters paid for themselves at once. The first full run on the virtual
chips failed the replay-adversary test: the replayer itself ended with three
supporters of four. Its `chip_missed` counter said why -- deaf while
transmitting its replays, seven frames -- and its honest neighbours' repair
resends should have covered that but did not: own-vote resends had moved
onto `RadioQueue` in the distress class, and the queue's dedup memory refused
the same frame in the next repair round. `RadioQueue::forget` on every
repair request fixed it (a request says the frame is still lacking), and the
test passes deterministically again: 26 replays, 36 delivered, 49 dropped,
every member at quorum. Gate on both radios: hub 20 of 20 in 640 s, virtual SX1262 20 of 20 in 631 s.

### A frame names its case by reference (D20)

The 32-byte content hash left the wire: a `CompactEnvelope` carries an
eight-byte `case_reference` of it, and the signature transcript covers the
full hash the receiver holds, so a frame about another case fails the
signature check rather than being counted. `MAX_FRAME_BYTES` is 152 (from
176); at SF10 that is roughly 200 ms off every frame and the slot shrinks
with it. Every signing and verifying site takes the hash explicitly; the
simulations and examples included.

Found on the way, by the gate on the virtual chip: an honest fleet at 30 %
loss declared a split. A member short of two votes got both carried by
neighbours in *their* slots, and the timing-based split check read the two
off-slot frames as two foreign anchors. The check now runs only while the
schedule is running; repair rounds are where frames are expected off their
author's slot. Gate on both radios: hub 20 of 20 in 745 s, virtual SX1262 20 of 20 in 631 s.

### Two ways onto hardware (D21)

`--radio rnode:<port>` drives an RNode -- any board with RNode firmware -- over
its KISS host protocol, implemented sans I/O in `infrastructure::rnode` and
pinned against a fake RNode on a pseudo-terminal pair (`tests/rnode.rs`).
`--radio spi:<spidev>,<gpiochip>,busy=,dio1=,reset=[,tcxo=]` runs the same
`lora-phy` driver thread as the virtual chip on `spidev` and the GPIO
character device, with the air for a medium; the driver thread is generic
over the bus (`Watch`, `DriverHandle`), so only the bus changed. Neither has
met a board. What the first board has to answer is in the pick-up list. Gate:
every non-container test, the hub baseline, and the full suite on the virtual
SX1262 (20 of 20, 631 s) with the generic driver and the static musl build
carrying the hardware crates. Upstream: https://github.com/lora-rs/lora-rs/pull/487.

### Security audit and threat model (D22)

`THREAT-MODEL.md`: assets, trust boundaries, six adversary classes, STRIDE per
component, twenty findings with severity and status, and Hermes's
independent static review merged. Fixed on the spot: a journal that feeds
nonces starts at a random sequence and never from zero (`open_with_entropy`);
replay windows are seeded from the journal at start; adversary modes live
behind the `harness` feature and a production build refuses the flag; the
fleet size is capped at the acknowledgement bitmap; a sequence past the round
label is refused, not truncated; the journal is created 0600; CI actions are
pinned by commit; RNode firmware versions are logged. Both reviews agree on
the verdict: the wire crypto and crash ordering are sound, and the binary is
an integration harness until provisioning, a signed manifest and epoch
lifecycle, rollback-resistant nonce state and persisted replay state exist.

### What to pick up next

1. **Provisioning and key lifecycle** (THREAT-MODEL F4, F5, F18) — before any
   vessel: signed manifest with digest, epoch and validity; per-member keys
   on the device; group key per epoch from Vault/sops through Morsik; fail
   closed on fixture keys and on a missing or restored journal. This is the
   product's decision to make and the protocol's to enforce.
2. **Hardware qualification** (roadmap M8): both adapters exist (D21) and
   have never seen a board. Two SX1262 boards first -- RNode firmware over USB
   needs no firmware work -- then five. First measurements, wired through
   attenuators: bring-up on both adapters, `SetTx` to `TxDone`, `SetRx` to the
   first catchable preamble, the late-listener and capture thresholds the
   model guesses at, CAD latency, sleep/wake.
3. **Chip model fidelity** — what is left of Hermes's review of D16 after
   D19: the medium's three outcomes, time-aware capture, the CAD state
   machine, the derived late-listener threshold and the `chip_missed`
   assertion are done. Still open: calibrate the lock, capture and header
   windows on hardware (measure `SetTx` to `TxDone`, `SetRx` to the first
   catchable preamble, `RxDone`/`CrcErr` to FIFO read, CAD latency,
   sleep/wake recovery and half-duplex overlap, wired through attenuators, a
   combiner and a shielded box before any antenna), and do not grow the
   emulator further before those measurements exist. CAD stays out of the
   protocol until then.
4. `RadioQueue` is what the node carries other members' votes with, on request
   (D15). The distress class exists and nothing yet uses it; the first
   application traffic that is genuinely urgent should.
5. **Per-sender budgets before verification** (THREAT-MODEL F10): triggers,
   requests and frames per member per window, refused before the signature
   check and recorded as evidence when exceeded. Cheap; turns the
   `verifications` meter into a defence.
6. Evidence of misbehaviour is held locally (four frames) and reported in the
   log; there is no exclusion procedure. That is governance, not protocol, and
   needs a product decision before it is built.
7. Hub calibration: `sensitivity_dbm_at`, the capture threshold and the lock
   window against `gr-lora_sdr` FER and capture tables, or against the
   hardware matrix above -- whichever comes first (D16, D19).
8. The replay page (https://claude.ai/artifact/7ArnQomVMHiArWiZyUzfWp, version 8)
   now shows the D20 frame: 122 bytes on the air, a slot sized to 152.
   `examples/trace.rs` measures both from the wire format instead of carrying
   constants; `cargo run --example trace` regenerates the page's data.
9. Upstream: https://github.com/lora-rs/lora-rs/pull/487 makes `lora-phy`'s
   `rx()` return `RadioError::CrcError` / `HeaderError` instead of handing
   corrupted bytes up. Once it is merged and the git dependency moves past it,
   the virtual chip's IRQ peek (`Watch::irq_flags`) and the bench test that
   pins the old behaviour can go, and a real board gets the same telemetry.

Equivocation on the trigger (D6, D10) and acknowledgement on the air (D8) used
to head this list; both are built and measured in containers.
