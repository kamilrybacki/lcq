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
DR0 **application-payload** cap. `lorai` is peer-to-peer raw LoRa, where the PHY
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
