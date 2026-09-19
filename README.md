# LCQ Protocol

**LoRa-based Confidence Quorum Protocol.** Crate: `lcq`.

A known-membership endorsement protocol for constrained radio networks: a fleet
whose members already hold each other's keys agrees, over one narrow LoRa
channel, whether a safety claim has enough confident support behind it to be
acted on. Membership is fixed, so the question is never who may speak — only
whether enough of them committed.

(*LoRa*, the radio modulation. Not *LoRA*, the machine-learning method.)

Built for fleets that must agree on a safety claim over LoRa, where a payload is
tens of bytes, there is no central server, and up to 40 % of members may be
compromised.

## Status

**Implemented through D16.** Signed and sealed frames, a durable append-only
journal, slotted access anchored on the trigger, receiver-driven repair,
relaying, six adversary modes, and a node that runs as a real process against
a channel emulator -- or against a virtual SX1262 under the real `lora-phy`
driver. `docs/HANDOFF.md` has the state and the measurements; `docs/DECISIONS.md`
has what a later session must not silently reverse. The quorum arithmetic below
is where it all started and is unchanged.

## Usage

```rust
use lcq::{evaluate, Policy};

let policy = Policy::new(
    ["a", "b", "c", "d", "e"].map(|id| (id.to_string(), 1)),
)?;

let signers = ["a", "b", "c", "d"].map(str::to_string);
let result = evaluate(&policy, signers)?;

assert!(result.approved());
# Ok::<(), Box<dyn core::error::Error>>(())
```

> **Trusted signer IDs only.** `evaluate` cannot check a signature, a message
> scope, a revision or a replay. Feeding it unvalidated network input produces
> arithmetic with no security meaning. Authentication belongs to a layer that
> does not exist yet.

## Thresholds

Both must be met, and both are reported separately because they fail for
different reasons and call for different responses.

| | rule | why |
|---|---|---|
| count | `midpoint(N, f) + 1` where `f = floor(0.4·N)` | any two approving quorums overlap in more than `f` members |
| weight | `3·support > 2·total` | a crowd of light members cannot outvote the manifest's substance |

Integer arithmetic throughout: no rounding decides a safety threshold.

## What this is not

- **Not a blockchain and not general BFT.** Positive-only endorsement with a
  known membership is a much smaller problem, and the tests are an arithmetic
  oracle, not a safety proof.
- **Not an "all clear" signal.** There is no negative verdict. A false result
  means "not endorsed", never "no danger".
- **Not fault-tolerant at small N.** A one-member manifest is mathematically
  permitted and tolerates nothing.
- **Not immune to a heavy member.** The 3:1 weight cap does not stop a single
  heavy member from holding the weight threshold hostage. That is a property of
  the design, not a defect in the code.

## Running a fleet

One channel emulator, then one process per member. Members hold the hub's
address; the emulator models airtime, reach, collisions and loss.

```sh
cargo run --release --bin lcq-hub -- --bind 127.0.0.1 --port 9000 --fleet 5 --scale 100
cargo run --release --bin lcq-node -- --index 1 --fleet 5 --hub 127.0.0.1:9000 --scale 100 --slots --journal ship1.log
# ... members 2 to 4 likewise, then the one that opens the round:
cargo run --release --bin lcq-node -- --index 0 --fleet 5 --hub 127.0.0.1:9000 --scale 100 --slots --journal ship0.log --trigger
```

`--radio sx1262` puts a member on the virtual SX1262: the unmodified `lora-phy`
driver, a behavioural chip model with a clock, the same hub as its medium (D16).
The container suite (`tests/containers.rs`) does all of this in Docker, one
container per vessel, and `LCQ_RADIO=sx1262` runs the whole suite on the
virtual chips.

## Development

```sh
cargo test                                   # everything, containers included when Docker is there
LCQ_RADIO=sx1262 cargo test --test containers  # the fleet on virtual SX1262 radios
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
