//! Bounded, fallible knowledge-plane identifiers.
//!
//! Every type here wraps a private `String`/integer behind a checked
//! constructor. There are no public tuple-struct fields, so an unbounded,
//! non-ASCII, or otherwise malformed identity is unrepresentable rather than
//! merely discouraged (B3-DESIGN §1 `identity.rs`).

use std::fmt;

use crate::error::KnowledgeError;

/// Upper bound on any single knowledge identity, in bytes.
pub const MAX_IDENTITY_BYTES: usize = 256;

/// Upper bound on a namespace or type name, in bytes.
pub const MAX_LABEL_BYTES: usize = 64;

/// Length of the hex digest carried by a `sha256:`-prefixed [`BlobId`].
const SHA256_HEX_LEN: usize = 64;

/// Prefix required of every content-addressed [`BlobId`].
const BLOB_PREFIX: &str = "sha256:";

fn checked_label(kind: &'static str, value: &str) -> Result<String, KnowledgeError> {
    if value.is_empty() {
        return Err(KnowledgeError::EmptyIdentity { kind });
    }
    if value.len() > MAX_LABEL_BYTES {
        return Err(KnowledgeError::IdentityTooLong {
            kind,
            found: value.len(),
            limit: MAX_LABEL_BYTES,
        });
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'_' | b'-' | b'.' | b'/')
    }) {
        return Err(KnowledgeError::MalformedIdentity { kind });
    }
    Ok(value.to_owned())
}

fn checked_identity(kind: &'static str, value: &str) -> Result<String, KnowledgeError> {
    if value.is_empty() {
        return Err(KnowledgeError::EmptyIdentity { kind });
    }
    if value.len() > MAX_IDENTITY_BYTES {
        return Err(KnowledgeError::IdentityTooLong {
            kind,
            found: value.len(),
            limit: MAX_IDENTITY_BYTES,
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    {
        return Err(KnowledgeError::MalformedIdentity { kind });
    }
    Ok(value.to_owned())
}

macro_rules! bounded_identity {
    ($name:ident, $kind:literal, $check:ident) => {
        /// Bounded knowledge identity. Constructed only through [`Self::new`].
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validates and wraps one identity value.
            ///
            /// # Errors
            /// Returns [`KnowledgeError`] when the value is empty, exceeds its
            /// byte bound, or carries a character outside the permitted set.
            pub fn new(value: impl AsRef<str>) -> Result<Self, KnowledgeError> {
                $check($kind, value.as_ref()).map(Self)
            }

            /// Borrows the validated identity.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

bounded_identity!(Namespace, "namespace", checked_label);
bounded_identity!(TypeName, "type name", checked_label);
bounded_identity!(FieldName, "field name", checked_label);
bounded_identity!(RecordId, "record id", checked_identity);
bounded_identity!(KnowledgeEventId, "event id", checked_identity);
bounded_identity!(EdgeId, "edge id", checked_identity);

/// Content-addressed blob identity: `sha256:` plus 64 lowercase hex digits.
///
/// The accepted shape mirrors `hashPattern` at
/// `skills/orchestrator/internal/advisorbridge/prepare.go:30`, so a blob id
/// minted here is a valid Go-side reference and vice versa.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct BlobId(String);

impl BlobId {
    /// Validates a `sha256:<64 hex>` content address.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::MalformedIdentity`] for any other shape,
    /// including uppercase hex and a correct-length non-hex tail.
    pub fn new(value: impl AsRef<str>) -> Result<Self, KnowledgeError> {
        let value = value.as_ref();
        let Some(hex) = value.strip_prefix(BLOB_PREFIX) else {
            return Err(KnowledgeError::MalformedIdentity { kind: "blob id" });
        };
        if hex.len() != SHA256_HEX_LEN
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(KnowledgeError::MalformedIdentity { kind: "blob id" });
        }
        Ok(Self(value.to_owned()))
    }

    /// Derives the content address of `bytes`.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(bytes);
        let mut rendered = String::with_capacity(BLOB_PREFIX.len() + SHA256_HEX_LEN);
        rendered.push_str(BLOB_PREFIX);
        for byte in digest.finalize() {
            rendered.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            rendered.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        Self(rendered)
    }

    /// Borrows the validated content address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonic revision counter for one identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct RevisionId(u64);

impl RevisionId {
    /// The first revision of any identity.
    pub const GENESIS: Self = Self(1);

    /// Builds a revision from a positive counter.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::MalformedIdentity`] for revision zero, which
    /// would be indistinguishable from "no revision".
    pub const fn new(value: u64) -> Result<Self, KnowledgeError> {
        if value == 0 {
            return Err(KnowledgeError::MalformedIdentity { kind: "revision" });
        }
        Ok(Self(value))
    }

    /// Returns the successor revision.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::RevisionOverflow`] at `u64::MAX`, so the
    /// counter can never silently wrap back over live revisions.
    pub const fn next(self) -> Result<Self, KnowledgeError> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(KnowledgeError::RevisionOverflow),
        }
    }

    /// Returns the raw counter.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Compare-and-swap token naming the revision a writer believes is current.
///
/// `None` asserts "this identity does not exist yet"; `Some(revision)` asserts
/// "the current head is exactly `revision`".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExpectedHead(Option<RevisionId>);

impl ExpectedHead {
    /// Asserts the identity has no prior revision.
    #[must_use]
    pub const fn absent() -> Self {
        Self(None)
    }

    /// Asserts the identity's head is exactly `revision`.
    #[must_use]
    pub const fn at(revision: RevisionId) -> Self {
        Self(Some(revision))
    }

    /// Returns the asserted head, if any.
    #[must_use]
    pub const fn revision(self) -> Option<RevisionId> {
        self.0
    }

    /// Checks this token against an observed head.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::RevisionConflict`] when the observed head
    /// differs from the asserted one.
    pub const fn check(self, observed: Option<RevisionId>) -> Result<(), KnowledgeError> {
        match (self.0, observed) {
            (None, None) => Ok(()),
            (Some(expected), Some(found)) if expected.0 == found.0 => Ok(()),
            (expected, found) => Err(KnowledgeError::RevisionConflict {
                expected: match expected {
                    Some(value) => value.0,
                    None => 0,
                },
                found: match found {
                    Some(value) => value.0,
                    None => 0,
                },
            }),
        }
    }
}

/// Schema version of one registered type.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// Builds a schema version from a positive counter.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::MalformedIdentity`] for version zero.
    pub const fn new(value: u32) -> Result<Self, KnowledgeError> {
        if value == 0 {
            return Err(KnowledgeError::MalformedIdentity {
                kind: "schema version",
            });
        }
        Ok(Self(value))
    }

    /// Returns the raw counter.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Union of the four primitive identities, used by [`crate::EdgeEnvelope`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnyIdentity {
    /// A record identity.
    Record(RecordId),
    /// An event identity.
    Event(KnowledgeEventId),
    /// A blob content address.
    Blob(BlobId),
    /// An edge identity.
    Edge(EdgeId),
}

impl AnyIdentity {
    /// Borrows the underlying identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Record(value) => value.as_str(),
            Self::Event(value) => value.as_str(),
            Self::Blob(value) => value.as_str(),
            Self::Edge(value) => value.as_str(),
        }
    }

    /// Names the primitive this identity belongs to.
    #[must_use]
    pub const fn kind(&self) -> crate::registry::PrimitiveKind {
        match self {
            Self::Record(_) => crate::registry::PrimitiveKind::Record,
            Self::Event(_) => crate::registry::PrimitiveKind::Event,
            Self::Blob(_) => crate::registry::PrimitiveKind::Blob,
            Self::Edge(_) => crate::registry::PrimitiveKind::Edge,
        }
    }
}

impl fmt::Display for AnyIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_and_non_ascii_identities() {
        assert!(Namespace::new("").is_err());
        assert!(Namespace::new("A").is_err());
        assert!(Namespace::new("ünïcode").is_err());
        assert!(Namespace::new("x".repeat(MAX_LABEL_BYTES + 1)).is_err());
        assert!(Namespace::new("fixture").is_ok());
        assert!(RecordId::new("x".repeat(MAX_IDENTITY_BYTES + 1)).is_err());
        assert!(RecordId::new("fixture/widget/1").is_ok());
    }

    #[test]
    fn blob_id_matches_go_hash_pattern() {
        let good = format!("sha256:{}", "a".repeat(64));
        assert!(BlobId::new(&good).is_ok());
        assert!(BlobId::new(format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(BlobId::new(format!("sha256:{}", "a".repeat(63))).is_err());
        assert!(BlobId::new(format!("sha1:{}", "a".repeat(64))).is_err());
        assert!(BlobId::new("a".repeat(64)).is_err());
        assert_eq!(BlobId::of_bytes(b"").as_str().len(), 7 + 64);
        assert!(BlobId::new(BlobId::of_bytes(b"widget").as_str()).is_ok());
    }

    #[test]
    fn expected_head_is_a_compare_and_swap_token() {
        assert!(ExpectedHead::absent().check(None).is_ok());
        assert!(
            ExpectedHead::absent()
                .check(Some(RevisionId::GENESIS))
                .is_err()
        );
        assert!(
            ExpectedHead::at(RevisionId::GENESIS)
                .check(Some(RevisionId::GENESIS))
                .is_ok()
        );
        assert!(ExpectedHead::at(RevisionId::GENESIS).check(None).is_err());
    }

    #[test]
    fn revision_and_schema_version_reject_zero() {
        assert!(RevisionId::new(0).is_err());
        assert!(SchemaVersion::new(0).is_err());
        assert_eq!(
            RevisionId::GENESIS.next().ok().map(RevisionId::get),
            Some(2),
        );
    }
}
