//! The fleet manifest as a file: who is in the fleet, and what each member's
//! judgment is worth.
//!
//! Before this, a node's fleet was a count on the command line and every
//! member counted the same, because the binary was a harness and the radio was
//! what was under test. A fleet of members that decide by different means
//! needs to say so (D24), and saying so is what this file is for: the service
//! that assembles a fleet writes one, and every member reads the same one.
//!
//! ```text
//! # Three vessels; the second and third defer to the first.
//! version 1
//! epoch 7
//! member 0 99 ship-alpha
//! member 1 66 ship-bravo
//! member 2 33 ship-charlie
//! ```
//!
//! Blank lines and everything after a `#` are ignored. `version` and `epoch`
//! must appear before the members. Indices must cover `0..n` exactly once, in
//! any order, because a frame names its author by index and a gap would make
//! an index mean nothing.
//!
//! # This is configuration, not authentication
//!
//! Nothing here is signed. A manifest is trusted exactly as far as the file
//! system it sits on -- the same trust the journal beside it already has --
//! and a fleet at sea needs more than that: a signed manifest naming each
//! member's public key, valid for one epoch, refusing to run on a fixture
//! (`THREAT-MODEL.md` F4, F5). This is the shape that lifecycle will take,
//! with the keys still to come; it is not that lifecycle.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::domain::quorum::{Policy, PolicyError};
use crate::wire::Heard;

extern crate alloc;

/// The manifest format this parser understands.
pub const MANIFEST_VERSION: u32 = 1;

/// Why a manifest file is not a fleet.
///
/// Every variant carries the line it was found on: an operator fixing a
/// manifest on a vessel deserves to be told where to look, not that "the
/// manifest is invalid".
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
    /// No `version` line, or one that came after a member.
    MissingVersion,
    /// A version this parser does not understand.
    UnsupportedVersion {
        /// Line number, counting from one.
        line: usize,
        /// What it asked for.
        version: u32,
    },
    /// No `epoch` line, or one that came after a member.
    MissingEpoch,
    /// A directive that must appear once appeared twice.
    Repeated {
        /// Line number, counting from one.
        line: usize,
        /// Which directive.
        directive: &'static str,
    },
    /// A member with an empty ID.
    BlankMemberId {
        /// Line number, counting from one.
        line: usize,
    },
    /// Two members with the same index.
    DuplicateIndex {
        /// Line number, counting from one.
        line: usize,
        /// The index.
        index: usize,
    },
    /// Two members with the same ID.
    DuplicateId {
        /// Line number, counting from one.
        line: usize,
        /// The ID.
        id: String,
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
            Self::MissingVersion => f.write_str("no version line before the first member"),
            Self::UnsupportedVersion { line, version } => write!(
                f,
                "line {line}: manifest version {version}, this build understands {MANIFEST_VERSION}"
            ),
            Self::MissingEpoch => f.write_str("no epoch line before the first member"),
            Self::Repeated { line, directive } => {
                write!(f, "line {line}: {directive} appears more than once")
            }
            Self::BlankMemberId { line } => write!(f, "line {line}: member ID is empty"),
            Self::DuplicateIndex { line, index } => {
                write!(f, "line {line}: index {index} is already taken")
            }
            Self::DuplicateId { line, id } => {
                write!(f, "line {line}: member ID {id:?} is already taken")
            }
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

/// One member: the index a frame names it by, what its judgment is worth, and
/// what to call it in a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    index: usize,
    competence: u8,
    id: String,
}

impl Member {
    /// The index a frame names this member by.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The member's competence, on the scale of `domain::quorum::Competence`.
    #[must_use]
    pub const fn competence(&self) -> u8 {
        self.competence
    }

    /// What to call it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// A fleet, as a file says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    epoch: u16,
    members: Vec<Member>,
}

impl Manifest {
    /// Read a manifest.
    ///
    /// # Errors
    ///
    /// [`ManifestError`], which names the line.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut version: Option<u32> = None;
        let mut epoch: Option<u16> = None;
        let mut members: Vec<Member> = Vec::new();

        for (offset, raw) in text.lines().enumerate() {
            let line = offset + 1;
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let mut words = content.split_whitespace();
            let directive = words.next().unwrap_or_default();
            match directive {
                "version" => {
                    if version.is_some() {
                        return Err(ManifestError::Repeated {
                            line,
                            directive: "version",
                        });
                    }
                    let value = one_number(&mut words, line, "version")?;
                    if value != MANIFEST_VERSION {
                        return Err(ManifestError::UnsupportedVersion {
                            line,
                            version: value,
                        });
                    }
                    version = Some(value);
                }
                "epoch" => {
                    if epoch.is_some() {
                        return Err(ManifestError::Repeated {
                            line,
                            directive: "epoch",
                        });
                    }
                    let value: u32 = one_number(&mut words, line, "epoch")?;
                    epoch = Some(u16::try_from(value).map_err(|_| ManifestError::NotANumber {
                        line,
                        field: "epoch",
                    })?);
                }
                "member" => {
                    if version.is_none() {
                        return Err(ManifestError::MissingVersion);
                    }
                    if epoch.is_none() {
                        return Err(ManifestError::MissingEpoch);
                    }
                    let member = parse_member(&mut words, line)?;
                    if members.iter().any(|held| held.index == member.index) {
                        return Err(ManifestError::DuplicateIndex {
                            line,
                            index: member.index,
                        });
                    }
                    if members.iter().any(|held| held.id == member.id) {
                        return Err(ManifestError::DuplicateId {
                            line,
                            id: member.id,
                        });
                    }
                    members.push(member);
                }
                other => {
                    return Err(ManifestError::UnknownDirective {
                        line,
                        word: other.to_string(),
                    });
                }
            }
        }

        if version.is_none() {
            return Err(ManifestError::MissingVersion);
        }
        let epoch = epoch.ok_or(ManifestError::MissingEpoch)?;
        if members.is_empty() {
            return Err(ManifestError::NoMembers);
        }
        if members.len() > Heard::CAPACITY {
            return Err(ManifestError::FleetTooLarge {
                size: members.len(),
                max: Heard::CAPACITY,
            });
        }
        // An index is how a frame names its author, so a gap would leave an
        // index that decodes to nobody.
        members.sort_by_key(|member| member.index);
        for (expected, member) in members.iter().enumerate() {
            if member.index != expected {
                return Err(ManifestError::IndicesNotContiguous {
                    missing: expected,
                    size: members.len(),
                });
            }
        }

        let manifest = Self { epoch, members };
        // Built once here so that a manifest which parses but cannot be a
        // policy -- a competence off the scale, a spread past the ratio cap --
        // is refused at the door rather than at the first vote.
        manifest.policy()?;
        Ok(manifest)
    }

    /// The mission epoch every frame from this fleet carries.
    #[must_use]
    pub const fn epoch(&self) -> u16 {
        self.epoch
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

    /// The quorum policy this fleet implies.
    ///
    /// # Errors
    ///
    /// [`PolicyError`] if the competences do not make a lawful manifest.
    pub fn policy(&self) -> Result<Policy, PolicyError> {
        Policy::new(
            self.members
                .iter()
                .map(|member| (member.id.clone(), member.competence)),
        )
    }
}

fn one_number<T: core::str::FromStr>(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
    field: &'static str,
) -> Result<T, ManifestError> {
    let word = words.next().ok_or(ManifestError::Malformed {
        line,
        expected: "a directive and one number",
    })?;
    if words.next().is_some() {
        return Err(ManifestError::Malformed {
            line,
            expected: "a directive and one number",
        });
    }
    word.parse()
        .map_err(|_| ManifestError::NotANumber { line, field })
}

fn parse_member(
    words: &mut core::str::SplitWhitespace<'_>,
    line: usize,
) -> Result<Member, ManifestError> {
    const SHAPE: &str = "member <index> <competence> <id>";
    let (Some(index), Some(competence), Some(id)) = (words.next(), words.next(), words.next())
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
    if id.is_empty() {
        return Err(ManifestError::BlankMemberId { line });
    }
    Ok(Member {
        index: index.parse().map_err(|_| ManifestError::NotANumber {
            line,
            field: "index",
        })?,
        competence: competence.parse().map_err(|_| ManifestError::NotANumber {
            line,
            field: "competence",
        })?,
        id: id.to_string(),
    })
}
