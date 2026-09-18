# Decisions

Records of choices that a later session must not silently reverse. Each entry
says what was decided, what it rules out, and what would justify revisiting it.

---

## D1 — The durable journal is an append-only log, not SQLite

**Decided 2026-09-18.** Supersedes the SQLite gate in
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
