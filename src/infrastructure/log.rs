//! An append-only safety journal on disk.
//!
//! The requirement is not a database. It is an **atomic durable append** over a
//! few dozen records, with no queries, so this is a log: each record is a length
//! prefix, a payload and a CRC, appended and flushed. See `DECISIONS.md` D1 for
//! why `SQLite` was rejected for this role.
//!
//! # What makes it survive a crash
//!
//! * A vote lock and its outgoing frame are **one record**. A crash cannot
//!   separate them, so the journal can never hold a frame whose lock is missing
//!   — which is the exact failure that would let a restarted node vote twice.
//! * Every append is flushed with `sync_all` before the call returns. Not
//!   `sync_data`: appending changes the file length, and the length is metadata.
//!   A caller that gets `Ok` may put the frame on the air.
//! * On creation the **parent directory** is flushed too, or the file's
//!   existence is not durable even once its contents are.
//! * Recovery replays forward and stops at the first record that is short, has a
//!   bad checksum or does not decode, then truncates the file there. A partial
//!   tail is the expected state after a power cut, not corruption.
//!
//! # What it does not defend against
//!
//! Tampering. Anyone who can write to this file can also recompute the
//! checksums. The defence there is that frames are signed and sealed before
//! they reach the journal, and the journal never leaves the node.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::application::{Journal, JournalError, JournalSnapshot, OutgoingFrame};
use crate::domain::contracts::Subject;
use crate::domain::time::Timestamp;
use crate::infrastructure::crc::crc32;
use crate::infrastructure::lock_key::lock_key;

extern crate alloc;

/// Longest record the reader will believe.
///
/// A corrupt length prefix is otherwise an instruction to allocate whatever
/// number happened to be written there. Real records are a few hundred bytes.
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// File size past which the log is rewritten to hold only live state.
///
/// Vote locks are never released and acknowledgements only ever add records, so
/// without this the file grows for the life of the node.
const COMPACT_THRESHOLD_BYTES: u64 = 256 * 1024;

/// One durable fact.
#[derive(Debug, Serialize, Deserialize)]
enum Entry {
    /// A vote lock together with the frame it authorises. Written as one record
    /// precisely so that no crash can produce one without the other.
    Vote {
        key: String,
        sequence: u64,
        bytes: Vec<u8>,
        expires_at: Option<u64>,
    },
    /// The highest sequence number handed out so far, plus one.
    Sequence { next: u64 },
    /// A frame that no longer needs sending: acknowledged, or expired.
    Retired { sequence: u64 },
    /// A lock whose frame is already gone. Written only by compaction, which
    /// rewrites a state that already satisfies the lock-before-frame invariant.
    Lock { key: String },
    /// A pending frame whose lock is recorded separately. Compaction only, for
    /// the same reason.
    Frame {
        sequence: u64,
        bytes: Vec<u8>,
        expires_at: Option<u64>,
    },
}

/// Why a journal could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The file could not be read, created or truncated.
    Io(io::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal could not be opened: {error}"),
        }
    }
}

impl core::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The live state a log replays into.
#[derive(Debug, Default)]
struct State {
    locks: BTreeSet<String>,
    pending: BTreeMap<u64, OutgoingFrame>,
    next_sequence: u64,
}

impl State {
    fn apply(&mut self, entry: Entry) {
        match entry {
            Entry::Vote {
                key,
                sequence,
                bytes,
                expires_at,
            } => {
                self.locks.insert(key);
                self.add_frame(sequence, bytes, expires_at);
            }
            Entry::Lock { key } => {
                self.locks.insert(key);
            }
            Entry::Frame {
                sequence,
                bytes,
                expires_at,
            } => self.add_frame(sequence, bytes, expires_at),
            // The counter only ever moves forward, whatever order records
            // arrive in, so a replayed log can never rewind it.
            Entry::Sequence { next } => self.next_sequence = self.next_sequence.max(next),
            Entry::Retired { sequence } => {
                self.pending.remove(&sequence);
            }
        }
    }

    fn add_frame(&mut self, sequence: u64, bytes: Vec<u8>, expires_at: Option<u64>) {
        let mut frame = OutgoingFrame::new(bytes, sequence);
        if let Some(secs) = expires_at {
            frame = frame.expiring_at(Timestamp::from_secs(secs));
        }
        self.pending.insert(sequence, frame);
        self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
    }
}

/// A journal that survives the process that wrote it.
#[derive(Debug)]
pub struct LogJournal {
    file: File,
    path: PathBuf,
    state: State,
    bytes_on_disk: u64,
    last_io_error: Option<io::Error>,
}

impl LogJournal {
    /// Open the journal at `path`, replaying whatever is already there.
    ///
    /// A truncated or corrupt tail is discarded and the file is cut back to the
    /// last whole record, so opening is also the repair.
    ///
    /// # Errors
    ///
    /// [`OpenError::Io`] if the file cannot be opened, read or truncated. The
    /// node must not proceed: it has no idea what it already decided.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OpenError> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        // The file may have just been created, and a new file's own existence is
        // only durable once its directory entry is flushed.
        sync_parent_dir(&path)?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let (state, valid_len) = replay(&bytes);

        if valid_len as u64 != bytes.len() as u64 {
            // Cut the partial tail off before anything is appended after it,
            // otherwise the next record would sit behind unreadable bytes and be
            // lost at the following restart.
            file.set_len(valid_len as u64)?;
            file.sync_all()?;
        }
        let bytes_on_disk = valid_len as u64;
        // Appends must land at the end, whatever the read above left behind.
        file.seek_end()?;

        Ok(Self {
            file,
            path,
            state,
            bytes_on_disk,
            last_io_error: None,
        })
    }

    /// The last I/O failure, for an operator trying to work out why a node
    /// stopped voting. A [`JournalError::NotDurable`] on its own says the vote
    /// did not happen; this says what the disk did.
    #[must_use]
    pub fn last_io_error(&self) -> Option<&io::Error> {
        self.last_io_error.as_ref()
    }

    /// Bytes the log currently occupies.
    #[must_use]
    pub fn bytes_on_disk(&self) -> u64 {
        self.bytes_on_disk
    }

    /// Rewrite the log so it holds only live state.
    ///
    /// Written to a sibling file, flushed, then renamed over the original: a
    /// crash at any point leaves either the whole old log or the whole new one,
    /// never a half-written mixture.
    ///
    /// # Errors
    ///
    /// [`OpenError::Io`] if the replacement could not be written or renamed. The
    /// original log is untouched in that case.
    pub fn compact(&mut self) -> Result<(), OpenError> {
        let temporary = self.path.with_extension("compacting");
        let mut entries: Vec<Entry> = Vec::new();
        entries.push(Entry::Sequence {
            next: self.state.next_sequence,
        });
        for key in &self.state.locks {
            entries.push(Entry::Lock { key: key.clone() });
        }
        for frame in self.state.pending.values() {
            entries.push(Entry::Frame {
                sequence: frame.sequence(),
                bytes: frame.bytes().to_vec(),
                expires_at: frame.expires_at().map(Timestamp::as_secs),
            });
        }

        let mut written = 0u64;
        {
            let mut fresh = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            for entry in &entries {
                let record = encode(entry)?;
                fresh.write_all(&record)?;
                written += record.len() as u64;
            }
            fresh.sync_all()?;
        }
        std::fs::rename(&temporary, &self.path)?;
        sync_parent_dir(&self.path)?;

        self.file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        self.file.seek_end()?;
        self.bytes_on_disk = written;
        Ok(())
    }

    /// Append one record and flush it, reporting durability rather than I/O.
    fn append(&mut self, entry: &Entry) -> Result<(), JournalError> {
        match self.try_append(entry) {
            Ok(()) => {
                if self.bytes_on_disk > COMPACT_THRESHOLD_BYTES {
                    // A failed compaction is not a failed append: the record is
                    // already durable in the original log.
                    if let Err(OpenError::Io(error)) = self.compact() {
                        self.last_io_error = Some(error);
                    }
                }
                Ok(())
            }
            Err(error) => {
                self.last_io_error = Some(error);
                Err(JournalError::NotDurable)
            }
        }
    }

    fn try_append(&mut self, entry: &Entry) -> io::Result<()> {
        let record = encode(entry)?;
        self.file.write_all(&record)?;
        // sync_all, not sync_data: an append changes the file length, and the
        // length is metadata. Flushing only the data can leave a durable record
        // inside a file that is still officially shorter than it.
        self.file.sync_all()?;
        self.bytes_on_disk += record.len() as u64;
        Ok(())
    }
}

impl Journal for LogJournal {
    fn commit_vote(
        &mut self,
        subject: &Subject,
        voter: &str,
        frame: OutgoingFrame,
    ) -> Result<(), JournalError> {
        let key = lock_key(subject, voter);
        if self.state.locks.contains(&key) {
            return Err(JournalError::AlreadyVoted);
        }
        // Disk first, memory second. If the append fails, nothing here changed,
        // so the caller may retry and the frame never reached the air.
        self.append(&Entry::Vote {
            key: key.clone(),
            sequence: frame.sequence(),
            bytes: frame.bytes().to_vec(),
            expires_at: frame.expires_at().map(Timestamp::as_secs),
        })?;
        self.state.locks.insert(key);
        self.state.next_sequence = self
            .state
            .next_sequence
            .max(frame.sequence().saturating_add(1));
        self.state.pending.insert(frame.sequence(), frame);
        Ok(())
    }

    fn has_voted(&self, subject: &Subject, voter: &str) -> bool {
        self.state.locks.contains(&lock_key(subject, voter))
    }

    fn reserve_sequence(&mut self) -> Result<u64, JournalError> {
        let reserved = self.state.next_sequence;
        let next = reserved.saturating_add(1);
        // Persisted before it is returned. Handing out a number first and
        // recording it afterwards would let a crash in between hand the same
        // nonce out twice.
        self.append(&Entry::Sequence { next })?;
        self.state.next_sequence = next;
        Ok(reserved)
    }

    fn next_sequence(&self) -> u64 {
        self.state.next_sequence
    }

    fn pending(&self) -> impl Iterator<Item = &OutgoingFrame> {
        self.state.pending.values()
    }

    fn acknowledge(&mut self, sequence: u64) -> bool {
        if !self.state.pending.contains_key(&sequence) {
            return false;
        }
        if self.append(&Entry::Retired { sequence }).is_err() {
            // Leave it pending. It will be sent again and acknowledged again,
            // which the contract already requires callers to tolerate — far
            // better than forgetting a frame that was never confirmed gone.
            return false;
        }
        self.state.pending.remove(&sequence);
        true
    }

    fn drop_expired(&mut self, now: Timestamp) {
        let expired: Vec<u64> = self
            .state
            .pending
            .values()
            .filter(|frame| frame.expires_at().is_some_and(|at| at < now))
            .map(OutgoingFrame::sequence)
            .collect();
        for sequence in expired {
            if self.append(&Entry::Retired { sequence }).is_ok() {
                self.state.pending.remove(&sequence);
            }
            // A frame that could not be retired stays pending and is dropped on
            // the next sweep. Dropping it in memory only would resurrect it at
            // the next restart.
        }
    }

    fn snapshot(&self) -> JournalSnapshot {
        JournalSnapshot {
            vote_locks: self.state.locks.iter().map(|k| (k.clone(), 0)).collect(),
            pending: self.state.pending.values().cloned().collect(),
            next_sequence: self.state.next_sequence,
        }
    }
}

/// Frame one entry as `length || payload || checksum`.
fn encode(entry: &Entry) -> io::Result<Vec<u8>> {
    let payload: Vec<u8> = postcard::to_allocvec(entry)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, alloc::format!("{error}")))?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal record exceeds the maximum readable size",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record length does not fit"))?;
    let mut record = Vec::with_capacity(payload.len() + 8);
    record.extend_from_slice(&length.to_le_bytes());
    record.extend_from_slice(&payload);
    record.extend_from_slice(&crc32(&payload).to_le_bytes());
    Ok(record)
}

/// Replay every whole record, returning the state and how many bytes were good.
///
/// Stops at the first record that is short, mis-checksummed or undecodable, and
/// reports the offset so the caller can cut the file there. It does not try to
/// resynchronise past the damage: a log is only meaningful as a prefix, and
/// guessing where the next record starts would invent history.
fn replay(bytes: &[u8]) -> (State, usize) {
    let mut state = State::default();
    let mut at = 0usize;

    while at + 4 <= bytes.len() {
        let length = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let Ok(length) = usize::try_from(length) else {
            break;
        };
        if length > MAX_RECORD_BYTES {
            break;
        }
        let payload_at = at + 4;
        let crc_at = payload_at + length;
        if crc_at + 4 > bytes.len() {
            break;
        }
        let payload = &bytes[payload_at..crc_at];
        let stored = u32::from_le_bytes([
            bytes[crc_at],
            bytes[crc_at + 1],
            bytes[crc_at + 2],
            bytes[crc_at + 3],
        ]);
        if stored != crc32(payload) {
            break;
        }
        let Ok(entry) = postcard::from_bytes::<Entry>(payload) else {
            break;
        };
        state.apply(entry);
        at = crc_at + 4;
    }

    (state, at)
}

/// Flush the directory holding `path`, so the file's own entry is durable.
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let directory = parent.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    File::open(directory)?.sync_all()
}

/// Position a handle at the end, so the next write appends.
trait SeekEnd {
    fn seek_end(&mut self) -> io::Result<()>;
}

impl SeekEnd for File {
    fn seek_end(&mut self) -> io::Result<()> {
        use std::io::Seek;
        self.seek(io::SeekFrom::End(0)).map(|_| ())
    }
}
