//! The fleet manifest as a signed file: who is in the fleet, what each
//! member's judgment is worth, and who says so.
//!
//! Before this, a node's fleet was a count on the command line and every
//! member counted the same, because the binary was a harness and the radio was
//! what was under test. A fleet of members that decide by different means
//! needs to say so (D24), and version 1 of this format said it -- but said it
//! unsigned, which made the fleet's root policy writable by anyone who could
//! write the file (`THREAT-MODEL.md` F21). Version 2 is the answer (D27).
//!
//! ```text
//! version 2
//! epoch 7
//! valid-from 1789000000
//! valid-until 1792000000
//! group-key 3f0a...                    an opaque id, never the secret
//! byzantine-bps 4000
//! max-competence-ratio 3
//! phy-profile eu868-sf10-v1
//! heard-capacity 64
//! member 0 100 8a1c... ship-alpha      index, competence, public key, label
//! member 1 66 42d9... ship-bravo
//! issuer 8VQ4-3JX0-Z9T2-K7MP           whose card this came from; a hint
//! signature 91be...                    over the canonical form, not the text
//! ```
//!
//! Blank lines and everything after a `#` are ignored. Every header directive
//! comes before the first member, and `signature` comes last.
//!
//! # The signature covers bytes, not this text
//!
//! What is signed is a canonical form built *after* parsing: a fixed order, a
//! fixed width for every number, no comments and no whitespace. Signing the
//! text instead would make a manifest's meaning depend on how somebody typed
//! it, and would let two files with the same fleet in them disagree about
//! whether they are the same fleet.
//!
//! The manifest's **identity is that form's digest**, computed rather than
//! declared. Nothing has to be typed for it, nothing can collide with it by
//! accident, and a replay window can be scoped by it (F2).
//!
//! # Who the signature is checked against
//!
//! The administration key, which reaches a vessel out of band and is entered
//! by hand (D26, [`crate::wire::hand`]). The `issuer` line carries only a
//! fingerprint, so an operator holding several cards can tell which one this
//! manifest wants. It is a diagnostic and never the trust decision: the key
//! the node actually checks against is the one in its own key file.
//!
//! # What refuses a manifest
//!
//! Structure first, before anything is canonicalized, so that an attacker
//! cannot pick which of two readings a parser takes: a repeated directive, a
//! repeated index, a repeated public key, a repeated label, indices that do
//! not cover `0..n`, a label outside the allowed characters, a number with a
//! sign or a leading zero, a profile or a capacity that is not this build's.
//!
//! Then the signature, and then the validity window. A manifest that does not
//! verify is not a weaker manifest; it is not one.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use blake2::{Blake2s256, Digest};

use crate::application::radio::PhyProfile;
use crate::domain::quorum::{Policy, PolicyError};
use crate::wire::{Heard, SigningKey, VerifyingKey, hand};

extern crate alloc;

/// The manifest format this parser understands.
///
/// Version 1 was the unsigned harness format and is gone: a node that reads a
/// fleet from a file now checks who wrote it. An old build refuses a version 2
/// file because it does not recognise the version, which is the behaviour the
/// directive exists for.
pub const MANIFEST_VERSION: u32 = 2;

/// What the signature is over, tagged so that these bytes cannot be mistaken
/// for any other signed thing in this protocol.
const CANONICAL_DOMAIN: &[u8] = b"lcq-manifest-v2";

/// The version as the canonical form carries it.
const VERSION_BYTE: u8 = 2;

/// The longest an operator label may be.
pub const MAX_LABEL_BYTES: usize = 32;

/// Why a manifest file is not a fleet.
///
/// Every variant that can name a line does: an operator fixing a manifest on a
/// vessel deserves to be told where to look, not that "the manifest is
/// invalid".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A word at the start of a line that means nothing here.
    UnknownDirective {
        /// Line number, counting from one.
        line: usize,
        /// The word.
        word: String,
    },
    /// A directive with the wrong number of words.
    Malformed {
        /// Line number, counting from one.
        line: usize,
        /// What the line should have looked like.
        expected: &'static str,
    },
    /// A number that is not one.
    NotANumber {
        /// Line number, counting from one.
        line: usize,
        /// Which field.
        field: &'static str,
    },
    /// A number written a second way: with a sign, or with a leading zero.
    ///
    /// One value has to have one spelling, or a signature over it means less
    /// than it looks like it means.
    NotCanonicalNumber {
        /// Line number, counting from one.
        line: usize,
        /// Which field.
        field: &'static str,
    },
    /// A field that should be hexadecimal, and is not, or is the wrong length.
    NotHex {
        /// Line number, counting from one.
        line: usize,
        /// Which field.
        field: &'static str,
        /// How many bytes it should decode to.
        bytes: usize,
    },
    /// A required directive that never appeared.
    MissingDirective {
        /// Which one.
        directive: &'static str,
    },
    /// A version this parser does not understand.
    UnsupportedVersion {
        /// Line number, counting from one.
        line: usize,
        /// What it asked for.
        version: u32,
    },
    /// A directive that must appear once appeared twice.
    Repeated {
        /// Line number, counting from one.
        line: usize,
        /// Which directive.
        directive: &'static str,
    },
    /// A header directive after the first member, or a member after the
    /// signature.
    OutOfOrder {
        /// Line number, counting from one.
        line: usize,
        /// Which directive.
        directive: &'static str,
    },
    /// A label with characters an operator log cannot be trusted to render, or
    /// that a canonical form cannot pin.
    LabelCharset {
        /// Line number, counting from one.
        line: usize,
        /// The label.
        id: String,
    },
    /// A label longer than [`MAX_LABEL_BYTES`].
    LabelTooLong {
        /// Line number, counting from one.
        line: usize,
        /// How long it is.
        found: usize,
    },
    /// Thirty-two bytes that are not a public key.
    NotAKey {
        /// Line number, counting from one.
        line: usize,
        /// Which index claimed them.
        index: usize,
    },
    /// Two members with the same index.
    DuplicateIndex {
        /// Line number, counting from one.
        line: usize,
        /// The index.
        index: usize,
    },
    /// Two members with the same label.
    DuplicateId {
        /// Line number, counting from one.
        line: usize,
        /// The label.
        id: String,
    },
    /// Two members with the same public key: one principal, two seats.
    DuplicateKey {
        /// Line number, counting from one.
        line: usize,
        /// The index that repeated it.
        index: usize,
    },
    /// A manifest with no members at all.
    NoMembers,
    /// Indices that do not cover `0..n`.
    IndicesNotContiguous {
        /// The index the manifest never names.
        missing: usize,
        /// How many members it has.
        size: usize,
    },
    /// More members than a frame's acknowledgement bitmap can name.
    FleetTooLarge {
        /// How many the manifest has.
        size: usize,
        /// How many a frame can acknowledge.
        max: usize,
    },
    /// A profile this build does not fly, so this fleet cannot hear this node.
    PhyProfileMismatch {
        /// Line number, counting from one.
        line: usize,
        /// What the manifest asked for.
        found: String,
        /// What this build has.
        expected: &'static str,
    },
    /// A capacity that is not this build's, so the two disagree about what a
    /// frame can say.
    HeardCapacityMismatch {
        /// Line number, counting from one.
        line: usize,
        /// What the manifest asked for.
        found: usize,
        /// What this build has.
        expected: usize,
    },
    /// A validity window that closes before it opens.
    ValidityInverted {
        /// When it opens.
        from: u64,
        /// When it closes.
        until: u64,
    },
    /// The signature does not check out against the key it was offered.
    BadSignature,
    /// A manifest whose window has not opened yet.
    NotYetValid {
        /// When it opens.
        from: u64,
        /// What the clock says.
        now: u64,
    },
    /// A manifest whose window has closed.
    Expired {
        /// When it closed.
        until: u64,
        /// What the clock says.
        now: u64,
    },
    /// The members are a fleet, but not a lawful one.
    Policy(PolicyError),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDirective { line, word } => {
                write!(f, "line {line}: {word:?} is not a manifest directive")
            }
            Self::Malformed { line, expected } => write!(f, "line {line}: expected {expected}"),
            Self::NotANumber { line, field } => {
                write!(f, "line {line}: {field} is not a number in range")
            }
            Self::NotCanonicalNumber { line, field } => write!(
                f,
                "line {line}: {field} must be plain decimal, without a sign or a leading zero"
            ),
            Self::NotHex { line, field, bytes } => {
                write!(
                    f,
                    "line {line}: {field} must be {bytes} bytes of hexadecimal"
                )
            }
            Self::MissingDirective { directive } => {
                write!(f, "no {directive} line")
            }
            Self::UnsupportedVersion { line, version } => write!(
                f,
                "line {line}: manifest version {version}, this build understands {MANIFEST_VERSION}"
            ),
            Self::Repeated { line, directive } => {
                write!(f, "line {line}: {directive} appears more than once")
            }
            Self::OutOfOrder { line, directive } => write!(
                f,
                "line {line}: {directive} must come before the first member, and the signature last"
            ),
            Self::LabelCharset { line, id } => write!(
                f,
                "line {line}: label {id:?} may hold only letters, digits, a dash and an underscore"
            ),
            Self::LabelTooLong { line, found } => write!(
                f,
                "line {line}: a label of {found} bytes is longer than {MAX_LABEL_BYTES}"
            ),
            Self::NotAKey { line, index } => {
                write!(f, "line {line}: member {index}'s key is not a public key")
            }
            Self::DuplicateIndex { line, index } => {
                write!(f, "line {line}: index {index} is already taken")
            }
            Self::DuplicateId { line, id } => {
                write!(f, "line {line}: member label {id:?} is already taken")
            }
            Self::DuplicateKey { line, index } => write!(
                f,
                "line {line}: member {index} claims a public key another member already has"
            ),
            Self::NoMembers => f.write_str("a manifest with no members is not a fleet"),
            Self::IndicesNotContiguous { missing, size } => write!(
                f,
                "a fleet of {size} must name every index from 0 to {}; {missing} is missing",
                size - 1
            ),
            Self::FleetTooLarge { size, max } => write!(
                f,
                "a fleet of {size} is larger than the {max} a frame can acknowledge"
            ),
            Self::PhyProfileMismatch {
                line,
                found,
                expected,
            } => write!(
                f,
                "line {line}: this fleet flies {found:?}, this build flies {expected:?}"
            ),
            Self::HeardCapacityMismatch {
                line,
                found,
                expected,
            } => write!(
                f,
                "line {line}: this fleet says a frame acknowledges {found}, this build says {expected}"
            ),
            Self::ValidityInverted { from, until } => {
                write!(f, "a window from {from} to {until} closes before it opens")
            }
            Self::BadSignature => {
                f.write_str("the signature does not check out against the administration key")
            }
            Self::NotYetValid { from, now } => {
                write!(f, "this manifest opens at {from}; the clock says {now}")
            }
            Self::Expired { until, now } => {
                write!(f, "this manifest closed at {until}; the clock says {now}")
            }
            Self::Policy(error) => write!(f, "{error}"),
        }
    }
}

impl core::error::Error for ManifestError {}

impl From<PolicyError> for ManifestError {
    fn from(error: PolicyError) -> Self {
        Self::Policy(error)
    }
}

/// One member: the index a frame names it by, the key that proves it wrote
/// one, what its judgment is worth, and what to call it in a log.
///
/// The principal is the index and the key. The label is for an operator, and
/// nothing is ever decided by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    index: u16,
    competence: u8,
    key: VerifyingKey,
    id: String,
}

impl Member {
    /// The index a frame names this member by.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index as usize
    }

    /// The member's competence, on the scale of `domain::quorum::Competence`.
    #[must_use]
    pub const fn competence(&self) -> u8 {
        self.competence
    }

    /// The public key a frame from this member must verify under.
    #[must_use]
    pub const fn key(&self) -> &VerifyingKey {
        &self.key
    }

    /// What to call it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// A fleet, as a signed file says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    epoch: u16,
    valid_from: u64,
    valid_until: u64,
    group_key_id: [u8; 32],
    byzantine_bps: u32,
    max_competence_ratio: u32,
    phy_profile: String,
    heard_capacity: u16,
    members: Vec<Member>,
    issuer: String,
    signature: [u8; 64],
}

impl Manifest {
    /// Read a manifest. Structure only: this does not check the signature.
    ///
    /// Use [`Manifest::verify`] before acting on one. Parsing and verifying
    /// are separate so that a tool can show an operator what a manifest says
    /// while telling them it does not check out.
    ///
    /// # Errors
    ///
    /// [`ManifestError`], which names the line wherever a line can be named.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let parts = Parts::read(text, true)?;
        parts.finish()
    }

    /// The mission epoch every frame from this fleet carries.
    #[must_use]
    pub const fn epoch(&self) -> u16 {
        self.epoch
    }

    /// The first instant this manifest is good for, in seconds.
    #[must_use]
    pub const fn valid_from(&self) -> u64 {
        self.valid_from
    }

    /// The first instant it is not, in seconds: the window is half open, so a
    /// manifest that ends where the next one begins leaves no gap and no
    /// overlap.
    #[must_use]
    pub const fn valid_until(&self) -> u64 {
        self.valid_until
    }

    /// The opaque name of the group key this mission uses.
    ///
    /// Never the key. What this identifies arrives through provisioning, and a
    /// manifest that carried the secret would put it wherever the manifest
    /// goes.
    #[must_use]
    pub const fn group_key_id(&self) -> &[u8; 32] {
        &self.group_key_id
    }

    /// The `PHY` profile name this fleet flies.
    #[must_use]
    pub fn phy_profile(&self) -> &str {
        &self.phy_profile
    }

    /// A fingerprint of the card this manifest came from, for an operator
    /// choosing between cards. Never the trust decision.
    #[must_use]
    pub fn issuer_fingerprint(&self) -> &str {
        &self.issuer
    }

    /// The members, by index.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// How many members the fleet has.
    #[must_use]
    pub fn size(&self) -> usize {
        self.members.len()
    }

    /// What to call the member at `index`.
    #[must_use]
    pub fn id_of(&self, index: usize) -> Option<&str> {
        self.members.get(index).map(Member::id)
    }

    /// The public key the member at `index` signs with.
    #[must_use]
    pub fn key_of(&self, index: usize) -> Option<&VerifyingKey> {
        self.members.get(index).map(Member::key)
    }

    /// This manifest's identity: the digest of the bytes that were signed.
    ///
    /// Computed, not declared, so two manifests are the same one exactly when
    /// they say the same thing.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        Blake2s256::digest(self.canonical_bytes()).into()
    }

    /// The bytes the signature is over.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical(
            self.epoch,
            self.valid_from,
            self.valid_until,
            &self.group_key_id,
            self.byzantine_bps,
            self.max_competence_ratio,
            &self.phy_profile,
            self.heard_capacity,
            &self.members,
        )
    }

    /// Check that this manifest was signed by `admin` and that `now` is inside
    /// its window.
    ///
    /// `now` is in seconds. A manifest that does not verify is not a weaker
    /// manifest, so there is no partial success here: act on `Ok` and on
    /// nothing else.
    ///
    /// # Errors
    ///
    /// [`ManifestError::BadSignature`], [`ManifestError::NotYetValid`] or
    /// [`ManifestError::Expired`].
    pub fn verify(&self, admin: &VerifyingKey, now: u64) -> Result<(), ManifestError> {
        admin
            .verify(&self.canonical_bytes(), &self.signature)
            .map_err(|_| ManifestError::BadSignature)?;
        if now < self.valid_from {
            return Err(ManifestError::NotYetValid {
                from: self.valid_from,
                now,
            });
        }
        if now >= self.valid_until {
            return Err(ManifestError::Expired {
                until: self.valid_until,
                now,
            });
        }
        Ok(())
    }

    /// The quorum policy this fleet implies.
    ///
    /// # Errors
    ///
    /// [`PolicyError`] if the competences do not make a lawful manifest.
    pub fn policy(&self) -> Result<Policy, PolicyError> {
        Policy::with_settings(
            self.members
                .iter()
                .map(|member| (member.id.clone(), member.competence)),
            self.byzantine_bps,
            self.max_competence_ratio,
        )
    }

    /// Sign a manifest that has everything but its `issuer` and `signature`
    /// lines, and return the finished text.
    ///
    /// This is what an administrator's tooling does with the key that never
    /// leaves them. Any `issuer` or `signature` lines already in `text` are
    /// replaced, so re-signing an edited manifest works the way an operator
    /// would expect.
    ///
    /// # Errors
    ///
    /// [`ManifestError`] if what it was handed is not a manifest apart from
    /// the signature.
    pub fn sign_text(text: &str, admin: &SigningKey) -> Result<String, ManifestError> {
        let parts = Parts::read(text, false)?;
        let bytes = parts.canonical()?;
        let signature = admin.sign(&bytes);
        let issuer = hand::fingerprint(&admin.verifying_key().to_bytes());

        let mut out = String::new();
        for raw in text.lines() {
            let directive = raw
                .split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            if directive == "issuer" || directive == "signature" {
                continue;
            }
            out.push_str(raw);
            out.push('\n');
        }
        out.push_str("issuer ");
        out.push_str(&issuer);
        out.push('\n');
        out.push_str("signature ");
        for byte in signature {
            push_hex(&mut out, byte);
        }
        out.push('\n');
        Ok(out)
    }
}

/// The canonical form: one fixed order, one fixed width per field, no
/// comments, no whitespace, no room for two spellings of one fleet.
//
// The two narrowing casts here are safe by checks `settle` has already made:
// a fleet is at most `Heard::CAPACITY` members, which is 64, and a label is at
// most `MAX_LABEL_BYTES`, which is 32. Both are well inside what they narrow
// to, and a manifest that failed either check never reaches this function.
#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation)]
fn canonical(
    epoch: u16,
    valid_from: u64,
    valid_until: u64,
    group_key_id: &[u8; 32],
    byzantine_bps: u32,
    max_competence_ratio: u32,
    phy_profile: &str,
    heard_capacity: u16,
    members: &[Member],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(CANONICAL_DOMAIN);
    out.push(VERSION_BYTE);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&valid_from.to_be_bytes());
    out.extend_from_slice(&valid_until.to_be_bytes());
    out.extend_from_slice(group_key_id);
    out.extend_from_slice(&byzantine_bps.to_be_bytes());
    out.extend_from_slice(&max_competence_ratio.to_be_bytes());
    push_text(&mut out, phy_profile);
    out.extend_from_slice(&heard_capacity.to_be_bytes());
    out.extend_from_slice(&(members.len() as u16).to_be_bytes());
    // The members are sorted by index before this runs, so the order the file
    // happened to list them in cannot change what gets signed.
    for member in members {
        out.extend_from_slice(&member.index.to_be_bytes());
        out.push(member.competence);
        out.extend_from_slice(&member.key.to_bytes());
        push_text(&mut out, &member.id);
    }
    out
}

/// Text in the canonical form: one length byte, then the bytes. Length
/// prefixes rather than separators, so no label can be made to look like the
/// end of one field and the start of another.
#[allow(clippy::cast_possible_truncation)]
fn push_text(out: &mut Vec<u8>, text: &str) {
    // `settle` bounds every label at `MAX_LABEL_BYTES`, and the one profile
    // name this build accepts is shorter still.
    out.push(text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

fn push_hex(out: &mut String, byte: u8) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.push(char::from(DIGITS[usize::from(byte >> 4)]));
    out.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
}

/// What a manifest file said, before any of it is trusted.
struct Parts {
    epoch: Option<u16>,
    valid_from: Option<u64>,
    valid_until: Option<u64>,
    group_key_id: Option<[u8; 32]>,
    byzantine_bps: Option<u32>,
    max_competence_ratio: Option<u32>,
    phy_profile: Option<String>,
    heard_capacity: Option<u16>,
    members: Vec<Member>,
    issuer: Option<String>,
    signature: Option<[u8; 64]>,
}

impl Parts {
    fn read(text: &str, want_signature: bool) -> Result<Self, ManifestError> {
        let mut parts = Self {
            epoch: None,
            valid_from: None,
            valid_until: None,
            group_key_id: None,
            byzantine_bps: None,
            max_competence_ratio: None,
            phy_profile: None,
            heard_capacity: None,
            members: Vec::new(),
            issuer: None,
            signature: None,
        };
        let mut version: Option<u32> = None;

        for (offset, raw) in text.lines().enumerate() {
            let line = offset + 1;
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let mut words = content.split_whitespace();
            let directive = words.next().unwrap_or_default();

            // Order is part of the structure: a header directive after the
            // members, or anything at all after the signature, is a file
            // somebody appended to.
            if parts.signature.is_some() {
                return Err(ManifestError::OutOfOrder {
                    line,
                    directive: "signature",
                });
            }
            if directive != "member" && directive != "issuer" && directive != "signature" {
                if !parts.members.is_empty() {
                    return Err(ManifestError::OutOfOrder {
                        line,
                        directive: "a header directive",
                    });
                }
                if directive != "version" && version.is_none() {
                    return Err(ManifestError::MissingDirective {
                        directive: "version",
                    });
                }
            }

            parts.apply(directive, &mut words, line, &mut version)?;
        }

        if version.is_none() {
            return Err(ManifestError::MissingDirective {
                directive: "version",
            });
        }
        if want_signature && parts.signature.is_none() {
            return Err(ManifestError::MissingDirective {
                directive: "signature",
            });
        }
        Ok(parts)
    }

    /// One directive.
    fn apply(
        &mut self,
        directive: &str,
        words: &mut core::str::SplitWhitespace<'_>,
        line: usize,
        version: &mut Option<u32>,
    ) -> Result<(), ManifestError> {
        match directive {
            "version" => {
                once(version.is_some(), line, "version")?;
                let value = number(words, line, "version")?;
                if value != MANIFEST_VERSION {
                    return Err(ManifestError::UnsupportedVersion {
                        line,
                        version: value,
                    });
                }
                *version = Some(value);
            }
            "epoch" => {
                once(self.epoch.is_some(), line, "epoch")?;
                self.epoch = Some(bounded(words, line, "epoch")?);
            }
            "valid-from" => {
                once(self.valid_from.is_some(), line, "valid-from")?;
                self.valid_from = Some(number(words, line, "valid-from")?);
            }
            "valid-until" => {
                once(self.valid_until.is_some(), line, "valid-until")?;
                self.valid_until = Some(number(words, line, "valid-until")?);
            }
            "group-key" => {
                once(self.group_key_id.is_some(), line, "group-key")?;
                self.group_key_id = Some(hex32(words, line, "group-key")?);
            }
            "byzantine-bps" => {
                once(self.byzantine_bps.is_some(), line, "byzantine-bps")?;
                self.byzantine_bps = Some(number(words, line, "byzantine-bps")?);
            }
            "max-competence-ratio" => {
                once(
                    self.max_competence_ratio.is_some(),
                    line,
                    "max-competence-ratio",
                )?;
                self.max_competence_ratio = Some(number(words, line, "max-competence-ratio")?);
            }
            "phy-profile" => {
                once(self.phy_profile.is_some(), line, "phy-profile")?;
                let found = one_word(words, line, "phy-profile <name>")?;
                let expected = PhyProfile::eu868_sf10().name;
                if found != expected {
                    return Err(ManifestError::PhyProfileMismatch {
                        line,
                        found: found.to_string(),
                        expected,
                    });
                }
                self.phy_profile = Some(found.to_string());
            }
            "heard-capacity" => {
                once(self.heard_capacity.is_some(), line, "heard-capacity")?;
                let found = bounded(words, line, "heard-capacity")?;
                if usize::from(found) != Heard::CAPACITY {
                    return Err(ManifestError::HeardCapacityMismatch {
                        line,
                        found: usize::from(found),
                        expected: Heard::CAPACITY,
                    });
                }
                self.heard_capacity = Some(found);
            }
            "member" => {
                self.add_member(words, line)?;
            }
            "issuer" => {
                once(self.issuer.is_some(), line, "issuer")?;
                self.issuer = Some(one_word(words, line, "issuer <fingerprint>")?.to_string());
            }
            "signature" => {
                self.signature = Some(hex64(words, line, "signature")?);
            }
            other => {
                return Err(ManifestError::UnknownDirective {
                    line,
                    word: other.to_string(),
                });
            }
        }
        Ok(())
    }

    fn add_member(
        &mut self,
        words: &mut core::str::SplitWhitespace<'_>,
        line: usize,
    ) -> Result<(), ManifestError> {
        const SHAPE: &str = "member <index> <competence> <public key> <label>";
        let (Some(index), Some(competence), Some(key), Some(id)) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(ManifestError::Malformed {
                line,
                expected: SHAPE,
            });
        };
        if words.next().is_some() {
            return Err(ManifestError::Malformed {
                line,
                expected: SHAPE,
            });
        }

        let index = canonical_number::<u16>(index, line, "index")?;
        let seat = usize::from(index);
        let competence = canonical_number::<u8>(competence, line, "competence")?;
        let bytes = hex_bytes::<32>(key, line, "member key")?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| ManifestError::NotAKey { line, index: seat })?;
        check_label(id, line)?;

        if self.members.iter().any(|held| held.index == index) {
            return Err(ManifestError::DuplicateIndex { line, index: seat });
        }
        if self.members.iter().any(|held| held.id == id) {
            return Err(ManifestError::DuplicateId {
                line,
                id: id.to_string(),
            });
        }
        // One principal, one seat: two members behind one key would let a
        // single holder vote twice and still look like two signers.
        if self.members.iter().any(|held| held.key == key) {
            return Err(ManifestError::DuplicateKey { line, index: seat });
        }
        self.members.push(Member {
            index,
            competence,
            key,
            id: id.to_string(),
        });
        Ok(())
    }

    /// Everything the structure requires, checked, and the members put in
    /// index order so that the file's order cannot change what is signed.
    fn settle(&mut self) -> Result<(), ManifestError> {
        for (value, directive) in [
            (self.epoch.is_some(), "epoch"),
            (self.valid_from.is_some(), "valid-from"),
            (self.valid_until.is_some(), "valid-until"),
            (self.group_key_id.is_some(), "group-key"),
            (self.byzantine_bps.is_some(), "byzantine-bps"),
            (self.max_competence_ratio.is_some(), "max-competence-ratio"),
            (self.phy_profile.is_some(), "phy-profile"),
            (self.heard_capacity.is_some(), "heard-capacity"),
        ] {
            if !value {
                return Err(ManifestError::MissingDirective { directive });
            }
        }
        let from = self.valid_from.unwrap_or_default();
        let until = self.valid_until.unwrap_or_default();
        if until <= from {
            return Err(ManifestError::ValidityInverted { from, until });
        }
        if self.members.is_empty() {
            return Err(ManifestError::NoMembers);
        }
        if self.members.len() > Heard::CAPACITY {
            return Err(ManifestError::FleetTooLarge {
                size: self.members.len(),
                max: Heard::CAPACITY,
            });
        }
        // An index is how a frame names its author, so a gap would leave an
        // index that decodes to nobody.
        self.members.sort_by_key(|member| member.index);
        for (expected, member) in self.members.iter().enumerate() {
            if usize::from(member.index) != expected {
                return Err(ManifestError::IndicesNotContiguous {
                    missing: expected,
                    size: self.members.len(),
                });
            }
        }
        Ok(())
    }

    /// The bytes an administrator signs, for a file that does not carry a
    /// signature yet.
    ///
    /// Held to exactly what [`Parts::finish`] holds a parsed manifest to,
    /// including the policy. Signing something no node will load is a way to
    /// hand an operator a file that bricks a fleet and report success.
    fn canonical(mut self) -> Result<Vec<u8>, ManifestError> {
        self.settle()?;
        Policy::with_settings(
            self.members
                .iter()
                .map(|member| (member.id.clone(), member.competence)),
            self.byzantine_bps.unwrap_or_default(),
            self.max_competence_ratio.unwrap_or_default(),
        )?;
        Ok(canonical(
            self.epoch.unwrap_or_default(),
            self.valid_from.unwrap_or_default(),
            self.valid_until.unwrap_or_default(),
            &self.group_key_id.unwrap_or_default(),
            self.byzantine_bps.unwrap_or_default(),
            self.max_competence_ratio.unwrap_or_default(),
            self.phy_profile.as_deref().unwrap_or_default(),
            self.heard_capacity.unwrap_or_default(),
            &self.members,
        ))
    }

    fn finish(mut self) -> Result<Manifest, ManifestError> {
        self.settle()?;
        let manifest = Manifest {
            epoch: self.epoch.unwrap_or_default(),
            valid_from: self.valid_from.unwrap_or_default(),
            valid_until: self.valid_until.unwrap_or_default(),
            group_key_id: self.group_key_id.unwrap_or_default(),
            byzantine_bps: self.byzantine_bps.unwrap_or_default(),
            max_competence_ratio: self.max_competence_ratio.unwrap_or_default(),
            phy_profile: self.phy_profile.unwrap_or_default(),
            heard_capacity: self.heard_capacity.unwrap_or_default(),
            members: self.members,
            issuer: self.issuer.unwrap_or_default(),
            signature: self.signature.unwrap_or([0; 64]),
        };
        // Built once here so that a manifest which parses but cannot be a
        // policy -- a competence off the scale, a spread past the ratio cap --
        // is refused at the door rather than at the first vote.
        manifest.policy()?;
        Ok(manifest)
    }
}

/// A directive that may appear once has not appeared yet.
fn once(held: bool, line: usize, directive: &'static str) -> Result<(), ManifestError> {
    if held {
        return Err(ManifestError::Repeated { line, directive });
    }
    Ok(())
}

fn one_word<'a>(
    words: &mut core::str::SplitWhitespace<'a>,
    line: usize,
    expected: &'static str,
) -> Result<&'a str, ManifestError> {
    let word = words
        .next()
        .ok_or(ManifestError::Malformed { line, expected })?;
    if words.next().is_some() {
        return Err(ManifestError::Malformed { line, expected });
    }
    Ok(word)
}

fn number<T: core::str::FromStr>(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
    field: &'static str,
) -> Result<T, ManifestError> {
    let word = one_word(words, line, "a directive and one number")?;
    canonical_number(word, line, field)
}

/// A `u16` field, read through `u32` so that "too large" says so rather than
/// saying "not a number".
fn bounded(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
    field: &'static str,
) -> Result<u16, ManifestError> {
    let value: u32 = number(words, line, field)?;
    u16::try_from(value).map_err(|_| ManifestError::NotANumber { line, field })
}

/// One number, one spelling. A sign or a leading zero would let two files that
/// sign to the same bytes look different, or two that look the same sign
/// differently.
fn canonical_number<T: core::str::FromStr>(
    word: &str,
    line: usize,
    field: &'static str,
) -> Result<T, ManifestError> {
    if word.starts_with('+') || word.starts_with('-') {
        return Err(ManifestError::NotCanonicalNumber { line, field });
    }
    if word.len() > 1 && word.starts_with('0') {
        return Err(ManifestError::NotCanonicalNumber { line, field });
    }
    word.parse()
        .map_err(|_| ManifestError::NotANumber { line, field })
}

fn check_label(id: &str, line: usize) -> Result<(), ManifestError> {
    if id.len() > MAX_LABEL_BYTES {
        return Err(ManifestError::LabelTooLong {
            line,
            found: id.len(),
        });
    }
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(ManifestError::LabelCharset {
            line,
            id: id.to_string(),
        });
    }
    Ok(())
}

fn hex32(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
    field: &'static str,
) -> Result<[u8; 32], ManifestError> {
    let word = one_word(words, line, "a directive and one hexadecimal field")?;
    hex_bytes::<32>(word, line, field)
}

fn hex64(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
    field: &'static str,
) -> Result<[u8; 64], ManifestError> {
    let word = one_word(words, line, "a directive and one hexadecimal field")?;
    hex_bytes::<64>(word, line, field)
}

fn hex_bytes<const N: usize>(
    word: &str,
    line: usize,
    field: &'static str,
) -> Result<[u8; N], ManifestError> {
    let wrong = || ManifestError::NotHex {
        line,
        field,
        bytes: N,
    };
    if word.len() != N * 2 {
        return Err(wrong());
    }
    let mut out = [0; N];
    let raw = word.as_bytes();
    for (index, slot) in out.iter_mut().enumerate() {
        let high = nibble(raw[index * 2]).ok_or_else(wrong)?;
        let low = nibble(raw[index * 2 + 1]).ok_or_else(wrong)?;
        *slot = (high << 4) | low;
    }
    Ok(out)
}

/// Lowercase only: one value, one spelling, the same rule the numbers follow.
fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
