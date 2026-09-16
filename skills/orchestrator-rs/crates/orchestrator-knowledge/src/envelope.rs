//! Governance headers, canonical bodies, and the four primitive envelopes
//! (B3-DESIGN §1 `envelope.rs`).

use std::{collections::BTreeMap, fmt};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{CanonicalJsonFault, KnowledgeError},
    identity::{
        AnyIdentity, BlobId, EdgeId, ExpectedHead, KnowledgeEventId, Namespace, RecordId,
        RevisionId, SchemaVersion, TypeName,
    },
    registry::{PrimitiveKind, RegistryGeneration},
};

/// Classification of the material an envelope carries.
///
/// There is deliberately **no** `Unknown` variant. Unclassified input cannot be
/// represented, so [`crate::TypeRegistry::admit`] refuses it with
/// [`KnowledgeError::UnknownSensitivity`] rather than defaulting — the
/// type-level form of Addendum §5.4's fail-closed rule.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Sensitivity {
    /// Egressable without restriction.
    Public,
    /// Internal to the estate.
    Internal,
    /// Credential-adjacent; never egresses without an explicit ceiling.
    Secret,
}

impl Sensitivity {
    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Secret => "secret",
        }
    }
}

impl fmt::Display for Sensitivity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where an envelope's content came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenance {
    producer: String,
    mission_id: Option<String>,
}

impl Provenance {
    /// Records the producing component and, when known, its mission.
    ///
    /// # Errors
    /// Returns [`KnowledgeError`] when either value is empty or exceeds the
    /// label bound.
    pub fn new(
        producer: impl AsRef<str>,
        mission_id: Option<&str>,
    ) -> Result<Self, KnowledgeError> {
        let producer = producer.as_ref();
        if producer.is_empty() {
            return Err(KnowledgeError::EmptyIdentity { kind: "producer" });
        }
        if producer.len() > crate::identity::MAX_LABEL_BYTES {
            return Err(KnowledgeError::IdentityTooLong {
                kind: "producer",
                found: producer.len(),
                limit: crate::identity::MAX_LABEL_BYTES,
            });
        }
        let mission_id = match mission_id {
            None => None,
            Some("") => {
                return Err(KnowledgeError::EmptyIdentity { kind: "mission id" });
            }
            Some(value) if value.len() > crate::identity::MAX_IDENTITY_BYTES => {
                return Err(KnowledgeError::IdentityTooLong {
                    kind: "mission id",
                    found: value.len(),
                    limit: crate::identity::MAX_IDENTITY_BYTES,
                });
            }
            Some(value) => Some(value.to_owned()),
        };
        Ok(Self {
            producer: producer.to_owned(),
            mission_id,
        })
    }

    /// The producing component.
    #[must_use]
    pub fn producer(&self) -> &str {
        &self.producer
    }

    /// The authorizing mission, when the producer had one.
    #[must_use]
    pub fn mission_id(&self) -> Option<&str> {
        self.mission_id.as_deref()
    }
}

/// Lifecycle position of one identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    /// Readable and writable.
    Active,
    /// Readable; writes are refused.
    Cold,
    /// Withheld pending review.
    Quarantined,
    /// Deleted; cannot be resurrected.
    Tombstoned,
}

impl LifecycleState {
    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cold => "cold",
            Self::Quarantined => "quarantined",
            Self::Tombstoned => "tombstoned",
        }
    }
}

/// Media type of a [`BlobEnvelope`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaType(String);

impl MediaType {
    /// Validates a `type/subtype` media type.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::MalformedIdentity`] for anything that is not
    /// lowercase ASCII with exactly one `/`.
    pub fn new(value: impl AsRef<str>) -> Result<Self, KnowledgeError> {
        let value = value.as_ref();
        let slashes = value.bytes().filter(|byte| *byte == b'/').count();
        if value.is_empty()
            || slashes != 1
            || value.len() > crate::identity::MAX_LABEL_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'/' | b'-' | b'+' | b'.')
            })
        {
            return Err(KnowledgeError::MalformedIdentity { kind: "media type" });
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the validated media type.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Governance header shared by all four primitives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Governance {
    /// Registered namespace.
    pub namespace: Namespace,
    /// Registered type name.
    pub type_name: TypeName,
    /// Schema version of the body.
    pub schema_version: SchemaVersion,
    /// Where the content came from.
    pub provenance: Provenance,
    /// Classification of the content.
    pub sensitivity: Sensitivity,
    /// Lifecycle position of the identity.
    pub lifecycle: LifecycleState,
    /// Registry generation this envelope targets.
    pub registry_generation: RegistryGeneration,
}

/// Deterministically encoded JSON object body.
///
/// The inner string is private and only constructible through
/// [`CanonicalJson::encode`], which sorts object keys, rejects non-finite
/// numbers, requires a top-level object, and enforces
/// `GO_EVENT_JSON_CONTENT_MAX_BYTES`. Digests over envelopes are therefore
/// stable across processes and architectures.
#[derive(Clone, Eq, PartialEq)]
pub struct CanonicalJson {
    encoded: String,
}

impl CanonicalJson {
    /// Canonicalizes a JSON object.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::CanonicalJson`] for a non-object or
    /// non-finite number, and [`KnowledgeError::BodyTooLarge`] beyond the Go
    /// event-content bound.
    pub fn encode(value: &Value) -> Result<Self, KnowledgeError> {
        let Value::Object(_) = value else {
            return Err(KnowledgeError::CanonicalJson {
                reason: CanonicalJsonFault::NotAnObject,
            });
        };
        let canonical = canonicalize(value)?;
        let encoded =
            serde_json::to_string(&canonical).map_err(|_| KnowledgeError::CanonicalJson {
                reason: CanonicalJsonFault::NotSerializable,
            })?;
        let limit = orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES;
        if encoded.len() > limit {
            return Err(KnowledgeError::BodyTooLarge {
                found: encoded.len(),
                limit,
            });
        }
        Ok(Self { encoded })
    }

    /// Borrows the canonical encoding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.encoded
    }

    /// Returns the SHA-256 of the canonical encoding as lowercase hex.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.encoded.as_bytes());
        let mut rendered = String::with_capacity(64);
        for byte in digest.finalize() {
            rendered.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            rendered.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        rendered
    }

    /// Reports whether the top-level object carries `field`.
    #[must_use]
    pub fn has_field(&self, field: &str) -> bool {
        serde_json::from_str::<Map<String, Value>>(&self.encoded)
            .is_ok_and(|map| map.contains_key(field))
    }

    /// Collects every string reachable from the body, for redaction scanning.
    #[must_use]
    pub fn string_leaves(&self) -> Vec<String> {
        let mut leaves = Vec::new();
        if let Ok(value) = serde_json::from_str::<Value>(&self.encoded) {
            collect_strings(&value, &mut leaves);
        }
        leaves
    }
}

/// Bodies are never rendered: `Debug` on an envelope must not leak content.
impl fmt::Debug for CanonicalJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalJson")
            .field("bytes", &self.encoded.len())
            .field("digest", &self.digest())
            .finish()
    }
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            out.push(key.clone());
            collect_strings(item, out);
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn canonicalize(value: &Value) -> Result<Value, KnowledgeError> {
    Ok(match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let mut out = Map::with_capacity(sorted.len());
            for (key, item) in sorted {
                out.insert(key.clone(), canonicalize(item)?);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(canonicalize)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Number(number) => {
            if number.as_f64().is_some_and(|float| !float.is_finite()) {
                return Err(KnowledgeError::CanonicalJson {
                    reason: CanonicalJsonFault::NonFiniteNumber,
                });
            }
            Value::Number(number.clone())
        }
        other => other.clone(),
    })
}

/// Shared shape of the four primitive envelopes.
///
/// Implemented only inside this crate: the [`sealed::Primitive`] supertrait is
/// crate-private, so a downstream crate cannot add a fifth primitive that
/// bypasses [`crate::TypeRegistry::admit`].
pub trait Registrable: sealed::Primitive {
    /// Which primitive this envelope is.
    fn primitive(&self) -> PrimitiveKind;
    /// The envelope's governance header.
    fn governance(&self) -> &Governance;
    /// The envelope's canonical body, when it carries one.
    ///
    /// [`BlobEnvelope`] and [`EdgeEnvelope`] are pure metadata and return
    /// `None`; their content lives out of band or in the graph respectively.
    fn body(&self) -> Option<&CanonicalJson>;
    /// The envelope's identity, rendered as text.
    fn identity(&self) -> String;
}

pub(crate) mod sealed {
    /// Marker implemented only by this crate's four envelopes.
    pub trait Primitive {}
}

/// A versioned, revisioned unit of knowledge.
#[derive(Clone, Debug)]
pub struct RecordEnvelope {
    /// Stable identity.
    pub id: RecordId,
    /// Revision this envelope creates.
    pub revision: RevisionId,
    /// Compare-and-swap token naming the head the writer believes is current.
    pub expected_head: ExpectedHead,
    /// Governance header.
    pub governance: Governance,
    body: CanonicalJson,
}

impl RecordEnvelope {
    /// Assembles a record envelope over a canonical body.
    #[must_use]
    pub const fn new(
        id: RecordId,
        revision: RevisionId,
        expected_head: ExpectedHead,
        governance: Governance,
        body: CanonicalJson,
    ) -> Self {
        Self {
            id,
            revision,
            expected_head,
            governance,
            body,
        }
    }
}

/// An immutable, sequenced fact.
#[derive(Clone, Debug)]
pub struct EventEnvelope {
    /// Stable identity.
    pub id: KnowledgeEventId,
    /// Position in the producer's sequence.
    pub sequence: u64,
    /// Governance header.
    pub governance: Governance,
    body: CanonicalJson,
}

impl EventEnvelope {
    /// Assembles an event envelope over a canonical body.
    #[must_use]
    pub const fn new(
        id: KnowledgeEventId,
        sequence: u64,
        governance: Governance,
        body: CanonicalJson,
    ) -> Self {
        Self {
            id,
            sequence,
            governance,
            body,
        }
    }
}

/// A content-addressed byte payload held out of band.
#[derive(Clone, Debug)]
pub struct BlobEnvelope {
    /// Content address.
    pub id: BlobId,
    /// Byte length of the addressed content.
    pub byte_len: u64,
    /// Media type of the addressed content.
    pub media_type: MediaType,
    /// Governance header.
    pub governance: Governance,
}

impl BlobEnvelope {
    /// Assembles a blob envelope.
    #[must_use]
    pub const fn new(
        id: BlobId,
        byte_len: u64,
        media_type: MediaType,
        governance: Governance,
    ) -> Self {
        Self {
            id,
            byte_len,
            media_type,
            governance,
        }
    }
}

/// A typed, revisioned relationship between two identities.
#[derive(Clone, Debug)]
pub struct EdgeEnvelope {
    /// Stable identity.
    pub id: EdgeId,
    /// Source identity.
    pub from: AnyIdentity,
    /// Target identity.
    pub to: AnyIdentity,
    /// Registered relation name.
    pub relation: TypeName,
    /// Revision this envelope creates.
    pub revision: RevisionId,
    /// Governance header.
    pub governance: Governance,
}

impl EdgeEnvelope {
    /// Assembles an edge envelope.
    #[must_use]
    pub const fn new(
        id: EdgeId,
        from: AnyIdentity,
        to: AnyIdentity,
        relation: TypeName,
        revision: RevisionId,
        governance: Governance,
    ) -> Self {
        Self {
            id,
            from,
            to,
            relation,
            revision,
            governance,
        }
    }
}

impl sealed::Primitive for RecordEnvelope {}

impl Registrable for RecordEnvelope {
    fn primitive(&self) -> PrimitiveKind {
        PrimitiveKind::Record
    }

    fn governance(&self) -> &Governance {
        &self.governance
    }

    fn body(&self) -> Option<&CanonicalJson> {
        Some(&self.body)
    }

    fn identity(&self) -> String {
        self.id.as_str().to_owned()
    }
}

impl sealed::Primitive for EventEnvelope {}

impl Registrable for EventEnvelope {
    fn primitive(&self) -> PrimitiveKind {
        PrimitiveKind::Event
    }

    fn governance(&self) -> &Governance {
        &self.governance
    }

    fn body(&self) -> Option<&CanonicalJson> {
        Some(&self.body)
    }

    fn identity(&self) -> String {
        self.id.as_str().to_owned()
    }
}

impl sealed::Primitive for BlobEnvelope {}

impl Registrable for BlobEnvelope {
    fn primitive(&self) -> PrimitiveKind {
        PrimitiveKind::Blob
    }

    fn governance(&self) -> &Governance {
        &self.governance
    }

    /// Blobs are pure metadata here: the addressed bytes live out of band.
    fn body(&self) -> Option<&CanonicalJson> {
        None
    }

    fn identity(&self) -> String {
        self.id.as_str().to_owned()
    }
}

impl sealed::Primitive for EdgeEnvelope {}

impl Registrable for EdgeEnvelope {
    fn primitive(&self) -> PrimitiveKind {
        PrimitiveKind::Edge
    }

    fn governance(&self) -> &Governance {
        &self.governance
    }

    /// Edges are pure metadata: the relationship *is* the content.
    fn body(&self) -> Option<&CanonicalJson> {
        None
    }

    fn identity(&self) -> String {
        self.id.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult<T = ()> = Result<T, KnowledgeError>;

    #[test]
    fn canonical_json_sorts_keys_and_is_stable() -> TestResult {
        let left = CanonicalJson::encode(&json!({"b": 1, "a": {"d": 2, "c": 3}}))?;
        let right = CanonicalJson::encode(&json!({"a": {"c": 3, "d": 2}, "b": 1}))?;
        assert_eq!(left.as_str(), right.as_str());
        assert_eq!(left.digest(), right.digest());
        assert_eq!(left.as_str(), r#"{"a":{"c":3,"d":2},"b":1}"#);
        Ok(())
    }

    #[test]
    fn canonical_json_requires_a_top_level_object() {
        assert!(CanonicalJson::encode(&json!([1, 2, 3])).is_err());
        assert!(CanonicalJson::encode(&json!("text")).is_err());
        assert!(CanonicalJson::encode(&json!({})).is_ok());
    }

    #[test]
    fn canonical_json_debug_never_prints_the_body() -> TestResult {
        let body = CanonicalJson::encode(&json!({"secret": "hunter2"}))?;
        let rendered = format!("{body:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("digest"), "{rendered}");
        Ok(())
    }

    #[test]
    fn string_leaves_reach_nested_values_and_keys() -> TestResult {
        let body = CanonicalJson::encode(&json!({"outer": {"inner": ["leaf"]}}))?;
        let leaves = body.string_leaves();
        for expected in ["outer", "inner", "leaf"] {
            assert!(leaves.iter().any(|leaf| leaf == expected), "{leaves:?}");
        }
        Ok(())
    }

    #[test]
    fn media_type_rejects_malformed_values() {
        assert!(MediaType::new("application/json").is_ok());
        assert!(MediaType::new("application").is_err());
        assert!(MediaType::new("a/b/c").is_err());
        assert!(MediaType::new("Application/JSON").is_err());
    }
}
