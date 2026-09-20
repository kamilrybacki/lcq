# Threat model and security audit

**2026-09-19**, against `main` at `d0c587a` (D1–D21). Two independent passes:
this one, from the code, and Hermes's (§8), from the repository as pushed.
Findings carry a severity, where they are, and what was done. Nothing here is
a proof; the adversary container tests (§6) are the evidence that the
defences named below are exercised rather than described.

## 1. What is being protected

| asset | why it matters | where it lives |
|---|---|---|
| **No fabricated quorum** | the whole point: an endorsement means ≥ threshold *genuine* members committed | signatures over the transcript (`wire/compact.rs`), count and competence thresholds (`domain/quorum`), one vote per member per case (`domain/state`, journal lock) |
| **Liveness within bounds** | a blocked fleet is safe but useless | slots on the trigger anchor, repair rounds, relaying, airtime budget (`lcq-node`, `application/budget.rs`) |
| **Member signing keys** | forge a member | provisioned out of band; today constants in the harness binary (F4) |
| **Group key** | membership: read the channel, put frames on it that open | shared symmetric key; same caveat (F4, F5) |
| **The journal** | the vote lock: a second binding vote after a restart breaks quorum intersection; the sequence counter: nonces | `infrastructure/log.rs`, one file per member |
| **The channel** | one LoRa channel, legally duty-cycled | the node's `AirtimeBudget`; jamming is a fact of life, not a defence |
| **Traffic confidentiality** | secondary: who voted what is group-private, not public | ChaCha20-Poly1305 under the group key |

## 2. Trust boundaries

```
  air (untrusted) ──► radio adapter ──► node process ──► journal file ──► application (Morsik, local MQTT/HTTP)
                      (hub | virtual   (one member,      (trusted with     (trusted; product scope,
                       SX1262 | RNode |  trusted)          the host)          not this crate)
                       SPI HAT)
```

- Everything from the air is hostile until the group key opens it (cheap) and
  a manifest key verifies it (expensive). Order of checks in `lcq-node`
  `admit`: manifest bounds → subject match → signature.
- The radio adapters are transports: an RNode or a HAT can drop, delay or
  garble; it cannot make a frame verify.
- The host is the member. A host compromise is a member compromise (A4).
- The hub emulator, the container harness and the `--adversary` modes are
  test equipment. They must not exist in a production build (F3).
- Provisioning of keys, the manifest and the mission epoch is outside the
  crate and undefined today (F4). Every property below assumes it is done
  right.

## 3. Adversaries

| class | has | can | cannot |
|---|---|---|---|
| **A0 outsider** | a radio, recordings | eavesdrop ciphertext, replay recorded bytes, jam, inject noise | open a frame (AEAD), make one verify, learn who voted |
| **A1 compromised member** | group key + own signing key + a slot | read the channel; equivocate, double-vote, lie about acks, replay, jam, withhold or delay relays, request repairs endlessly | forge another member (their key), alter a signed vote, count for more than one |
| **A2 coalition ≤ f** | f = ⌊0.4N⌋ of A1 | everything A1 can, together; block liveness outright | fabricate a quorum: the threshold `midpoint(N, f) + 1` makes any two approving quorums overlap in more than f members |
| **A3 coalition > f** | more than the design tolerates | fabricate a quorum | — out of scope by construction; the number is the contract |
| **A4 host/journal** | a member's filesystem | rewind the journal (double vote), delete it (nonce reuse: F1), read keys | anything A1 cannot, plus A1 |
| **A5 supply chain** | the build | anything | — mitigated by pinned deps (`Cargo.lock`, git rev) and pinned CI actions (F7) |
| **A6 operations** | wrong clocks, reused epochs, a stale manifest | starve liveness (skew), let old frames verify under a reused epoch, keep an excluded member in | — needs a provisioning design (F4, F5) |

## 4. STRIDE, per component

| component | S poof | T amper | R epudiate | I nfo | D oS | E levate |
|---|---|---|---|---|---|---|
| `wire/crypto.rs` seal/open | header (author, sequence) is AAD; nonce = author‖sequence; cross-member nonce collision proven and fixed (D5) | tag over header+body | — | group key = membership only | Poly1305 rejects outsiders at ~µs | **F1** nonce reuse across a lost journal |
| `wire/compact.rs` envelope | transcript covers stage, epoch, event, revision, heard, round, full case hash, reference, `started_at`, author, verdict, sequence; domain `lcq-v1-compact` | any field change fails the signature (tests: tamper every field) | signed by manifest key: non-repudiable | 8-byte case reference is a lookup key, not a leak | — | header author must equal envelope author must equal signer (`trigger_round`, `admit`) |
| `infrastructure/log.rs` journal | — | CRC32 detects torn writes, not a hostile editor (**F8**, host trust) | vote lock survives restart (D1, D11 tests) | ciphertext only; no keys | disk full → `NotDurable`, node stops voting rather than voting twice | **F2** replay windows not journaled |
| `lcq-node` admission | manifest bounds before signature; subject match before signature | — | evidence frames kept (4) on equivocation | logs carry counts and indices, never bytes or keys (**F9**) | verification only after AEAD; **F10** amplification needs the group key | `--adversary` modes in the binary (**F3**) |
| `application/replay.rs` | — | — | — | — | replays refused before verification, 64-deep per sender | memory-only until restart (**F2**) |
| `application/queue.rs` | — | — | — | — | bounded items/bytes, dedup 1024, `forget` only on a verified NACK | — |
| repair (NACK/resend/relay) | NACK must verify | relays carry signed votes unchanged | — | — | 3 rounds, one frame per slot, airtime budget; a lying `heard` bitmap is advisory (D8, test) | — |
| radio adapters | transport only | KISS deframer bounded (600 B); unknown commands dropped | — | RSSI/SNR to log only | a wedged modem = silence, watched by `chip_missed`/timeouts | CRC-garbled frames reach the parser on hardware until upstream #487 (**F6**); AEAD rejects them |
| test hub / harness | test only | — | — | — | — | never deployed (F3) |

## 5. Findings

| id | severity | finding | status |
|---|---|---|---|
| **F1** | **High** | A member whose journal is lost seals its next frames from sequence 0 with the same group key and author index: nonce reuse, keystream reuse, Poly1305 key exposure. `wire/crypto.rs` `nonce_for`; `infrastructure/log.rs` `open` counted from zero. | **Fixed (D22)**: `LogJournal::open_with_entropy` — a fresh log starts at a random sequence in [2^24, 2^31), persisted first; the node uses it. Residual: ~2^-31 per frame per pair of lives. **Also done (D27)**: the group key now follows the mission epoch, so a fleet that rotates after a loss gets a new key and the old sequence range cannot be reused under it. The derivation in this build is a labelled fixture; the property is real, the secrecy is not. |
| **F2** | Medium | Replay windows lived only in memory: after a restart every recorded frame verified again once (cost) and the state machine alone stood between a replayed vote and a double count (it does: one vote per member per case). | **Mitigated**: at start the windows are seeded from every frame the journal holds (witnessed votes, own frames). Residual: frames not journaled (triggers, NACKs) can be replayed once per restart, at the cost of a signature check each. |
| **F3** | Medium | The six adversary behaviours were reachable in any build via `--adversary`. | **Fixed**: behind the `harness` cargo feature (off by default); a build without it refuses the flag rather than running honest. The container harness builds with the feature. |
| **F4** | **High** (deployment) | Keys were constants: `GROUP_KEY`, `seed_for(index)`, `EVENT`, `REVISION` in `lcq-node`. Fine for a harness, fatal for a vessel. | **Partly closed (D27)**. A manifest is now signed and carries each member's public key, the mission epoch, a validity window, the policy and an opaque group-key id; `lcq-node --manifest` verifies it against the administration key a human entered by hand (D26) and **fails closed** on an unknown version, a bad signature, a window that has not opened or has closed, an index outside the fleet, a local key the manifest does not name, or an epoch its journal has already spent. The node's own key comes from `--signing-key`, a file it refuses to read if anybody else can. **Still open**: the group key itself. `GroupKey::fixture_for_epoch` derives one from the manifest's public id, which is no secret at all and is labelled as a fixture; a real one has to arrive through provisioning (Vault/sops through Morsik), per mission, and rotation on exclusion is F5. |
| **F5** | Medium | No group-key rotation and no member exclusion: a compromised member keeps reading the channel and keeps its slot for as long as the key lives; equivocation evidence is kept but nothing acts on it. | **Open — governance**. The protocol reports; who decides and how the fleet re-keys is a product decision. Until then a compromised member is a liveness risk, not a safety one. |
| **F6** | Low | On hardware, `lora-phy` hands CRC-failed frames up as receptions; the virtual chip peeks the IRQ status, a board cannot. The AEAD tag rejects them; the cost is a failed open. | **Upstream**: https://github.com/lora-rs/lora-rs/pull/487. |
| **F7** | Low | CI actions were pinned by tag. | **Fixed**: pinned by commit SHA. |
| **F8** | Info | The journal's CRC32 detects torn writes, not tampering. | By design: the host is the member (A4). Document; do not mistake it for integrity against an editor. |
| **F9** | Info | Logs: counts, indices, timings, RSSI. No frame bytes, no keys, no hex dumps (checked). | Keep it so; `report` is the only log path. |
| **F10** | Low | Verification amplification: a member (A1) can put verifiable-looking frames on the air at the channel's rate and cost every receiver an ed25519 check each. | Bounded by airtime -- at SF10 one member can force at most one ed25519 check per ~1.2 s, about 1 ms on a Raspberry Pi, under 0.1 % of a core -- refused before verification if replayed, metered (`verifications`). Recommended: per-sender rate — a member sending more than the schedule allows is dropped before verification and noted as evidence. |
| **F11** | Low | Clocks: pairwise skew up to 30 s is designed for; a spoofed GNSS time on one member shifts its local deadlines. | Liveness only: the schedule anchors on the trigger's end (D6, D7), and a member with a bad clock refuses or is refused, never counts twice. |
| **F12** | Info | A member at the top of the competence scale can hold the competence threshold hostage (README). Competence itself is taken from the manifest on trust; a member never declares its own (D24), so this is a manifest-integrity question, not an on-air one. | Documented property; the manifest's integrity is F4. |
| **F13** | Low | The journal file is created with the process umask. | **Fixed**: mode 0600 on creation (unix). It holds ciphertext, not keys, but nobody else needs to read it. |
| **F14** | Info | A malicious USB "RNode" can inject bytes; the deframer is bounded and every frame still needs the group key and a manifest key. | Accepted; the modem is on the trusted side of the USB port. |
| **F15** | Info | On-air metadata is not hidden: the cleartext header names the author index and sequence, and slots, timing and frame counts are observable. | By design; stated here so nobody expects the AEAD to hide who spoke. |
| **F16** | Medium | A fleet larger than `Heard::CAPACITY` (64) would lose acknowledgements silently. | **Fixed**: the node refuses to start with `--fleet` outside 1..=64. |
| **F17** | Medium | `RoundId` carries a 32-bit sequence while frames carry 64; a sequence past 2^32 would give every later round of that opener the same label (transport diagnostics, not safety). | **Guarded**: the entropy start keeps sequences below 2^31 and the node refuses to open a round whose sequence does not fit the label, rather than truncating. A wider label is a wire change for a later version. |
| **F18** | High (A4) | Journal **rollback** -- restoring an older copy -- rewinds the sequence and reuses nonces exactly as loss does; no persisted state can detect its own restoration. | **Open — deployment**, with the remedy now built (D27). A journal records the mission epoch it runs under, monotonically and across compaction, and a node refuses a manifest naming an epoch that journal has already spent. Since the group key follows the epoch, rotating after a restore makes the restored sequence range harmless. What this does **not** do is detect the restore: the rule fires only when somebody tries to reuse a spent epoch, and a restore that also restores the manifest looks like an ordinary boot. Closing it needs storage the host cannot rewind -- a secure element or a TPM counter. Fail closed until one exists. |
| **F19** | Low | RNode firmware is in the trusted computing base and was only detected, never identified. | **Partial**: the firmware version is logged (`rnode_firmware`) so a fleet can pin it; a hash-based attestation is not something the host protocol offers. |
| **F21** | **High** (deployment) → **Mitigated (D27)** | The fleet manifest was an unsigned file, so whoever could write it changed the membership, both quorum outcomes, the mission epoch and the index-to-member mapping -- enough to leave two members deliberating against different fleets. It is **not** at the journal's trust level: a journal is one member's own state, a manifest is the fleet's root policy. | **Mitigated**: version 2 is signed over a canonical form built after parsing, and a node acts on nothing else. The residual is where the trust root lives. The administration key is pinned by hand (D26) into a file on the vessel, and whoever can rewrite the manifest can usually rewrite that file too; what the node does is print the key's fingerprint at every start so a human holding the administrator's card can see that it changed. That is manual tamper-evidence, not a security control, and it is worth nothing on an unattended vessel. Hardware-backed pinning is the answer that does not depend on somebody looking. |

## 6. Evidence: the adversary tests

`tests/containers.rs`, every one a separate process per vessel, on both the
hub radio and the virtual SX1262: **forger** (holds the group key, signs as
another member — counts for nothing), **double-voter** (counted exactly once),
**ack liar** (cannot inflate a tally), **jammer** (blocks the fleet, fabricates
nothing), **replayer** (every delivered replay dropped before verification),
**equivocating opener** (evidence, not a split), and **two adversaries of
five** (the whole budget; safety holds). Plus the wire tests: tamper every
field, wrong key, wrong sequence, wrong group key, flipped bit, truncated
frame, altered header, cross-member nonce, a frame signed over another case.

## 7. What to do next, in order

Both reviews land on the same order.

1. **Provisioning (F4, F21)** — **done as far as the protocol can take it**
   (D26, D27). The trust root is the administration key the fleet's
   administrator issues and a person types in; `wire::hand` is the encoding
   that survives being read aloud; manifest version 2 is signed over the
   canonical form below and a node fails closed on anything less. What is left
   is not protocol but secrets: the group key per mission from Vault or sops
   through Morsik, each member's signing key generated on its own device, and
   the rotation path of F5. The rest of this item is the record of what was
   built, kept because version 3 has to keep it:
   a canonical signed body carrying each member's public key, a manifest id,
   the mission epoch, validity dates and a group-key id; each member's signing
   key on its device and never in an image; the group key per epoch from
   Vault/sops through Morsik; and a runtime that **fails closed** on an
   unknown version, an invalid or expired signature, a rollback, a missing
   local key, a local key that does not match the manifest, or fixture keys.
   The signature must cover a canonical form of the body, so the whitespace,
   comment and ordering rules have to be pinned before it is added rather than
   after. The member's ID stays an operator label: the principal is the index
   and the public key. Today's binary is an integration harness, and must be
   called one.

   **The canonical form, pinned now.** Review's rules, recorded here so that
   version 2 starts from them instead of deriving them again:

   - **Sign bytes built after parsing, not normalised text.** The signature
     covers a canonical byte form constructed once the directives have been
     read, never "the file after the parser tidied it".
   - **Version.** `version 2` is a new signed schema. A version 1 parser fails
     closed on a version 2 file and the reverse, rather than reading what it
     recognises.
   - **Record order.** Version, epoch, validity, policy fields, group-key id,
     then member records ascending by numeric index, then issuer and signature
     metadata last.
   - **Numbers.** Decimal ASCII only. No sign, no leading zeroes except the
     literal `0`, explicit bounds on every field.
   - **Text.** Restricted ASCII for operational identifiers, `LF` newlines.
     Comments and whitespace are not signed.
   - **Member identity.** The index and a fixed-length 32-byte Ed25519 public
     key are the cryptographic identity. The `id` is an operator label with a
     fixed character set and a maximum length.
   - **Public-key encoding.** Exactly 32 bytes in a machine encoding with one
     spelling per value, such as hex or base64url. **Not** `wire::hand`: that
     reads `I` as `1` and `O` as `0` deliberately, so several strings decode to
     the same bytes, which a canonical form cannot allow. Hand entry is for
     the one key a person types (D26) and nothing else.
   - **What the signature covers.** Competence, the Byzantine budget, the
     competence concentration cap, the `Heard` capacity and protocol limits,
     the PHY profile identity, the group-key id, the epoch and the validity
     interval.
   - **Rollback.** An epoch needs an anti-rollback rule and not merely an
     integer comparison.
   - **The issuer.** The administration public key is pinned separately, by
     hand. Any issuer fingerprint the manifest carries is an operator
     diagnostic and never the trust decision.
   - **The group secret is not in the manifest.** Only an opaque
     `group_key_id`. The key itself arrives through separate provisioning.
   - **Duplicates fail first.** A duplicate directive, index, public key or
     `id` is refused before canonicalization, not resolved by last-wins.
2. **Rollback and loss (F1, F18)** — a journal that is missing, replaced or
   older than the last one seen is a new epoch with a new group key, or the
   node does not start. Monotonic storage the host cannot rewind is the
   stronger answer; the rule is the cheaper one.
3. **Exclusion and re-keying (F5)** — the decision path from evidence to a new
   manifest; the new key never travels under the old one.
4. **Persist the rest of the replay state (F2)** — the windows themselves,
   scoped by epoch and manifest digest, expiring after validity plus relay
   grace, in the same crash-consistent journal.
5. **Per-sender budgets before verification (F10)** — triggers, requests and
   frames per member per window, so the `verifications` meter becomes a
   defence and the evidence store fills on abuse.
6. **Take upstream #487** when merged (F6), and drop the IRQ peek.
7. **Say what the radio cannot hide (F15)**: author index, sequence, timing,
   participation; and what no protocol solves: targeted jamming.
8. Re-run this audit after the first hardware session: a real front end and a
   real serial port are new attack surface only in the sense that they are
   real.

## 8. Independent review (Hermes)

Hermes reviewed `d0c587a` statically (Discord, 2026-09-19 19:54 UTC, ten
parts), without running tests, and without seeing the fixes recorded above,
which were made in response. The full text is on the channel; this is what it
established, with the file references it gave.

**Verdict.** "The protocol's wire crypto and crash ordering are thoughtfully
designed, but the current binary is a sophisticated integration harness. It
should not be called a deployable secure fleet node until provisioning, a
signed manifest/epoch lifecycle, rollback-resistant nonce state and persistent
replay defence are complete." Agreed, and adopted as the first line of §7.

**Every candidate confirmed.** (a) nonce reuse after a lost or rolled-back
journal — critical (`crypto.rs:96-113`, `log.rs:359-367`); (b) replay window
in memory only — high (`replay.rs:16-21`, `lcq-node.rs:277-283`); (c) no
group-key lifecycle or exclusion — high (`mission_epoch` a constant,
`lcq-node.rs:80-83`); (d) adversary modes reachable at runtime — medium/high
hardening (`lcq-node.rs:99-134`); (e) fleet keys derivable from the binary —
critical (`lcq-node.rs:45-46`, `:164-170`, `:1869-1873`); (f) CRC-failed
frames on real hardware — partial (`hardware.rs:82-86`, `radio.rs:399-446`).

**What the second pair of eyes added** (now F15–F20): the on-air metadata
that AEAD does not hide; the `Heard` capacity of 64 members with no hard
refusal; the 32-bit `RoundId` under a 64-bit sequence; journal rollback as
distinct from loss, and CRC32 as no defence against a hostile editor
(`log.rs:22-26`, `:446-505`); a queue identity without the sender; RNode
firmware in the trusted computing base without attestation; the `Heard`
bitmap lying to suppress retransmissions (advisory by design, D8); a case
reference that must never choose among several local cases on its own (it
does not: one subject per process). Also a residual on the hardware path --
DIO1 polling and SPI error paths can starve the node or flood it with events
-- that the first hardware session has to measure.

**Where the reviews differ.** Hermes ranks verification amplification high
(any group-key holder can force decrypt-and-verify work); this audit ranks it
low on the arithmetic in F10, while agreeing that per-sender budgets are the
right defence and cheap. On the journal, Hermes asks for monotonic hardware
storage; this audit adds the cheaper rule -- a restored journal is a new
epoch -- and leaves the choice to deployment (F18).

**Remediation order Hermes gave**: fixture secrets out of the production
path; a signed manifest and key lifecycle; fail closed on journal rollback or
loss; persisted, epoch-scoped replay state; a typed queue identity and a hard
fleet-size limit; the adversary harness out of the release artifact; the
real-hardware CRC/header gap closed before operational use; per-author rate
limits and accounting; and an explicit statement of what encryption does not
hide. Items 5, 6 and the limit are done above; the rest is §7.
