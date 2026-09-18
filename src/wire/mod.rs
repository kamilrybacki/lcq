//! Bytes on the air: a compact codec and the cryptography that makes them mean
//! something.
//!
//! Two independent mechanisms, doing two different jobs. **Group encryption**
//! keeps traffic off the air in the clear and proves the sender holds the
//! mission key — it proves membership, never authorship. **Per-member
//! signatures** prove authorship. A member holding the group key must not be
//! able to pass as another member, so the signature is what a quorum counts,
//! and decryption alone is worth nothing to it.
//!
//! The transcript is domain-separated by stage, so an independent opinion
//! cannot be replayed as a binding vote: the same fields at a different stage
//! sign to different bytes.

mod codec;
mod compact;
mod crypto;

pub use codec::{Envelope, SignedEnvelope, decode, encode};
pub use compact::{CompactEnvelope, SignedCompactEnvelope, decode_compact, encode_compact};
pub use crypto::{
    FRAME_HEADER_BYTES, GroupKey, SigningKey, VerifyingKey, WireError, open, open_frame, seal,
    seal_frame,
};
