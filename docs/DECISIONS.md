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

---

## D7 — The protocol across real processes, and four things only that could show

**Recorded 2026-09-19.** `src/bin/lcq-node.rs`, `src/bin/lcq-hub.rs`,
`tests/multiprocess.rs`.

Every earlier result came from one process, where nodes shared a heap, a
scheduler and — until D6 — a clock. Each member is now its own operating-system
process with its own journal on disk, its own clock and its own error, reaching
the others only through a channel emulator that enforces the two things that
make a radio a radio: **one frame at a time, and frames take time**.

Five processes, slotted access, no loss: **zero collisions, sixteen frames, and
all five independently reach the same verdict**. Disagreement there would mean
the quorum depends on who you ask, which is the failure the whole design exists
to prevent.

### The anchor is when the trigger *ends*

D6 forced a trigger anchor. Implementing it across processes exposed a detail a
single process cannot have: the originator knows its trigger at the instant it
starts sending, and everybody else knows it only once the frame has finished
arriving. An originator counting from its own first symbol therefore runs a
whole airtime ahead of the fleet — and lands its slot *k* on top of everyone
else's slot *k−1*. The shared instant is the end of the trigger, not its start.

### A node must not miss its own slot because it is busy listening

The first version verified received frames before checking whether it was time
to transmit. Verifying an ed25519 signature is not free, and in a debug build it
is slow enough to push a send past its slot and into the next member's. Send
checks now come first and at most one frame is read per turn. This is not a test
artefact: a real node on a constrained CPU has the same problem, and a node that
loses its slot to its own workload collides with whoever comes next.

### A node must count its own vote

The emulator does not echo, so a member heard everyone except itself. For a
fleet of five that still cleared a threshold of four, which is exactly the kind
of accident that survives review. A node needs no radio to know its own
utterance.

### Scaling amplifies real jitter by the scale factor

The protocol's shortest interval is five minutes, so an honest multi-process
test needs a clock faster than wall time. The cost is that every real-world
jitter is multiplied too. At scale 100 a 200 ms guard interval is 2 ms of wall
time — below the Linux scheduler's noise — and the schedule falls apart for
reasons that have nothing to do with the protocol. At scale 20 the same guard
is 10 ms and it holds.

**The usable scale is bounded by the guard interval divided by the host's
scheduling jitter.** A result obtained above that bound is measuring the host.
This is a limit on the method, and it is why these tests run at 20 and take
about ninety seconds.

### What this still does not do

No radio hardware. The emulator models occupancy and collisions but runs over
loopback, so every link is perfect and equally strong — there is no capture
effect to be had, and the propagation model of `sim::phy` is not in the path.
Nodes send once per stage: there is no retry policy here, because without an
acknowledgement on the air there is nothing to retry against (D4).

### Addendum: one container per vessel

`tests/containers.rs` takes the same protocol a step further: each member is a
container with its own root filesystem, its own network namespace and a journal
on a mount no other member can see. The harness drives Docker from code --
builds the image, launches the ships, sinks them, refloats them, tears it all
down. There is no compose file and no Dockerfile: the binaries are statically
linked, so `docker import` turns a tarball straight into an image and the whole
vessel is two files and a kernel.

Twelve vessels finish in the same wall-clock time as five, which is the property
worth having: a slotted round grows linearly with the fleet while the
deliberation window does not grow at all, so **members cost airtime, not delay**.

Three things the containers forced that processes on one host did not:

* **A round cannot be opened on a timer.** The harness launches every vessel but
  the opener, waits for each to report itself listening, and only then launches
  the one that opens the round. A fixed delay is a guess about how long Docker
  takes today, and a round opened while vessels are still starting is a round
  opened for nobody.
* **The case begins at the trigger, not at boot.** The protocol clock used to
  start when the process did, so a vessel that waited ten seconds for the fleet
  to assemble believed two hundred protocol seconds of deliberation had already
  passed, and finished the whole thing before anybody spoke. The clock now
  starts when the trigger does.
* **A vessel that comes back after the round has closed does not invent a
  verdict.** It recovers its lock, declines to decide again, and waits. The test
  asserts the silence, because a refloated member announcing an outcome for a
  round it missed would be exactly the fabrication this protocol forbids.

---

## D8 — Acknowledgement rides on the frames already being sent

**Decided and measured 2026-09-19.** Implements the largest item D4 left open.

### The cost of not having one

The simulator let a sender stop retrying once its frame was decoded, which
assumes it somehow *learns* it was heard. Nothing carried that.
`Journal::acknowledge` exists at the storage layer; no frame said it. D4 priced
the assumption at roughly four and a half times the airtime.

### Eight bytes, no extra frames

Every binding frame now carries `Heard`: one bit per manifest index, for the
members its sender has admitted a binding vote from. It rides on frames the
protocol already sends, so acknowledgement costs **eight bytes rather than a
frame** — 1313 ms to 1354 ms of airtime, about 3 %.

It is covered by the signature, so it cannot be rewritten in flight or lifted
onto another frame.

### Measured across five containers

Both fleets allowed four attempts at their binding vote; one reads the
acknowledgements, one is told to ignore them.

| | attempts spent | members acknowledged |
|---|---:|---:|
| ignoring acknowledgements | 20 | 0 / 5 |
| reading them | **8** | 4 / 5 |

**Two and a half times fewer transmissions**, for 3 % more per frame. The fifth
member is the one in the last slot: nobody transmits after it in the first
round, so nobody has anything to report about it yet, and it spends one extra
attempt. That is inherent to a slotted order, not a defect.

### Advisory, never binding

A member can lie about what it heard and silence somebody who was not. That is a
liveness attack of the same class as jamming a slot (D6): no bitmap and no
schedule lets anyone forge a signature, so a fleet fed lies **blocks rather than
approves**, and the count threshold is still decided by signatures alone. A
receiver therefore treats the bitmap as a reason to stop early and never as
proof.

Sixty-four members is the ceiling, because one `u64` of bits is what a frame can
spare. An index past that is dropped rather than wrapped: dropping costs a
retransmission, wrapping would silence the wrong member.

### Found while building it

The binding stage ended before the retries it was configured for, so the first
measurement was of the stage window rather than of acknowledgement — both fleets
spent nine attempts and the difference was invisible. The stage is now long
enough for every attempt it allows.

---

## D9 — Reliability hardening: what a second review and real processes found

**Decided and measured 2026-09-19.** Prompted by "the protocol has to be
reliable", taken as three separate questions: does it never fabricate a quorum
(safety), does it reach a verdict when honest members can hear each other
(liveness), and does it survive crashes, skew and malicious members inside the
fault budget (robustness). An independent review contributed several of the
points below; where it changed the design that is said.

### Safety

* **Opening a round is the full admission path.** The trigger used to be
  decrypted and its stage checked, nothing more, so any holder of the group
  key could open rounds as any member. It is now verified like a vote: the
  signature against the manifest, the cleartext header against the signed
  envelope, the subject, and the replay window.
* **Every frame is checked against the subject.** The receiver used to build
  the opinion from its *own* subject, so an utterance about a different event,
  revision or content would have been admitted as if it were about ours. The
  state machine's subject check can only catch what the receiver hands it.
* **The replay window advances only after the signature verifies.** The
  cleartext header is a claim; advanced any earlier, anyone holding the group
  key could poison a member's window with forged headers.
* **`RoundId` is a transport label, never a security identity.** Review's
  correction, and right: a compromised opener ignoring its journal can send two
  triggers with the same `(index, sequence)` at different times, so the label
  cannot prove anything. Votes bind to the subject and the journal allows one
  per member whatever round they were cast in. The label organises transmission
  and names evidence; the signed frames are the evidence.
* **A frame wider than the slot is refused at the sender.** Slots are sized to
  `MAX_FRAME_BYTES` (176, the widest legal frame with every field at its longest
  encoding), never to the frame in hand, so a field added later can only make a
  sender refuse rather than overrun its neighbour.
* **A repeated non-binding opinion is the same opinion**, neither counted twice
  nor refused as a fault.

### Timing under skew

* **The binding stage opens at `cutoff + 2·SKEW + PHASE_SETTLE`.** A member
  closes consultation on its own clock once certainly past the cutoff, so the
  one furthest behind closes a whole pairwise budget after the one furthest
  ahead, and the slot schedule counts from a shared instant. The settle budget
  is separate from skew on review's advice: it covers a strict comparison,
  polling and turnaround, not clock disagreement.
* **A missed slot is never caught up.** A member whose slot passed before it
  closed spends the attempt and waits for its own slot in the next window. Firing
  late lands in whoever's slot is current, turning one member's liveness problem
  into two members' collision.
* **The schedule lives on the anchor's timeline; only the subject's deadlines
  live on the local clock.** Found by the skewed-fleet test: the member fifteen
  seconds ahead decided the round was over before its own binding slot.
* **Listening stops at the endorsement target, or after the scheduled window
  once a quorum is seen — never at "my frames are out", and never at the fourth
  vote of five.** Both were found by tests: the first left a split fleet half
  deaf, the second reported four supporters with the fifth member's slot still
  to come.
* **A binding vote that arrives before this node has closed is held, not
  dropped**, and offered again the moment consultation closes. Under a split
  the halves' anchors differed by two minutes and one half was refusing every
  vote from the other. Safety is unchanged: the state machine checks the held
  vote exactly as it would have.

### Liveness

* **Any member may open a round.** Turns rotate with the subject's content, so
  the same low index does not always open first — a member that always opened
  would be the fleet's de facto scheduler — and each waits two slot widths
  longer than the one before. Nobody opens the instant it boots. Measured with
  the designated opener never launched: member 1 opened, four of four agreed.
* **Split detection needs two independent members.** A vote's round label is
  written by the voter, so one compromised member could stamp a foreign label
  on its own frames and push the whole fleet off its slots. Review's threshold
  was "two verified triggers"; that is too strict, because in a real split most
  members never hear the second trigger — they hear the other half's votes. Two
  distinct verified members disagreeing, by label or by timing implied from a
  vote's arrival, is the threshold; a lone liar cannot reach it. Detection is
  once per subject and never resets the anchor.
* **After a split, retries are randomised from the operating system's
  entropy.** In-slot retries would repeat the same collision every window; a
  seeded generator would tell an adversary when to be waiting. Anchors are never
  reconstructed or canonicalised — review argued against it and the argument
  holds: reception time is not a shared clock, and "lowest label wins" invites a
  member to send one.
* **A member that missed the opening joins from the first vote it hears.** The
  schedule is deterministic, so a vote's sender and stage say exactly when the
  round began. Measured under 30 % loss, where the opening itself is lost to
  some receivers: one member joined this way and quorum landed.

### Measured, one container per vessel

| scenario | result |
|---|---|
| clocks skewed by the full 30 s pairwise budget | 0 collisions, 5 / 5 |
| designated opener never launched | member 1 opened, 4 / 4 |
| halves isolated while the round opened | 2 openings, 5 / 5 detected the split, 0 collisions, quorum |
| 30 % of deliveries dropped | 1 member joined without the opening, quorum |
| twelve vessels | 12 / 12 in the same wall time as five |

### What the harness taught

Distributed opening with a zero default delay turned every test into several
rounds, and the failures looked like protocol bugs. A member now waits five
seconds by default; the harness gives waiting members eight. Isolation for the
split scenario is measured from the first frame, not from emulator start,
because the harness spends seconds launching. And every failing assertion now
prints the refusals, missed slots and splits each vessel logged — the two
timing bugs above were invisible as numbers and obvious as lines.

### Still open

Evidence is kept but not reported anywhere; there is no exclusion procedure.
The opening backoff is rank-ordered on each member's own clock, so under skew
two members can open within one airtime and collide — bounded by the fault
budget, handled by detection, not eliminated. Per-sender rate limits on
accepted triggers. Radio hardware.

---

## D10 — Adversaries in containers, and what acknowledgement means under loss

**Decided and measured 2026-09-19.** Everything before this was measured with
honest members in bad conditions. The fault budget allows 40 % faulty members,
and none had ever been put on the air. `lcq-node --adversary <mode>` makes one
member misbehave in a chosen way; one container test per mode asserts that
safety holds and records what liveness costs.

### Six adversaries, one at a time

| adversary | what it does | measured |
|---|---|---|
| forger | signs as another member, holds the group key | 12 refusals across the honest four; forger counts for nothing; 4 / 4 endorse |
| double voter | ignores its journal, casts a second contradicting vote | every honest member refuses the second (`already cast`) exactly once; counted once; 5 / 5 |
| acknowledgement liar | claims to have heard everyone, under 30 % loss | nobody endorses below threshold; 4 honest members stopped retrying after one attempt |
| jammer | junk in the next member's binding slot every window, votes for nothing | 20 jams, 3 collisions; the three who could not hear the jammed member block at 3 / 4; **the jammed member endorses at 4 / 4** |
| replayer | puts a captured frame back on the air during the vote | every replay dropped before a signature check; quorum unaffected |
| equivocating opener | opens the round, then opens it again under a new label | 4 / 4 keep the second opening as evidence; **0 declare a split**; 5 / 5 |

### Two findings that changed the design

**A jammed member may hold a quorum the rest cannot see.** The first assertion
written for the jam test was "nobody endorses". It was wrong. The jammed member
cast its vote — it collided, but it exists, signed and journalled — and heard
the other three: four genuine signatures, which *is* a quorum. Nobody
fabricated anything. What jamming does is make the fleet **disagree about
liveness**, never about safety: one member holds a verdict the others lack. The
answer is store-and-forward, not a different tally. The test now asserts
exactly that: nobody counts more than the four who actually voted, the three
block, the one endorses.

**"Somebody heard me" is the wrong stop rule under per-link loss.** The
acknowledgement bit (D8) stops a sender once *anyone* reports hearing it. Loss
is per link: a receiver that dropped that frame never gets it, and at 30 % loss
each receiver was missing about one vote in four. The earlier passing runs were
luck; measured five times, one in five failed. Stricter stop rules do not help,
because acknowledgements ride only on frames and frames stop — the last member
in slot order is never acknowledged by anyone's first frame, and a "stop once
everyone acknowledged" rule chains into every member exhausting its attempts.

The fix is the classic one for a broadcast channel: **receiver-driven repair**.
After the scheduled attempts, a member still missing a vote from anyone who
*spoke* in consultation sends, in its own slot, a frame saying what it holds —
the same `heard` bitmap, under a new stage code. Any member absent from that
list resends its committed vote once, from the outbox, in the next window.
Three such rounds: request and resend are each a frame and each is lost as
readily as the vote was, so one round left 20 % of runs short and three put it
under 3 %. Measured five times after: five of five, four to eleven requests and
two to five resends per run. The acknowledgement test is unchanged at eight
attempts, since a clean channel has nothing to repair.

### Three defects the adversaries exposed in the honest code

* **Timing-based split detection fired on every honest retransmission.** It
  assumed each frame was a first attempt, so a retry one window later implied
  a foreign anchor, and any fleet with two retrying members declared a split
  and left its slots. The drift is now taken modulo the window. Nothing is
  lost: two anchors exactly a window apart put every slot on its own twin,
  which collides with nobody. Every honest-fleet test now asserts no split.
* **A member's own sequences were not in its replay window**, so a replay of
  its own earlier frame reached the state machine before being refused.
* **A replay aligned with the schedule is jamming.** A replayer repeating
  every half window hit the same two slots every window and silenced them
  through every retry. The replay test now drifts its period; the aligned case
  is the jam test's.

### What this does not cover

Two adversaries at once, which the budget permits. An adversary that is also
the majority of a partition. Evidence is still only kept locally. A compromised
member can send a repair request with an empty list and cost every member one
resend per round — bounded, and noted.

---

## D11 — A member killed inside a round comes back and finishes it

**Decided and measured 2026-09-19.** The refloat test (D7) killed a vessel
*after* the round and checked it did not vote again. This one kills it in the
middle — after it has voted and heard the members before it in slot order — and
brings it back while the round is still going. That is the sharpest test the
durable journal can be given, and it failed four separate ways before it
passed. Each was a real defect.

### Witnessed votes are journalled

A tally kept only in memory dies with the process. The members a restarted
vessel had already heard are done transmitting, and it needs them most. Every
binding vote admitted from another member is now written to the journal **as
the frame it arrived in**, so the restarted vessel re-verifies the signature
rather than trusting its earlier self. One per author. Compaction keeps them.
On start they enter the same gate as votes that arrive too early: held until
consultation closes, then offered to the state machine.

### Four defects the test found, in the order it found them

1. **The emulator stopped listening once the fleet was complete.** A refloated
   vessel spent thirty seconds failing to connect and died — while the earlier
   refloat test saw its start line, logged *before* the connection, and passed.
   The emulator now accepts for as long as it runs; a reconnecting member
   replaces its own dead socket.
2. **Joining late closed consultation directly and skipped the drain.** The
   restored votes sat in the held-vote buffer forever. Joining now leaves the
   close to the main loop, which drains everything held.
3. **A vessel's own vote was admitted only on "attempt one".** A member that
   missed its slot had already spent an attempt before it first sent, and a
   member back from a restart may never send again at all — the fleet had
   acknowledged it — so it was the one member missing from its own tally. The
   own vote is now admitted on the fact of first sending, and a recovered vote
   is restored into the held buffer at start like any other.
4. **Repair only asked after members it had heard since restarting.** The
   member it needed had voted before the restart and never spoke again. A
   repair request carries what is *held*, so asking after every manifest member
   costs no bytes and an absent member simply sends no reply. Repair now asks
   about everyone.

### Measured

Killed after casting its vote, refloated into the live round: recovered its
vote, restored two witnessed votes, derived the anchor from the next frame it
heard, closed, admitted the three it held in one tick, saw itself acknowledged
and did not retransmit, asked for the one member it had never heard, received
it in the first repair round — **five of five, endorsed**. The rest of the fleet
unaffected.

### What the test does not do

It kills one vessel once, at one point in the round. Killing during the journal
append itself is covered at the journal level by the every-offset truncation
test, not end to end. Two vessels restarting at once, or one restarting twice,
are not tried.

---

## D12 — The node meters its own airtime

**Decided and measured 2026-09-19.**

The simulator has refused transmissions over the 1 %-per-hour duty cycle since
the radio model was built (D2). The node never did: it transmitted whatever the
schedule asked for, and after D8, D10 and D11 it asks for more than it ever did
— retries, repair requests, resends on request. That is exactly the traffic that
pushes a real transmitter over its legal budget.

`AirtimeBudget` now lives in the application layer, where a transmitter's rules
belong; the simulator re-exports the same constant and the same check so a
scenario and a node are metered by one rule. It is a sliding window over
protocol time — a node may deliberate again next hour — and every one of the
node's seven transmit paths goes through it. A refused frame is logged with the
airtime it would have cost and dropped, never queued: by the time the budget
frees up, the slot it was meant for is long gone. The attempt is spent all the
same, so a refusal does not buy a second try at the same slot.

Measured: a member with thirty-five of its thirty-six seconds already gone
before the round refused all six transmissions it was asked for — three stage
frames and three resends other members requested of it — and put nothing on
the air, while the other four reached their threshold without it.

One correction on the way: the send loop logged `sent` whether or not the frame
went out. It now says so only when it did.

---

## D13 — The state machine against a reference model

**Recorded 2026-09-19.** `tests/state_model.rs`.

The unit tests each pin one rule. This drives `Case` through random histories —
utterances from six members at every stage and verdict, closings, clock
readings drawn to straddle every boundary the rules name and free to jump
backwards, since the API accepts any `Clock` — and compares **every result** to
a model written from the rules as documented: expiry first, one binding vote
per author, support alone counts, consultation frozen once, a binding vote
refused while validity is uncertain.

Five hundred and twelve histories of up to forty steps agreed on the first run.
That says two things: the machine does what its documentation says, and the
documentation states every rule the machine applies. A second property test
states the safety property without the model — however the history goes, the
supporters are distinct members each of whom had a supporting binding vote
admitted, and no dissenter is among them — so the claim does not rest on the
model being right.

---

## D14 — The emulator decides reception per receiver, and the suite's flake rate is measured

**Decided and measured 2026-09-19.**

### Reception is per receiver

The channel emulator used to hold one frame at a time and destroy any two that
overlapped, for everybody. That is the right model for a loopback where every
pair is equally loud, and it made the propagation model in `sim::phy` --
two-ray path loss, sensitivity, capture -- something only the single-process
simulator ever exercised.

`lcq-hub --spacing-m` strings the members out in a line and the emulator now
settles each frame **when it ends, per receiver**: below sensitivity it is too
weak; overlapped by a frame it is not six decibels stronger than, it is
collided; otherwise it is delivered, minus the configured residual loss. Without
geometry every pair is equally loud, nothing can capture over anything and
every overlap destroys both frames -- exactly the old behaviour, which the
baseline and split tests confirm unchanged.

Measured: five vessels eight kilometres apart, so the two ends are thirty-two
kilometres from each other and past the radio horizon for these masts. Fourteen
frames fell below sensitivity, all between the two ends. The tally came out
**[4, 5, 5, 5, 4]**: each end is deaf to the other, the middle hears everyone,
everyone reaches the threshold of four. A partial partition drawn by physics
rather than by a flag -- and one that repair cannot mend, however many rounds,
because a resend to a receiver past the horizon is as inaudible as the first.

### Flake rate

Timing-based tests deserve a number. The full container suite -- eighteen
scenarios at the time, including skewed clocks, isolated halves, thirty per
cent loss, six adversaries and two restarts -- was run three times in a row
with nothing else on the machine: **54 of 54 passed, zero collisions**.

The one failure seen earlier that day, a twelve-vessel run where one member
counted eleven, happened while a build was running alongside. D7 already
records that contention for a core is what destroys a slot schedule, and it
did: the run took 26 s where an idle machine takes 22. It was the host, not the
protocol, and the way to know that was to measure in isolation.

### What the emulator still is not

Loopback with arithmetic on top. Path loss is the textbook two-ray model over
assumed geometry; there is no fading in the emulator (the single-process
simulator has Rician fading, the emulator is kept deterministic so a failure
replays), no sea state, no mast sway, no traffic from outside the fleet. It
distinguishes a plausible link from a hopeless one. Nothing here supports a
claim about real maritime range.

---

## D15 — Members carry each other's votes: the relaying D2 promised

**Decided and measured 2026-09-19.**

D2 settled the spreading factor by putting longer range on relaying: two SF10
hops cost less airtime than one SF12 hop and reach further. The node never
relayed anything. `RadioQueue` — bounded, priority with a guaranteed share for
routine traffic, de-duplicating — sat in the application layer with nothing
running it, and D14's geometry test showed the exact scenario it was for: the
two ends of a line deaf to each other, the middle hearing both.

### How a vote gets carried

Only on request, never speculatively — every frame is airtime. When a repair
request arrives (D10) from a member R that can hear us (our bit is set in what R
holds), and R lacks a member X whose binding vote we hold — verified when it
arrived, journalled as the frame itself (D11) — that frame is queued to be
carried, **unchanged**: same signature, same seal, same cleartext header. In
our own slot of the resend window we send one frame: our own vote if R lacked
that, otherwise the next carried one. Every other holder queues the same frame
and sends in *its* own slot, so copies never collide, and R drops each copy
after the first as a replay of a sequence it now holds.

The queue's identity is one number and members count sequences from zero, so
carried frames are keyed by author and sequence together. Sixty-four frames
deep, one per slot per round, three rounds: a middle member carries at most
three votes per deliberation, and nothing it carries can displace its own vote.

### Measured

Five vessels eight kilometres apart, the ends past the radio horizon from each
other. Eleven frames fell below sensitivity between the ends; six were carried
by the middle; the tally is **[5, 5, 5, 5, 5]**. Before this change it was
[4, 5, 5, 5, 4] — every member at or above the threshold either way, but with
relaying the fleet also *agrees on the count*, which is the stronger property.

### What relaying does not do

It cannot help a member nobody can hear. It does not extend a signed validity —
a carried frame is the original frame and expires when it does. It adds airtime
exactly where a request shows it is needed and nowhere else. And a compromised
member can request repairs it does not need and cost each holder one carried
frame per round: bounded, and the same class of cost as an empty-list request
(D10).

**Amended 2026-09-19.** A carried vote arrives in the carrier's slot, not its
author's, so the timing-based split check of D9 read two carried votes as two
foreign anchors and declared a split in an honest lossy fleet (found by the
container suite on the virtual chip once forwards could repeat every round).
The timing check now runs only while the schedule is running; in repair rounds
frames are expected off their author's slot, and a split there still shows by
its label.

---

## D16 — One radio seam; the first radio after the hub is a virtual SX1262 under the unmodified `lora-phy` driver

**Decided 2026-09-19**, on the research in `RESEARCH-lora-module-emulation.md`
(own sources plus Hermes's consultation, which reached the same ranking).

The question was whether the node can be made to talk to a *virtual LoRa
module* the way it talks to a real one, so that the binary the container suite
exercises is the binary that goes to sea. It can, and nobody ships it: the
closest existing pieces are Meshtastic's `SimRadio` (a loopback to a TCP medium,
the shape `lcq-hub` already has) and a clockless test emulator of the SX126x
command set inside `lora-rs/lora-rs`.

### What was decided

1. **The node talks to a radio through one small seam** — construct with a PHY
   profile, `transmit(bytes)`, `poll()` for received frames with RSSI/SNR and
   for CRC failures. Today's hub socket (`Link`) becomes the first adapter. The
   protocol, the journal and the schedule do not learn which adapter is behind
   it. The 27 methods of a chip driver's `RadioKind` stay behind the seam.
2. **Module emulation means a timed behavioural model of the SX126x LoRa
   command subset**, driven by the *unmodified* `lora-phy` `Sx126x` driver
   through a virtual SPI device and virtual BUSY/DIO1/reset lines, attached to
   `lcq-hub` as its medium. The model owns a clock: TX occupies the airtime the
   modulation and packet parameters imply and raises `TxDone` at its end; the
   chip is deaf outside RX; RX timeouts run; a frame is received only if the
   chip was in RX for its whole duration; packet status carries the medium's
   RSSI/SNR; a collided frame arrives as a CRC error. Register-for-register
   fidelity is *not* the goal — behaviour the datasheet specifies and the hub
   can time is.
3. **Hardware is the gate, not another emulator**: two SX1262 boards first,
   five later (roadmap M8) — RNode firmware over USB for a zero-firmware start,
   an SPI HAT on a Linux SBC for the deployment shape. Only the virtual SPI and
   GPIO are swapped for `linux-embedded-hal`; driver, seam and protocol stay.
4. **`gr-lora_sdr` is a calibration lab only** — a separate GPL process that
   produces FER-vs-SNR and capture-vs-(power, offset) tables for the hub and the
   chip model. Never in CI, never linked into the crate.

### What it rules out

- Renode, QEMU and Wokwi as the emulation vehicle: none has a Semtech LoRa
  model (code search across `renode/renode-infrastructure` and `qemu/qemu`:
  zero hits), and each would demand the same modem state machine again, plus an
  MCU we do not target.
- ns-3 `lorawan`, ELoRa, FLoRa and LoRaSim as a runtime the node attaches to:
  they are LoRaWAN-shaped and cannot run the node binary. They remain useful
  as independent models to compare the hub's rules against, and LoRaSim's
  preamble-relative capture rule is worth porting into the hub.
- A LoRa driver of our own for the virtual chip or the hardware. `lora-phy`
  checks its SPI stream against Semtech's reference driver; a rewrite would
  discard that.
- Exposing the driver API to the protocol, or letting the protocol block on
  the driver: the radio runs on its own thread behind the seam.

### What would justify revisiting

- LCQ becoming `no_std` firmware on an MCU — then Renode earns its keep for
  reset/GPIO/SPI/IRQ paths and the seam moves down a layer.
- `lora-phy` stalling (last upstream push 2026-08-27) — the fallback is
  `radio-sx126x`-style blocking drivers behind the *same* seam, not a new seam.
- A chosen module outside the SX126x/SX127x families (LR11xx, SX128x): the
  chip model is per family; the seam is not.

### Implemented 2026-09-19

Built: `application::radio` (the seam), `infrastructure::hub` (the delivery
format and the socket both adapters share), `infrastructure::hub_radio`,
`infrastructure::sx126x` (`chip` model, `bus`, `executor`, `radio`), the
`--radio` option on `lcq-node`, `LCQ_RADIO` in the container harness, a bench
suite in `tests/sx126x.rs`, and the hub delivering collided frames as CRC
failures. Dependencies added: `lora-phy` and `lora-modulation` from
`lora-rs/lora-rs` (git, MIT), `embedded-hal`, `embedded-hal-async`.

What the chip model asserts, and what it leaves out:

- Opcodes are the datasheet's (DS.SX1261-2 table 11-1), not the driver's enum,
  so the model does not lean on the crate it exercises. Read-only commands
  answer with the status byte first, as the chip does; buffer and register
  reads answer without one, as the driver reads them past a NOP.
- Time: TX lasts `lora-modulation`'s time on air for the chip's own modulation
  and packet parameters -- the same AN1200.13 formula `sim::channel::airtime_ms`
  uses, so hub and chip agree on when a frame ends. Timed receives run in
  15.625 µs ticks. Both are divided by the test's time scale.
- Deafness: a frame is received only if the chip was in receive mode before
  the frame's preamble had fewer than six symbols left -- a model parameter,
  conservative against LoRaSim's five, so two symbols of slack on the
  eight-symbol profile (floor 5 ms wall time, for tests that compress time a
  hundredfold). Boundary tests at 1.5 and 2.5 symbols for eight, 5.5 and 6.5
  for twelve. TX, standby and sleep hear nothing,
  and say so as `chip_missed` notes in the node's log.
- Not modelled: capture timing (still the hub's power-only rule), CAD against
  real channel activity (the hub does not yet expose frames in flight, so CAD
  answers "clear"), register semantics, BUSY beyond a fixed 100 µs.

Measured: baseline fleet 5/5 on both radios, no frame missed by any chip; nine
bench tests under the real driver; 20 of 20 tests in 638 s, no container left behind, geometry tally still [5, 5, 5, 5, 5] with relaying through the virtual chips.

---

## D17 — Replay detection is a per-sender window, not "newest wins"

**Decided and implemented 2026-09-19.** `application::replay::ReplayWindow`,
`REPLAY_WINDOW = 64`; tests in `tests/replay.rs`.

The first rule was: any frame from a sender at or below the highest sequence
already seen from it is a replay. Correct against the replay adversary, and
wrong for a genuine older frame carried late -- the case D15 created: a vote
relayed by a neighbour after its author's later frame (a NACK, a resend) has
already been heard. Under "newest wins" the relayed vote is dropped by the one
member who needed it.

The window keeps one bit per recent sequence, sixty-four deep, per sender: a
sequence above the highest advances the window; one inside it is admitted if
its bit is clear and refused if set; one older than the window is refused
because nothing that old can be told apart any more. Memory per sender is
fixed whatever the traffic. This is the IPsec anti-replay shape, and it keeps
D10's property -- no receiver pays a signature check for a frame it already
had -- while letting a late genuine frame through exactly once.

Rules out: unbounded per-sender history, and any rule that lets a frame be
admitted twice. Revisit if a sender can legitimately have more than sixty-four
frames in flight behind its newest, which no stage of this protocol comes near.

---

## D18 — One frozen PHY profile, and a node that reports what it spent

**Decided and implemented 2026-09-19**, from Hermes's review of D16.

`PhyProfile::eu868_sf10()` is versioned (`eu868-sf10-v1`) and complete:
spreading factor, bandwidth, coding rate, preamble, header mode, CRC, IQ,
low-data-rate optimisation, frequency, power, sync word, receive mode, CAD
parameters, the preamble a lock needs, the capture threshold. Every process
logs its name; the hub's spreading factor, carrier, power and capture
threshold are pinned to it by `tests/phy_profile.rs`, which also checks that
the emulator's airtime rule and the chip driver's time-on-air agree to the
millisecond across payload sizes. The driver is configured from the profile,
field by field, and refuses a profile whose low-data-rate flag disagrees with
the chip's own rule. An emulator run and a hardware run are compared on this
profile or not at all.

The node's final report carries meters: signature checks paid for, CRC
failures heard, frames its chip could not hear, replays dropped before any
check, airtime spent, evidence frames kept, whether it saw a split. The
container harness reads them, and the virtual-fleet test asserts that no chip
missed a frame and no CRC failure occurred on a clean channel. Own-vote
resends in a repair round now ride `RadioQueue` in the distress class, ahead
of carried votes in the routine class -- the queue decides the order, not an
ad hoc flag -- which is the first use of the class M5 reserved. The queue's dedup memory would have refused the same
frame in the next repair round; a repair request now forgets it first, because
being asked again means the frame is still lacking.

---

## D19 — The medium is judged in time: no lock, header lost, payload lost

**Decided and implemented 2026-09-19**, from Hermes's review of D16.

D14's rule was that any overlap in time is a collision unless one frame is
six decibels stronger, whenever the other arrives. A receiver does not work
like that. It locks onto a frame during the last symbols of the preamble;
before that window an interferer that has already ended never mattered, and
after it an interferer cannot take the lock away -- it corrupts what is being
received. `sim::medium::judge` now tells five outcomes apart, and the hub
delivers accordingly:

- **too weak** — below sensitivity, nothing heard;
- **no lock** — an interferer not enough weaker was on the air during the
  symbols a lock needs (the profile's `preamble_symbols_to_lock`, ending with
  the sync word): the receiver never had the frame, and the hub sends nothing;
- **header error** — locked, then an interferer arrived over the header's
  eight symbols: the chip raises `HeaderErr` and no `RxDone`; the hub delivers
  an empty frame with that verdict; the chip model raises exactly that IRQ
  and the node meters it as `header_errors`;
- **CRC error** — locked, then an interferer arrived over the payload: the
  bytes are noise, delivered with the verdict `CrcError`, raised as
  `RxDone | CrcErr`;
- **decoded**.

A frame stronger than an interferer by the profile's capture threshold is
decoded whenever the interferer arrives; a receiver's own transmission is an
interferer of infinite strength, which is how half-duplex falls out of the
same rule. This is `LoRaSim`'s timing rule (the last five preamble symbols;
here six, conservative) with the header told apart from the payload. The
thresholds are the profile's, not truths about a chip: the hardware matrix
in the handoff -- power difference by arrival offset by overlap length --
calibrates them.

Rules out: a capture rule without a clock, and a collision that is silent to
the receiver when a real chip would have raised an IRQ. Revisit when hardware
measurements say the lock window or the header window is elsewhere.

---

## D20 — A frame names its case by an eight-byte reference; the signature still covers the hash

**Decided and implemented 2026-09-19.** D4 ranked this first among the airtime
savings: the 32-byte content hash was 30 % of every frame, and every member
already holds the case it votes on.

On the wire a `CompactEnvelope` carries `case`, the first eight bytes of a
domain-separated Blake2s of the content hash (`wire::case_reference`). The
signature transcript covers the *full* hash, which the wire does not carry:
the receiver supplies the one it holds for the case it is deliberating, and a
frame signed over any other case fails to verify -- the reference is a lookup
key, not a commitment, and picking the wrong case costs a failed signature
check, never a wrong count. `MAX_FRAME_BYTES` is 152, twenty-four bytes down;
at SF10 that is about 197 ms off every frame and the slot shrinks with it.

Rules out: carrying the hash on the wire again, and any receiver that counts a
frame without a case of its own to verify it against. Revisit if a fleet ever
deliberates several cases at once with references that collide -- eight bytes
make that a design fault, not a chance.

---

## D21 — Two ways onto real hardware, behind the same seam

**Decided and implemented 2026-09-19**, untested against boards until there
are boards (roadmap M8).

- **RNode over a serial port** (`--radio rnode:/dev/ttyACM0`): the host
  protocol of RNode firmware -- KISS frames, a command byte at the head,
  configuration echoed as confirmation, `RADIO_STATE` echoed as `1` once the
  radio is on, an RSSI/SNR/data triplet per received packet -- implemented
  sans I/O in `infrastructure::rnode::RNodeLink` and pinned by tests against
  a fake RNode on a pseudo-terminal pair. Nothing to flash but a released
  firmware; the cheapest first contact with the air.
- **An SX1262 on SPI and GPIO** (`--radio spi:/dev/spidev0.0,/dev/gpiochip0,busy=24,dio1=16,reset=18,tcxo=1.8`):
  the same `lora-phy` driver thread as the virtual chip, with `spidev` and
  the GPIO character device for a bus (`infrastructure::sx126x::linux`) and
  the air for a medium (`sx126x::hardware`). The driver thread is generic
  over the bus (`Watch`, `DriverHandle`); only the bus is swapped, as D16
  promised. Without the virtual chip's view of the IRQ status a CRC-failed
  frame reaches the node as a reception and the seal rejects it; the
  upstream change that surfaces CRC and header errors closes that.

Both live behind the `hardware` cargo feature (default on); the container
suite builds them into the static binaries and never exercises them. What
would justify revisiting: the first board. Every timing in the Linux bus --
BUSY timeout, DIO1 polling, reset pulse -- is a datasheet figure or a guess,
and Hermes's measurement matrix (handoff) is where they get replaced.

---

## D22 — What the security audit changed, and what it left to the product

**Decided and implemented 2026-09-19.** The audit is `THREAT-MODEL.md`; this
records the decisions it forced.

- **A journal that feeds nonces never starts from zero.** `LogJournal::open`
  counts from zero, for tools and tests; `open_with_entropy`, which the node
  uses, starts a fresh log at a random sequence in [2^24, 2^31) and persists
  it before anything else. A member whose journal is lost -- a replaced
  device, a reflashed card -- would otherwise seal its next frames under
  nonces its earlier life used, with the same group key and author index:
  keystream reuse, and the Poly1305 key with it. The ceiling keeps every
  sequence under the 32-bit round label. Residual risk ~2^-31 per frame per
  pair of lives; binding the group key to the mission epoch (provisioning,
  below) is the complete answer.
- **Replay windows are seeded from the journal at start**: every witnessed
  vote and every own frame the journal holds is marked seen, so a restart does
  not reopen the window for them. Frames the journal does not hold (triggers,
  requests) can be replayed once per restart at the cost of one signature
  check; the state machine's one-vote-per-member rule stands regardless.
- **Adversary modes live behind the `harness` cargo feature**, off by
  default. A production build started with `--adversary` refuses to run. The
  container harness builds with the feature on.
- **CI actions are pinned by commit**, and the journal file is created 0600.

Left to the product, not the protocol: key and manifest provisioning
(constants in the harness binary today), group-key rotation and member
exclusion (evidence is kept, nothing acts on it), and a per-sender rate before
signature verification. The threat model ranks them.

## D23 — Real boards are reached through a bridge: the board is the wires, the driver stays on the host

**Decided and implemented 2026-09-19; the first boards arrive 2026-09-22.**
Three Seeed XIAO ESP32-S3 + Wio-SX1262 kits (the B2B-connector version) are
the first hardware. Two roads reached them without new firmware: `RNode`
firmware, which supports the kit since 1.82 and which the D21 KISS adapter
already drives, or a Linux single-board computer over `spidev`, which the kit
is not. `RNode` was measured against its source (1.86): every transmission
goes through its CSMA -- wait for a free medium, a DIFS of two slots of
twelve symbols (197 ms at SF10), then a contention window of 0 to 13 slots on
a quiet channel and 15 to 29 above 7 % airtime, that is 0.2 to 1.5 s of
random delay per frame and up to 3 s under load -- and the preamble is forced
to at least 18 symbols, with a one-byte header on the air. A modem that
decides when to transmit cannot carry a protocol whose slots are anchored on
the trigger: the timing split check would take every late frame for a split,
and any guard measured through it would be `RNode`'s, not the chip's.

So the board is made into wires. The bridge firmware (`firmware/bridge`)
exposes SPI transactions, BUSY, DIO1, NRESET and the receive-side antenna
switch over a KISS-framed request/response protocol on USB CDC, and the
unmodified `lora-phy` driver runs on the host through `BridgeSpi`, `BridgeIv`
and `BridgeWatch` -- the third implementation of the seam that already
carries the virtual chip and the Linux bus. What this buys: the chip's own
timing; `GetIrqStatus` readable between the driver's transactions, so a CRC
failure is seen on real hardware without waiting for upstream #487; CAD; and
one code path from the container fleets to the bench. What it costs: a USB
round trip per SPI transaction (well under a millisecond, against frames of
a second) and DIO1 as an event over the same pipe, with the level re-read
every 100 ms during a wait in case an edge went missing.

The protocol is specified in `bridge.rs` and implemented twice: by the
firmware and by the reference device on the virtual chip, which the tests
drive through a pseudo-terminal pair. The firmware must match the reference
byte for byte; a difference is a firmware bug. The version is the first byte
of `HELLO`'s reply, and a mismatch refuses to bring the radio up.

The firmware is Arduino C++ rather than Rust: it is three hundred lines of
glue, the board's own core is the toolchain Seeed and `RNode` use, and that
toolchain is several gigabytes, so it is built in GitHub Actions and never on
the workstation. Boards are one file each behind a `Board` interface; an
unknown FQBN is a build error, never a guess at pins. It lives in this
repository while the protocol is young; once frozen on hardware it can move
to its own repository with the host crate, since neither side knows LCQ.

Not decided: whether a vessel node is a host plus a bridge, or the protocol
on the microcontroller itself. The bridge answers the qualification question
either way.

**Amended 2026-09-20, after review.** Three rules the first draft did not
have, all of them about the gap between what the cable says and what the
chip is doing.

- **A DIO1 notice is a hint; the line is the fact.** The firmware sends the
  level it read after the edge, the host drops a notice that says the line
  has fallen, and every wait confirms by reading the line before it wakes the
  driver. Without this a notice that crosses the cable after the driver has
  cleared the IRQ wakes the driver into the path that clears the status
  again, which would discard a reception that arrived in between -- a lost
  frame with no log line. The virtual chip cannot show this because its IRQ
  view is exact and its waits are not carried by a cable.
- **The device names its session, and a change poisons the link.** `HELLO`
  answers with an identifier drawn at every boot (protocol version 2). A host
  that stops getting replies asks again; a different identifier means the
  microcontroller restarted and the chip is no longer the one the driver
  configured, so every later call fails and the radio reports
  `bridge_reset`. It does not re-initialise itself: a node that silently
  recovers hides that it missed a round, which is the one thing hardware
  qualification must not hide.
- **The IRQ deadline comes from the profile.** The only place the driver
  waits on the line is `tx()`, for `TxDone`, so the bound is the time on air
  of the longest frame the profile can carry plus a second for the bridge.
  A fixed twelve seconds was either too slow to notice a dead board or,
  for a slower profile, too short.

Also from the review: the pre-flight is a tool, not a paragraph in a runbook.
`lcq-bridge` walks the rungs -- `HELLO`, reset, BUSY, `GetStatus`,
`GetDeviceErrors`, `GetIrqStatus`, DIO1, the antenna switch, then twenty
round trips for a median and a p95 -- and names the lowest one that failed,
so that a board which will not come up is a firmware, cable, pin-map, power
or driver problem and not all five at once. It runs against the reference
device in the test suite, so the tool is qualified before the boards are.

## D24 — A member's standing is a competence on a fixed scale, and the protocol never learns what it measures

**Decided and implemented 2026-09-20.** The manifest gave each member an
unbounded `u32` weight, and the only written rule for choosing one was
Morsik's: the square root of a language model's parameter count, adjusted by
a quantization coefficient, capped at 3:1. Two things were wrong with that.
It tied the protocol's vocabulary to one kind of member, when the fleet this
is for may just as well hold a vessel that decides by a deterministic
calculation or an instrument reading. And an unbounded number invites a
manifest to say 4 000 000 000, which means nothing to anyone reading it.

So: `Competence`, a validated number from 1 to 100. The manifest carries it,
`Policy` validates it at construction, and the threshold arithmetic is
unchanged -- still `3·support > 2·total` in integers, still alongside the
count threshold. What changed is what the number *is*: a normalised statement
of how much a member's judgment is worth, produced wherever the manifest is
assembled, by whatever means suits that fleet. The square root of a parameter
count is one recipe among several; a benchmark score, a calibration record or
a figure a harbourmaster wrote down all arrive identically. Nothing below the
manifest can tell them apart, which is exactly what lets one fleet mix
members that decide by different means.

**It is not on the wire, and that is the point.** A frame carries the
author's index and its verdict, never its standing. A self-declared
competence is a member voting itself heavier; bound to the manifest instead,
it would be bytes spent on something every member already holds. Competence
is therefore exactly as trustworthy as the manifest, which is `THREAT-MODEL`
F4 and still open.

**The ratio cap and the fixed scale had to be reconciled.** The 3:1 cap is a
property of the whole fleet -- the most competent member may not exceed three
times the least -- so a service that scores members one at a time cannot know
whether its manifest is legal until it assembles it. `Competence::band`
answers that: at a cap of 3 it returns 34 to 100, and any set of scores in
that band satisfies the cap by construction. A service may also score freely
and let `Policy::new` reject the spread, which it does loudly. Both are
supported; the band is the recommendation.

Kept deliberately: the cap does not stop one member at the top of the scale
from blocking the threshold (F12). That remains a property of the design, now
stated in the scale's own terms.

## D25 — A fleet is a file, not a count on the command line

**Decided and implemented 2026-09-20.** D24 gave a member's standing a scale
and a name, and then left it with no way in: the node built its fleet from
`--fleet N`, gave every member a synthetic `n0`, `n1` identity and the same
competence, and took the mission epoch from a constant. A fleet whose members
decide by different means cannot say so through an integer.

So a manifest is a file, and every member reads the same one:

```text
# Three vessels; the second and third defer to the first.
version 1
epoch 7
member 0 99 ship-alpha
member 1 66 ship-bravo
member 2 33 ship-charlie
```

`lcq-node --manifest <path>` takes the fleet's size, its members' names,
their competences and the mission epoch from it; `--fleet N` remains for the
harness and the container suites, where there is no service to normalise
anything and every member counts the same.

**Why a line format and not JSON or TOML.** The crate has no parser for
either, and a manifest is the one file an operator may have to fix on a
vessel with a serial console and no documentation. Three directives, one per
line, `#` for comments, and every complaint names its line number. Adding a
dependency to read six lines would have been the larger decision.

**Indices must cover `0..n` exactly once.** A frame names its author by index
to save bytes on the air (D3), so a gap would leave an index that decodes to
nobody, and a duplicate would leave one that decodes to two. The parser
refuses both, and refuses a fleet larger than the acknowledgement bitmap can
name -- the check the node used to make about `--fleet`, now made where the
fleet is defined.

**A manifest that cannot be a policy is not a manifest.** Parsing ends by
building the `Policy`, so a competence off the scale or a spread past the
ratio cap is refused at the door rather than at the first vote.

**Both thresholds are now reported.** The `final` line carried
`supporters` and the count threshold and said nothing about competence,
which made half the verdict invisible: "four of five voted" and "but not
enough of the fleet's competence" are different situations. It now also
carries `competence` and `total_competence`, and the container and
multiprocess harnesses read them.

**Two epochs, now named apart.** The node already had a `--epoch`: the
instant its clock counts from, nothing to do with the mission epoch a
manifest names. Review flagged the collision as a hazard worth removing
before provisioning builds on either, so the clock's is now `--clock-epoch`
and the field `clock_epoch`, and the mission epoch is the only thing called
an epoch without qualification.

**It found a latent defect.** Two of the three places that build a frame --
the vote and the trigger -- carried the mission epoch, event and revision as
the literals `1, 1, 0` rather than the constants, which was invisible for as
long as the constant was 1. The first fleet with an epoch of 7 refused every
frame as being about another subject. Epoch rotation is exactly what F5 asks
for, so this would have failed silently the first time anyone rotated one.
All three sites now name the fleet's epoch.

**What this is not.** Nothing here is signed, which makes it a development
and harness format. The first draft of this record called it the journal's
trust level; the review was right that this is wrong. A journal is one
member's own safety state, and losing it harms that member. A manifest is the
fleet's root policy, and whoever can write it can change the membership, both
quorum outcomes, the mission epoch and which index means which member --
enough to leave two members with different views of the same fleet. That it
cannot steal a signing key today is an accident of the harness, not a
property. The signing keys are still
derived from an index (`seed_for`), so the fleet this file describes and the
keys that authenticate it are two halves that do not yet meet. Joining them
is F4 and F5 in the threat model: a signed manifest naming each member's
public key, valid for one epoch, a node that refuses to run on a fixture. The
format has room for the key column that will carry it, and the version
directive exists so that adding one is a refusal on an old node rather than a
misparse.
