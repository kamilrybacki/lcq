# Decisions

Records of choices that a later session must not silently reverse. Each entry
says what was decided, what it rules out, and what would justify revisiting it.

---

## D1 — The durable journal is an append-only log, not SQLite

**Decided 2026-09-18. Implemented 2026-09-18** as `LogJournal` in
`src/infrastructure/log.rs`, with recovery tests in `tests/log_journal.rs`. Supersedes the SQLite gate in
`IMPLEMENTATION-ROADMAP.md` (M3) and the storage paragraph of the design spec.

### What has to survive a crash, and why

Three pieces of state, each for a different reason:

- **The vote lock.** If it is lost, a node that restarts casts a second binding
  vote while its first is already on the air. Two votes from one member break
  the quorum-intersection argument that the count threshold rests on. This is a
  safety failure, not a duplicate-message annoyance.
- **The sequence counter.** The sequence is the AEAD nonce. A rewound counter
  reuses a nonce under the same group key, which leaks the XOR of two plaintexts
  and exposes the Poly1305 key, enabling forgery against the group key. Not
  recoverable by retrying.
- **The outbox.** Store-and-forward without durability is forward.

The binding requirement is that **the vote lock and the outgoing frame commit
atomically**. A torn write that leaves a frame queued without its lock is
exactly the failure this layer exists to prevent.

### Why not SQLite

SQLite is a correct answer to a question this protocol does not ask. There are
no queries, no joins, no indexes and on the order of tens of records. What it
actually buys is careful `fsync` discipline against lying disks and partial
writes — and that is obtainable directly, at a fraction of the surface area:

| | SQLite | append-only log |
|---|---|---|
| atomic commit | yes | yes |
| dependency | a C library, several hundred KB | Rust, none added |
| records held | tens | tens |
| query surface used | none | none |
| recovery | WAL replay | truncate at the first bad record |

### The format this commits to

Not "a log file" in the loose sense. Specifically:

- Each record is `length prefix || payload || CRC32 of the payload`.
- A record is appended, then `fsync` on the file.
- On file creation, `fsync` the **parent directory** as well, or the file's
  existence is not durable even though its contents are.
- Recovery scans forward and **truncates at the first record with a bad CRC or
  a length that runs past end of file**. A partial tail is the expected state
  after a power cut, not corruption.
- The vote lock and the outbox entry for the same decision are written as one
  record, so they cannot be separated by a crash.

### What was actually built

`LogJournal` follows the format above. Two details are worth naming because they
are the ones that are easy to get wrong:

* The flush is `sync_all`, not `sync_data`. An append changes the file length,
  and the length is metadata; flushing only the data can leave a durable record
  inside a file that is still officially shorter than it.
* Recovery does not try to resynchronise past damage. It stops at the first bad
  record and truncates there, because a log is only meaningful as a prefix and
  guessing where the next record begins would invent history.

Compaction rewrites the live state to a sibling file, flushes it, renames it
over the original and flushes the directory. A crash at any point leaves either
the whole old log or the whole new one.

The tests check the invariant at **every byte offset** a crash could land on,
not at a few sampled ones, plus a real process killed with `SIGKILL` mid-write.
What they cannot check is whether `sync_all` reached the platter: the scratch
directory is usually a tmpfs, and no test can simulate a disk that lies about
flushing. That part rests on the ordering being right.

### What would justify revisiting

A node that needs to query historical cases rather than replay them, or a
deployment where an operator already relies on SQLite tooling for inspection.
Neither applies now. `MemoryJournal` and this adapter sit behind the same
`Journal` trait, so the swap is one file either way.

### Not a reason

"Rust reaches `no_std`" was considered and is **not** load-bearing: the crate is
std today and only written in a `no_std` style (`extern crate alloc`). The
atomicity requirement and the absence of any query workload carry the decision
on their own.

---

## D2 — SF10 is the operating floor; longer range comes from relaying, not SF12

**Decided 2026-09-18.** Supersedes the open "SF12 signature or fragmentation"
question in the handoff.

### The question was wrong

The earlier framing was "a signed vote does not fit 51 bytes at SF12, so we need
either a shorter signature or fragmentation." The 51-byte figure is LoRaWAN's
DR0 **application-payload** cap. `lcq` is peer-to-peer raw LoRa, where the PHY
carries up to 255 bytes at any spreading factor. A 105-byte signed frame fits
everywhere. What it cannot afford is the airtime.

### Measured, for one 105-byte endorsement frame

| SF | sensitivity | airtime | frames/h at 1 % | range vs SF10 |
|---:|---:|---:|---:|---:|
| 7 | −123.0 dBm | 179 ms | 201 | 0.60× |
| 8 | −126.0 dBm | 318 ms | 113 | 0.71× |
| 9 | −129.0 dBm | 574 ms | 62 | 0.84× |
| **10** | **−132.0 dBm** | **1067 ms** | **33** | **1.00×** |
| 11 | −134.5 dBm | 2298 ms | 15 | 1.15× |
| 12 | −137.0 dBm | 4104 ms | 8 | 1.33× |

Sensitivities are the SX1276 datasheet figures. Range is derived from them at
12 dB per doubling of distance, which is the two-ray far-field exponent over
water — the same model `sim::phy` uses.

### Why SF12 loses

Each step up buys about 2.5 dB but doubles the symbol time. Over water, where
loss grows 12 dB per doubling, 5 dB of extra budget is only **1.33× the
distance** — while costing **3.85× the airtime**.

Relaying wins on both axes:

```
1 hop  at SF12:  4104 ms, reach 1.33x
2 hops at SF10:  2134 ms, reach 2.00x
```

SF12 costs **1.92× more airtime for less reach**. And the duty cycle makes it
worse than that ratio suggests: at SF12 a node gets 8 frames an hour, so a
single contested vote at the five-attempt retry ceiling consumes more than half
of everything it may lawfully transmit — before any position report or
application message. We already have a scenario where 34 s of prior traffic
blocks a quorum outright; SF12 would make that the normal case.

Relaying is not new machinery. The protocol already stores and forwards, and the
signature means a relay cannot alter what it carries.

### What this costs, stated plainly

Relaying needs an intermediate node on the path. Two nodes alone at 1.5× SF10
range have no relay, and for them SF12 is the only option — at which point the
duty cycle binds hard and endorsement becomes a several-minute affair. This
decision says SF10 is the floor for the **design target**, a fleet, not that
SF12 must be refused if someone configures it.

### Also fixed here

`airtime_ms` applied the low-data-rate optimisation at SF10. The chip enables it
only where the symbol time passes about 16 ms, which at 125 kHz means SF11 and
SF12. Every SF10 airtime figure was therefore overstated by about 23 %, and
airtime is what the duty cycle is spent on.

---

## D3 — Members transmit in the slot their manifest index gives them

**Decided 2026-09-18.** Random contention is kept as a fallback and as the
baseline every measurement here is against.

### The problem, measured

A ten-member endorsement took 161 seconds. Of that, **17 seconds was
transmission and 144 seconds was silence** — the channel sat idle 89 % of the
time. The delay was not the radio. It was the access scheme: members drew a
random offset inside a 30 s contention window, collided, and retried inside a
window twice as wide.

That is self-inflicted. This is a *known-membership* protocol. Every frame
already carries `author_index`, its position in the manifest. Competing at
random for the channel throws away information the protocol is built on.

### Slots, counted from the trigger

Member *i* transmits at `i × (airtime + guard)` after the frame that triggered
the round.

The reference point matters. Agreeing on a shared wall-clock instant to within
a second is impossible here — `MAX_CLOCK_SKEW_SECONDS` is 30. But agreeing on
*when that frame ended* needs only millisecond accuracy, which the demodulator
already has, and a member that did not hear the trigger cannot take part
anyway. A 200 ms guard covers demodulation jitter and drift across one round.

### Measured against the same fleets

| scenario | random | slotted | frames | collisions |
|---|---:|---:|---|---|
| 10 members | 161.2 s | **12.5 s** | 16 → 10 | 6 → 0 |
| 20 members | 181.7 s | **25.1 s** | 42 → 20 | 22 → 0 |
| 10 members, 60 % loss | 607.3 s | **47.9 s** | 27 → 25 | 5 → 0 |
| convoy, 4 km spacing | 660.8 s | **63.1 s** | 19 → 18 | 1 → 0 |

Collisions are zero in every case, not merely fewer: two members cannot pick the
same slot because neither picks at all.

**The result that is not about latency:** the scenario where 34 of 36 seconds of
hourly airtime were already spent on other traffic *failed* to reach quorum
under random access and *reaches it* under slots. Without collisions there are
no retries, so the remaining budget is enough. Scheduling turned an unlawful-or-
blocked situation into a working one.

### What this does not buy, and what it costs

* **It does not repair a partition or reach a distant member.** Scheduling
  decides who talks over whom; it cannot make an unreachable member audible. A
  scheme that appeared to would be inventing quorum. Both are asserted in
  `tests/access.rs`.
* **It makes jamming easier to aim.** A faulty member — and the fault budget
  allows 40 % of them — can transmit in someone else's slot and silence that
  specific member every round. Under random access the damage is spread. This
  is a **liveness** regression, not a safety one: no schedule lets anyone forge
  a signature, so a jammed fleet blocks rather than approves. A determined
  jammer defeats both schemes by transmitting continuously.
* **Silent members still hold their slots.** A round is `N × (airtime + guard)`
  whether members answer or not. Compacting would require knowing in advance who
  is going to stay quiet.
* **It scales linearly, which random access does not.** A round grows with *N*;
  random collisions grow with *N²*, which is why the advantage widens from 12×
  at ten members to more at twenty.

### Still open

Piggybacked acknowledgement. Members currently retry up to five times with no
way to learn they were already heard, which is pure waste on a budget this
tight. The observer's own frames could carry a bitmap of verified indices.

---

## D4 — Where the remaining airtime goes, and what is worth spending effort on

**Recorded 2026-09-18.** Not a decision so much as the measurement that any
later optimisation has to argue against. Produced by `examples/budget`.

### The frame is two fields

| field | bytes | share |
|---|---:|---:|
| ed25519 signature | 64 | 61 % |
| content hash of the case | 32 | 30 % |
| everything else | 9 | 9 % |

Under slotted access (D3) the channel is busy for most of a round, so latency is
now dominated by airtime itself. Airtime is very nearly linear in payload, which
kills one idea before it costs anything: **packing several votes into one frame
does not help.** Three votes in one 210-byte frame cost 642 ms per vote against
821 ms alone — a 22 % saving that comes only from amortising the preamble, and
five votes do not fit a `LoRa` payload at all.

### Ranked by what they are worth

1. **A short case reference instead of the full hash.** 105 B → 77 B, 1067 ms →
   821 ms, a **23 % cut to every frame the protocol will ever send**. No security
   is traded: the signature keeps covering the full 32-byte hash, which the
   receiver reconstructs from the case it already holds. The short tag is a
   lookup key, and picking the wrong case simply fails verification. Cheap,
   contained, and it compounds with everything below.
2. **Stop once the threshold is met.** Members past the quorum need not vote.
   Ten members with a threshold of eight is a 20 % saving, and the ratio holds
   as fleets grow.
3. **An acknowledgement on the air.** Measured below; worth about 4.5× the
   airtime. Larger than either of the above, and not yet designed.
4. **BLS aggregation.** Ten signatures become one 48-byte aggregate, so a whole
   endorsement fits a single frame: 10.7 s of channel time becomes 0.9 s. An
   order of magnitude, and the only way past the signature. It costs pairing
   arithmetic, a substantial dependency, slower verification on a constrained
   node, and a different curve assumption. A milestone, not an afternoon.

### The acknowledgement, measured rather than assumed

The simulator lets a sender stop retrying once its frame is decoded. That
assumes the sender **learns** it was heard, and nothing in the wire format says
so. `Journal::acknowledge` exists at the storage layer; no frame carries the
fact.

Dropping the assumption costs 46 frames instead of 10, and 4.9 s of airtime per
node instead of 1.1 s — **33 decisions an hour per node against 7**.

The first guess was that this would break quorum under a tight budget. It does
not, and the measurement said so: under slots the first attempt always lands, so
every blind retry is waste arriving *after* the vote already counted, and the
1 % limit clips the waste rather than the vote. An acknowledgement is worth a
great deal of airtime and no correctness at all.

### Not worth doing

Truncating the signature. Concatenating votes without aggregating them. Raising
the spreading factor for range (D2). Assuming a shared wall clock (D3).

---

## D5 — What the simulation was actually testing, and the two bugs that found

**Recorded 2026-09-18**, after an audit prompted by the question "does the
simulation reflect the real algorithm". The answer was no, and building one that
did found two defects in the protocol itself.

### What `sim::Scenario` covers

It signs frames, collides them on a modelled channel and counts signatures. It
never touched:

| layer | exercised |
|---|---|
| quorum arithmetic (`Policy`, `evaluate`) | yes |
| signatures (`CompactEnvelope`, verify) | yes |
| radio (collisions, capture, path loss, duty cycle) | yes |
| **state machine** (`Case`, `Phase`, expiry, cutoff) | **no** |
| **journal** (vote locks, sequence reservation) | **no** |
| **clock** (`Clock`, skew) | **no** |
| **group encryption** (`seal`/`open`) | **no** |
| radio queue (priority, dedup) | no |

So every figure this project reported came from a model of **one round of one
stage**. `sim::Deliberation` drives the real objects and is the one entitled to
say anything about the protocol.

### Two defects, both found by building it

**Cross-member nonce collision (critical).** The group key is shared; each
member counts its own sequence from zero; the sequence was the entire nonce.
Two members' first frames therefore sealed under identical keystream.
`examples/nonce_proof` showed the XOR of the ciphertexts reproducing the XOR of
the plaintexts. Fixed by putting the author index in the nonce. An independent
review reached the same conclusion unprompted and rated it a blocker.

**Frames could not be opened.** The sequence is the nonce and lived only inside
the ciphertext that needed it. `seal_frame`/`open_frame` now carry the author
index and sequence in a cleartext header which is also the AAD.

**And one in the simulator that is a lesson about the protocol.** A collided
vote must be retransmitted *from the outbox*. Rebuilding it asks the journal for
a second vote lock on the same case, which it correctly refuses — and the member
then never retransmits, losing its vote to the first collision with no error
raised anywhere. The lock stops a second *decision*, not a second *transmission*,
and an implementation that conflates them fails silently.

### What the faithful run says

| | random | slotted |
|---|---:|---:|
| elapsed | 421 s | 346 s |
| radio time | 96 s | 45 s |
| airtime | 54.7 s | 38.2 s |
| on-air frame | 136 B | 136 B |

**No endorsement can complete in under 330 s** — the 300 s consultation cutoff
plus the skew allowance — however fast the radio is. The radio is 13 % of the
elapsed time. The earlier "12× faster endorsement" (D3) measured one stage as
if it were the whole thing; the scheduling win is real but it is a **radio-time
and airtime** win, not a latency one. Airtime still matters most, because it is
what the duty cycle meters.

### Open blocker: the slot schedule has no canonical anchor

Raised by review, and correct. D3 anchors slots at "the frame that triggered the
round". A compromised member can broadcast **different but validly signed
triggers to different receivers**, splitting honest nodes onto colliding
schedules. Signatures do not prevent equivocation by a manifest member.

The tension is real and not yet resolved:

* Anchoring at signed content (`subject.started_at()`) is canonical but
  wall-clock, so slots would have to exceed the skew budget — 30 s slots for a
  1.3 s frame.
* Anchoring at a heard frame gives millisecond accuracy but is equivocable.
* A middle option is to make the round context include the trigger's hash, so
  two triggers are two different rounds: the split stays visible and no quorum
  is fabricated, but liveness still suffers.

Until this is settled, slotted access is an airtime optimisation with a known
liveness hole, not a security mechanism. Also outstanding from the same review:
per-sender replay windows with retention limits, cheap rejection of senders
outside the manifest, and reporting targeted slot denial and fairness rather
than only collision counts.

---

## D6 — The slot schedule must anchor on the trigger, so equivocation has to be solved directly

**Decided 2026-09-18**, by measurement. Closes half of the blocker left open in
D5 and sharpens the other half.

### The two anchors, measured

D5 left the anchor undecided between a heard frame (accurate but equivocable)
and signed content on each node's wall clock (canonical but skewed). Every
figure before this rested on a fiction: all members shared one clock. Giving
each member its own error, drawn rather than spread — a monotone spread by index
runs in the same order as the slots and pushes members apart instead of into
each other — settles it.

Ten members, twenty seeds each, slotted access:

| anchor | skew | frames | collisions | airtime | quorum |
|---|---:|---:|---:|---:|---|
| trigger | 0 s | 30 | 0 | 38.2 s | always |
| trigger | 2 s | 30 | 0 | 38.2 s | always |
| trigger | 10 s | 30 | 0 | 38.2 s | always |
| trigger | 30 s | 30 | **0** | 38.2 s | always |
| clock | 0 s | 30 | 0 | 38.2 s | always |
| clock | 2 s | 91 | **76** | 115.8 s | **lost in some runs** |
| clock | 10 s | 94 | 79 | 119.1 s | lost in some runs |
| clock | 30 s | 76 | 56 | 96.7 s | lost in some runs |

A clock-anchored schedule does not degrade gracefully: it collapses as soon as
clocks are allowed to disagree **at all**. The reason is a ratio, not a detail.
A slot is about 1.5 s wide and the skew budget is 30 s, so members land in each
other's slots immediately. Surviving it would need slots wider than the budget —
a 30 s slot for a 1.3 s frame — which discards the whole point of scheduling.

### Therefore

**Slots anchor on the trigger.** There is no second option, so the equivocation
problem cannot be dodged by changing anchors and has to be solved head on.

### Recommended treatment of equivocation, not yet implemented

A compromised member can send different, validly signed opening frames to
different receivers. Signatures do not prevent a member from saying two things.
The available mitigations are about **detecting and containing** it:

1. **Make the round identity the trigger's hash.** Two different triggers are
   then two different rounds, and their votes never merge into one tally. The
   fleet's schedule can still be split, but a quorum cannot be fabricated from
   the halves, which is the property that actually matters.
2. **Treat hearing two triggers for one subject as evidence.** A member that
   sees both holds signed proof that one member said two things, which is
   exactly the material an exclusion procedure needs.
3. **Fall back to random contention on detection.** Contention is immune to
   equivocation because it has no schedule to split. Slower, and correct.

That combination keeps safety unconditional and degrades liveness gracefully,
which is the same shape as every other trade in this protocol. Until it is
built, slotted access remains an airtime optimisation with a known liveness
hole, and random contention stays the default.
