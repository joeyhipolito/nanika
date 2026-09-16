//! Immutable, verifiable audit chain (B3-DESIGN §5).
//!
//! Each entry binds the previous entry's link digest, a digest over the
//! canonical payload, its sequence, and its timestamp into one `link_digest`.
//! [`AuditChain::verify`] recomputes every link from genesis (or from a
//! previously verified head) and returns the *first* fault it finds.
//!
//! The four adversary classes are each caught by a distinct check, and each
//! check is the one the previous adversary's repair defeats:
//!
//! | Adversary | Broken invariant | Fault |
//! |---|---|---|
//! | rewrite a stored payload | payload no longer hashes to `payload_digest` | [`ChainFault::PayloadMutated`] |
//! | …and recompute `payload_digest` | link no longer hashes to `link_digest` | [`ChainFault::ForgedLink`] |
//! | …and recompute `link_digest` | the *next* entry's `prev_digest` no longer matches | [`ChainFault::LinkMismatch`] |
//! | delete the tail | rows fall short of the persisted head claim | [`ChainFault::Truncated`] |
//! | present another chain of equal length under a verified checkpoint | the row at the checkpoint's sequence does not carry its link digest | [`ChainFault::CheckpointNotOnChain`] |
//!
//! Immutability is enforced twice — here, by the absence of any update or
//! delete method, and in SQLite by the `audit_chain_no_update` /
//! `audit_chain_no_delete` triggers, which refuse a direct statement even from
//! inside the owning process. The triggers are the load-bearing half: a fault
//! that `verify` merely *detects* has already happened, whereas a trigger
//! prevents it.

use std::fmt;

use orchestrator_knowledge::PublicationEvidence;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    capability::AuditAppendCapability,
    runtime_store::{MAX_AUDIT_CHAIN_PAGE, RuntimeStore, RuntimeStoreError, StorageActorAuthority},
};

/// Hex text of [`ChainDigest::GENESIS`], used to seed the head claim.
pub(crate) const GENESIS_DIGEST_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// A 32-byte chain digest.
///
/// Rendered as lowercase hex. Equality is over the bytes, so a differently
/// cased hex string cannot compare equal to a digest by accident.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ChainDigest([u8; 32]);

impl ChainDigest {
    /// The all-zero digest that precedes the first entry.
    pub const GENESIS: Self = Self([0u8; 32]);

    /// Parses lowercase hex into a digest.
    ///
    /// # Errors
    /// Returns [`AuditChainError::MalformedDigest`] for anything that is not
    /// exactly 64 lowercase hex characters.
    pub fn parse(text: &str) -> Result<Self, AuditChainError> {
        if text.len() != 64 {
            return Err(AuditChainError::MalformedDigest);
        }
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let high = hex_value(text.as_bytes()[index * 2])?;
            let low = hex_value(text.as_bytes()[index * 2 + 1])?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    /// Lowercase hex rendering.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut rendered = String::with_capacity(64);
        for byte in self.0 {
            rendered.push(hex_digit(byte >> 4));
            rendered.push(hex_digit(byte & 0x0f));
        }
        rendered
    }

    /// Whether this is the genesis digest.
    #[must_use]
    pub fn is_genesis(self) -> bool {
        self == Self::GENESIS
    }
}

impl fmt::Debug for ChainDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ChainDigest")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for ChainDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

const fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + nibble - 10) as char,
    }
}

const fn hex_value(byte: u8) -> Result<u8, AuditChainError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(AuditChainError::MalformedDigest),
    }
}

/// One entry as it exists in the chain.
///
/// Every field is read-only after construction: there is no setter and no
/// `&mut` accessor, so a holder cannot edit an entry and re-present it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditChainEntry {
    sequence: u64,
    prev_digest: ChainDigest,
    payload_digest: ChainDigest,
    recorded_at: String,
    link_digest: ChainDigest,
}

impl AuditChainEntry {
    /// Position in the chain; the first entry is 1.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Link digest of the preceding entry, or genesis for the first.
    #[must_use]
    pub const fn prev_digest(&self) -> ChainDigest {
        self.prev_digest
    }

    /// Digest over the canonical payload.
    #[must_use]
    pub const fn payload_digest(&self) -> ChainDigest {
        self.payload_digest
    }

    /// RFC3339 timestamp bound into the link.
    #[must_use]
    pub fn recorded_at(&self) -> &str {
        &self.recorded_at
    }

    /// This entry's link digest, which the next entry must carry as its
    /// `prev_digest`.
    #[must_use]
    pub const fn link_digest(&self) -> ChainDigest {
        self.link_digest
    }
}

/// A verified head, usable as a checkpoint so the next verification need not
/// rewalk the whole chain.
///
/// The fields are private and only [`AuditChain::verify`] constructs one, so a
/// caller cannot fabricate a checkpoint that skips past a fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedChainHead {
    sequence: u64,
    link_digest: ChainDigest,
}

impl VerifiedChainHead {
    /// The highest sequence proven consistent.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The link digest at that sequence.
    #[must_use]
    pub const fn link_digest(&self) -> ChainDigest {
        self.link_digest
    }
}

/// Where a verification pass starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainRange {
    /// Recompute every link from genesis. The only way to detect a fault that
    /// predates a checkpoint.
    FromGenesis,
    /// Resume from a previously verified head.
    FromCheckpoint(VerifiedChainHead),
}

/// The distinct ways a chain can be inconsistent.
///
/// No variant carries payload bytes: a fault names a sequence and, at most,
/// digests. That keeps `Display` safe to log even when the entry that faulted
/// held sensitive content (B3-DESIGN §7 rule 1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ChainFault {
    /// The first entry does not carry the genesis digest.
    GenesisMismatch {
        /// The digest actually found at sequence 1.
        found: ChainDigest,
    },
    /// Sequences are not contiguous.
    SequenceGap {
        /// The sequence the walk expected next.
        expected: u64,
        /// The sequence actually found.
        found: u64,
    },
    /// An entry's `prev_digest` is not the previous entry's `link_digest`.
    LinkMismatch {
        /// Sequence of the entry whose backward link is wrong.
        sequence: u64,
    },
    /// A stored payload no longer hashes to its recorded `payload_digest`.
    PayloadMutated {
        /// Sequence of the mutated entry.
        sequence: u64,
    },
    /// Rows fall short of the persisted head claim.
    Truncated {
        /// Highest sequence actually present and consistent.
        last_verified: u64,
        /// Sequence the persisted head claims to exist.
        head_claim: u64,
    },
    /// An entry's `link_digest` is not the digest of its own fields.
    ForgedLink {
        /// Sequence of the forged entry.
        sequence: u64,
    },
    /// A resumption checkpoint does not describe this chain: the row at its
    /// sequence is absent, or its stored link digest is not the checkpoint's.
    CheckpointNotOnChain {
        /// Sequence the checkpoint claims to have verified.
        sequence: u64,
    },
}

impl fmt::Display for ChainFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenesisMismatch { found } => {
                write!(formatter, "audit chain genesis digest is {found}")
            }
            Self::SequenceGap { expected, found } => {
                write!(
                    formatter,
                    "audit chain expected {expected} but found {found}"
                )
            }
            Self::LinkMismatch { sequence } => {
                write!(
                    formatter,
                    "audit chain entry {sequence} has a broken backward link"
                )
            }
            Self::PayloadMutated { sequence } => {
                write!(
                    formatter,
                    "audit chain entry {sequence} payload was mutated"
                )
            }
            Self::Truncated {
                last_verified,
                head_claim,
            } => write!(
                formatter,
                "audit chain verified to {last_verified} but the head claims {head_claim}"
            ),
            Self::ForgedLink { sequence } => {
                write!(
                    formatter,
                    "audit chain entry {sequence} link digest is forged"
                )
            }
            Self::CheckpointNotOnChain { sequence } => {
                write!(
                    formatter,
                    "audit chain checkpoint at {sequence} is not this chain's"
                )
            }
        }
    }
}

/// Outcome of one verification pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainVerification {
    /// Every link recomputed and the head claim matched.
    Intact(VerifiedChainHead),
    /// The first fault found; later entries were not examined.
    Faulted(ChainFault),
}

impl ChainVerification {
    /// The verified head, or `None` when a fault was found.
    #[must_use]
    pub const fn head(self) -> Option<VerifiedChainHead> {
        match self {
            Self::Intact(head) => Some(head),
            Self::Faulted(_) => None,
        }
    }

    /// The fault, or `None` when the chain is intact.
    #[must_use]
    pub const fn fault(self) -> Option<ChainFault> {
        match self {
            Self::Intact(_) => None,
            Self::Faulted(fault) => Some(fault),
        }
    }
}

/// Failures raised while appending to or verifying the chain.
///
/// No variant carries record content, file bytes, or SQL text.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuditChainError {
    /// The underlying store rejected the read or write.
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    /// A stored digest was not 64 lowercase hex characters.
    #[error("audit chain digest is malformed")]
    MalformedDigest,
    /// A checkpoint named a sequence the chain does not reach.
    #[error("audit chain checkpoint is beyond the chain head")]
    CheckpointBeyondHead,
}

/// Append-and-verify authority over the `audit_chain` table.
///
/// There is deliberately no `update`, no `delete`, and no method returning a
/// mutable entry. Obtain one from [`crate::audit_chain`].
pub struct AuditChain {
    capability: AuditAppendCapability,
}

impl AuditChain {
    pub(crate) const fn new(capability: AuditAppendCapability) -> Self {
        Self { capability }
    }

    /// Appends one admitted publication to the chain.
    ///
    /// The payload is the canonical JSON the registry already admitted, so the
    /// chain records exactly the bytes that passed the secret scan. The entry
    /// and the head claim advance in one transaction.
    ///
    /// # Errors
    /// Returns [`AuditChainError::Store`] when the append transaction fails or
    /// the store's own validation refuses a field.
    pub fn append(
        &self,
        store: &mut RuntimeStore,
        evidence: &PublicationEvidence,
        payload_json: &str,
        recorded_at_utc: &str,
    ) -> Result<AuditChainEntry, AuditChainError> {
        let (head_sequence, head_digest) = store.audit_chain_head()?;
        let prev_digest = ChainDigest::parse(&head_digest)?;
        let payload_digest = digest_payload(payload_json);
        let sequence = head_sequence.saturating_add(1);
        let link_digest = link_digest(prev_digest, payload_digest, sequence, recorded_at_utc);
        let append = AuditChainAppend {
            prev_digest: &prev_digest.to_hex(),
            namespace: evidence.namespace().as_str(),
            type_name: evidence.type_name().as_str(),
            identity: evidence.identity(),
            payload_json,
            payload_digest: &payload_digest.to_hex(),
            recorded_at_utc,
            link_digest: &link_digest.to_hex(),
        };
        let assigned = store.append_audit_chain(self.capability.actor(), &append)?;
        // The store refuses a non-successor sequence, so this holds; assert it
        // anyway rather than return an entry whose digest was computed over a
        // sequence the row does not carry.
        if assigned != sequence {
            return Err(AuditChainError::Store(RuntimeStoreError::CorruptDatabase));
        }
        Ok(AuditChainEntry {
            sequence,
            prev_digest,
            payload_digest,
            recorded_at: recorded_at_utc.to_owned(),
            link_digest,
        })
    }

    /// Recomputes every link in `range` and returns the first fault, if any.
    ///
    /// Rows are read in pages of at most
    /// [`crate::runtime_store::MAX_AUDIT_CHAIN_PAGE`], so verification cost is
    /// linear in chain length but memory is constant.
    ///
    /// A [`ChainRange::FromCheckpoint`] resume whose range is empty still costs
    /// one row read: the checkpoint is re-anchored against the stored link at
    /// its own sequence, so it certifies only the chain it was earned on.
    ///
    /// # Errors
    /// Returns [`AuditChainError::CheckpointBeyondHead`] when a checkpoint names
    /// a sequence past the persisted head, and [`AuditChainError::Store`] when a
    /// read fails.
    pub fn verify(
        &self,
        store: &RuntimeStore,
        range: ChainRange,
    ) -> Result<ChainVerification, AuditChainError> {
        let (head_claim, _) = store.audit_chain_head()?;
        let checkpoint = match range {
            ChainRange::FromGenesis => None,
            ChainRange::FromCheckpoint(head) => {
                if head.sequence > head_claim {
                    return Err(AuditChainError::CheckpointBeyondHead);
                }
                Some(head)
            }
        };
        let (mut expected_sequence, mut previous_link) = checkpoint
            .map_or((1u64, ChainDigest::GENESIS), |head| {
                (head.sequence.saturating_add(1), head.link_digest)
            });
        let mut last_verified = expected_sequence.saturating_sub(1);
        let mut walked_a_row = false;
        loop {
            let rows = store.audit_chain_rows(expected_sequence, MAX_AUDIT_CHAIN_PAGE)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let sequence =
                    u64::try_from(row.sequence).map_err(|_| AuditChainError::MalformedDigest)?;
                if sequence != expected_sequence {
                    return Ok(ChainVerification::Faulted(ChainFault::SequenceGap {
                        expected: expected_sequence,
                        found: sequence,
                    }));
                }
                let prev_digest = ChainDigest::parse(&row.prev_digest)?;
                if sequence == 1 && !prev_digest.is_genesis() {
                    return Ok(ChainVerification::Faulted(ChainFault::GenesisMismatch {
                        found: prev_digest,
                    }));
                }
                if prev_digest != previous_link {
                    return Ok(ChainVerification::Faulted(ChainFault::LinkMismatch {
                        sequence,
                    }));
                }
                let recorded_digest = ChainDigest::parse(&row.payload_digest)?;
                if digest_payload(&row.payload_json) != recorded_digest {
                    return Ok(ChainVerification::Faulted(ChainFault::PayloadMutated {
                        sequence,
                    }));
                }
                let stored_link = ChainDigest::parse(&row.link_digest)?;
                let recomputed =
                    link_digest(prev_digest, recorded_digest, sequence, &row.recorded_at_utc);
                if stored_link != recomputed {
                    return Ok(ChainVerification::Faulted(ChainFault::ForgedLink {
                        sequence,
                    }));
                }
                previous_link = stored_link;
                last_verified = sequence;
                expected_sequence = sequence.saturating_add(1);
                walked_a_row = true;
            }
        }
        // An exhausted checkpoint digests nothing: every row it names is
        // behind the resume point. Without re-reading the row it claims, a
        // checkpoint earned on one chain would certify any other chain of the
        // same length, because the head claim is only a count. Re-read that
        // one row and require the stored link to be the checkpoint's.
        if let Some(head) = checkpoint {
            if !walked_a_row && head.sequence > 0 {
                let anchor = store.audit_chain_rows(head.sequence, 1)?;
                let on_this_chain = match anchor.first() {
                    Some(row)
                        if u64::try_from(row.sequence)
                            .is_ok_and(|sequence| sequence == head.sequence) =>
                    {
                        ChainDigest::parse(&row.link_digest)? == head.link_digest
                    }
                    _ => false,
                };
                if !on_this_chain {
                    return Ok(ChainVerification::Faulted(
                        ChainFault::CheckpointNotOnChain {
                            sequence: head.sequence,
                        },
                    ));
                }
            }
        }
        if last_verified < head_claim {
            return Ok(ChainVerification::Faulted(ChainFault::Truncated {
                last_verified,
                head_claim,
            }));
        }
        Ok(ChainVerification::Intact(VerifiedChainHead {
            sequence: last_verified,
            link_digest: previous_link,
        }))
    }
}

/// One entry on its way into storage. Crate-private: an out-of-crate caller
/// cannot assemble a row and hand it to the store directly.
pub(crate) struct AuditChainAppend<'a> {
    pub(crate) prev_digest: &'a str,
    pub(crate) namespace: &'a str,
    pub(crate) type_name: &'a str,
    pub(crate) identity: &'a str,
    pub(crate) payload_json: &'a str,
    pub(crate) payload_digest: &'a str,
    pub(crate) recorded_at_utc: &'a str,
    pub(crate) link_digest: &'a str,
}

/// One entry as read back from storage, before verification.
pub(crate) struct StoredAuditRow {
    pub(crate) sequence: i64,
    pub(crate) prev_digest: String,
    #[allow(dead_code, reason = "read back for parity with the stored row shape")]
    pub(crate) namespace: String,
    #[allow(dead_code, reason = "read back for parity with the stored row shape")]
    pub(crate) type_name: String,
    #[allow(dead_code, reason = "read back for parity with the stored row shape")]
    pub(crate) identity: String,
    pub(crate) payload_json: String,
    pub(crate) payload_digest: String,
    pub(crate) recorded_at_utc: String,
    pub(crate) link_digest: String,
}

fn digest_payload(payload_json: &str) -> ChainDigest {
    let mut digest = Sha256::new();
    digest.update(payload_json.as_bytes());
    ChainDigest(digest.finalize().into())
}

/// `sha256(prev_digest || payload_digest || sequence || recorded_at)`.
///
/// Every field is length-prefixed so no two distinct field tuples can render to
/// the same byte stream — without that, a longer timestamp could absorb a
/// shorter digest and two different entries would share a link.
fn link_digest(
    prev: ChainDigest,
    payload: ChainDigest,
    sequence: u64,
    recorded_at: &str,
) -> ChainDigest {
    let mut digest = Sha256::new();
    field(&mut digest, &prev.0);
    field(&mut digest, &payload.0);
    field(&mut digest, &sequence.to_be_bytes());
    field(&mut digest, recorded_at.as_bytes());
    ChainDigest(digest.finalize().into())
}

fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

/// Builds an [`AuditChain`] under this crate's storage-actor authority.
///
/// Minting stays inside `orchestrator-app`, matching
/// [`crate::publication_drain`].
#[must_use]
pub fn audit_chain() -> AuditChain {
    AuditChain::new(AuditAppendCapability::assume(StorageActorAuthority::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_hex_matches_the_genesis_digest() {
        assert_eq!(ChainDigest::GENESIS.to_hex(), GENESIS_DIGEST_HEX);
        assert!(ChainDigest::GENESIS.is_genesis());
    }

    #[test]
    fn digest_parse_round_trips_and_refuses_malformed_text() -> Result<(), AuditChainError> {
        let digest = digest_payload("{\"a\":1}");
        assert_eq!(ChainDigest::parse(&digest.to_hex())?, digest);
        assert!(matches!(
            ChainDigest::parse("abc"),
            Err(AuditChainError::MalformedDigest)
        ));
        // Uppercase hex is not the canonical rendering and must not parse.
        assert!(matches!(
            ChainDigest::parse(&digest.to_hex().to_uppercase()),
            Err(AuditChainError::MalformedDigest)
        ));
        Ok(())
    }

    #[test]
    fn link_digest_is_field_separated() {
        // Without length prefixing these two would hash identically: the
        // sequence's trailing bytes would absorb the timestamp's leading ones.
        let payload = digest_payload("{}");
        let first = link_digest(ChainDigest::GENESIS, payload, 1, "2026-08-26T00:00:00Z");
        let second = link_digest(ChainDigest::GENESIS, payload, 1, "2026-08-26T00:00:00Z ");
        assert_ne!(first, second);
        assert_ne!(
            first,
            link_digest(ChainDigest::GENESIS, payload, 2, "2026-08-26T00:00:00Z")
        );
    }

    #[test]
    fn fault_display_carries_no_payload() {
        let rendered = ChainFault::PayloadMutated { sequence: 4 }.to_string();
        assert!(rendered.contains('4'));
        assert!(!rendered.contains("sk-"));
    }
}
