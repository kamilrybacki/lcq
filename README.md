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

**M1 only: the quorum-policy calculator.** Nothing is authenticated, nothing is
transmitted, nothing is stored. This is deliberately the smallest reviewable
piece — the arithmetic that everything later depends on.

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

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
