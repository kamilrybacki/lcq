# Handoff — Claude Code / next implementer

## State at 2026-09-18 (M1 implemented, awaiting user review)

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
| `cargo test` | 21 passed (10 policy, 8 evaluation, 3 property) |
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

M2, after user review. Do not start it unprompted.

## Original state at 2026-09-18

- GitHub: https://github.com/kamilrybacki/lorai.git
- Local checkout used to prepare this handoff: `/home/kamil-rybacki/Code/lorai`.
- Separate integration repository: https://github.com/kamilrybacki/morsik-lora.git
- Original Morsik code inspected read-only: `/home/kamil-rybacki/Code/Baltic_Hackaton_26`.
- Product design and acceptance criteria were discussed and approved. User requested specifications and plans usable by Claude Code.
- Implementation has NOT started. No package, test suite, radio, model, broker or container has been launched for `lorai`.
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
