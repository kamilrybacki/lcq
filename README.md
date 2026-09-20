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

**Implemented through D22.** Signed and sealed frames, a durable append-only
journal, slotted access anchored on the trigger, receiver-driven repair,
relaying, six adversary modes (test builds only), and a node that runs as a
real process against a channel emulator -- or against a virtual SX1262 under
the real `lora-phy` driver -- with two adapters onto real boards that no board
has tried yet. `docs/HANDOFF.md` has the state and the measurements;
`docs/DECISIONS.md` has what a later session must not silently reverse;
`docs/THREAT-MODEL.md` has the security audit and what still keeps this from
being a deployable node. The quorum arithmetic below is where it all started
and is unchanged.

## Usage

```rust
use lcq::{evaluate, Competence, Policy};

// Five members the manifest says count the same. A fleet whose members
// differ would give each its own score on the 1..100 scale.
let policy = Policy::new(
    ["a", "b", "c", "d", "e"].map(|id| (id.to_string(), Competence::FULL.value())),
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
| competence | `3·support > 2·total` | a crowd of less competent members cannot outvote the manifest's substance |

Integer arithmetic throughout: no rounding decides a safety threshold.

**Competence is a number the manifest gives a member, on a fixed scale of 1
to 100 (D24), and the manifest is a file every member reads (D25).** The protocol never learns how it was arrived at: the square
root of a language model's parameter count adjusted for quantisation, a
benchmark score, an instrument's calibration record, the agreement history of
a deterministic calculator, or a figure someone wrote down. Whoever assembles
the manifest normalises onto the scale; everything below sees the scores and
nothing else, which is what lets one fleet mix members that decide by wholly
different means. A member never declares its own competence on the air.

## What this is not

- **Not a blockchain and not general BFT.** Positive-only endorsement with a
  known membership is a much smaller problem, and the tests are an arithmetic
  oracle, not a safety proof.
- **Not an "all clear" signal.** There is no negative verdict. A false result
  means "not endorsed", never "no danger".
- **Not fault-tolerant at small N.** A one-member manifest is mathematically
  permitted and tolerates nothing.
- **Not immune to one very competent member.** The 3:1 competence cap does
  not stop a single member at the top of the scale from holding the competence
  threshold hostage. That is a property of the design, not a defect in the
  code.
- **Not a judge of competence.** The scale is taken from the manifest on
  trust; it is exactly as sound as whoever assembled it, and the manifest is
  not yet authenticated (`docs/THREAT-MODEL.md` F4).

## The fleet manifest

Who is in the fleet, and what each member's judgment is worth (D25). Every
member reads the same file; `docs/three-vessels.manifest` is one to copy.

```text
# Three vessels; the second and third defer to the first.
version 1
epoch 7
member 0 99 ship-alpha
member 1 66 ship-bravo
member 2 33 ship-charlie
```

`lcq-node --manifest <path>` takes the fleet from it. Indices must cover
`0..n` exactly once, because a frame names its author by index; competences
must be on the scale and within the 3:1 ratio cap, which a service can
guarantee by scoring into the band that cap implies (34 to 100). Without
`--manifest`, `--fleet N` gives synthetic members that all count the same,
which is what the container suites use.

Nothing in the file is signed, so it is a development and harness format
for now. It is not at the journal's trust level: a journal is one member's
own state, while this is the fleet's root policy, and whoever can write it
can change the membership, both quorum outcomes and the mission epoch. A
vessel needs the signed version 2 sketched in `docs/THREAT-MODEL.md` F4 and
F21.

The key that will sign it comes from the fleet's administrator, who hands its
public half to each vessel out of band for somebody aboard to type in (D26).
`wire::hand` is the encoding that survives that trip: Crockford's base32,
without the letters that get confused for digits when a key is read aloud,
in fourteen groups of four with four check symbols. A mistyped key is refused
when it is entered, not at the first manifest it cannot verify, with
probability `1 - 2^-20` rather than a guarantee.

Pinning a key this way does not stop somebody who can already write the
vessel's files. It makes a substituted key visible to whoever holds the
administrator's card, and only while the card is trusted independently, a
human compares against it at every start, and the value on screen comes from
an unmodified binary. It is manual tamper-evidence rather than a security
control, and on an unattended vessel it is worth nothing. The encoding is for
that one hand-typed key: the member keys a signed manifest carries need one
spelling per value, which reading `I` as `1` rules out.

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

## Running on hardware

Three ways onto a real SX1262, behind the same seam. The first is for the
boards on order (D23): a Seeed XIAO ESP32-S3 + Wio-SX1262 kit running the
bridge firmware under `firmware/bridge`, which makes the board a remote SPI
device so the unmodified `lora-phy` driver on the host drives the chip
directly. `docs/HARDWARE-BRINGUP.md` is the first day with them.

```sh
# First, what is on the other end of the cable, one rung at a time:
lcq-bridge --port /dev/ttyACM0 --radio
# Then a node. A 1.8 V TCXO and the DC-DC converter are the kit's defaults;
# `,tcxo=none` and `,ldo` say otherwise:
lcq-node --index 1 --fleet 3 --slots --journal ship1.log --radio bridge:/dev/ttyACM0
# An RNode (Heltec, LilyGO, RAK boards with RNode firmware) on USB serial:
lcq-node --index 1 --fleet 5 --slots --journal ship1.log --radio rnode:/dev/ttyACM0
# An SX1262 HAT on a Raspberry Pi: spidev, then the gpiochip and the BUSY,
# DIO1 and NRESET line offsets, and the TCXO voltage if the module has one:
lcq-node --index 1 --fleet 5 --slots --journal ship1.log \
  --radio spi:/dev/spidev0.0,/dev/gpiochip0,busy=24,dio1=16,reset=18,tcxo=1.8
```

All three live behind the `hardware` cargo feature, on by default. Members
on hardware need no hub: the air is the medium. The RNode and SPI adapters
(D21) have not met a board; the RNode one never will carry the protocol
well, since RNode firmware's CSMA delays every frame by up to seconds at
SF10 (D23).

## Security

`docs/THREAT-MODEL.md` is the audit: assets, trust boundaries, adversaries,
STRIDE per component and twenty findings, with an independent second review
merged in. What the node defends today: forged, tampered, replayed and
cross-case frames, tallies inflated by lies about acknowledgements, and nonce
reuse after a lost journal. What it does not have yet, and what makes it an
integration harness rather than a deployable secure fleet node: provisioning
(the group key and the member keys are constants in the binary), a signed
manifest and epoch lifecycle for rotation and exclusion, a rollback rule for a
restored journal, and replay state that survives a restart. The adversary
modes exist only in builds with `--features harness`; a release build refuses
`--adversary`.

## Development

```sh
cargo test                                   # everything, containers included when Docker is there
LCQ_RADIO=sx1262 cargo test --test containers  # the fleet on virtual SX1262 radios
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
