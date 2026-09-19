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
