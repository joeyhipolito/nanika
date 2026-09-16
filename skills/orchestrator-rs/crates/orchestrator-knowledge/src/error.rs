//! Typed knowledge-plane failures.
//!
//! Per Knowledge-Plane Addendum §5.4 no variant is flattened to a free-form
//! string, and — critically for B3-DESIGN §7 rule 1 — **no variant carries
//! content drawn from a record body, a file, provider output, or SQL**. Where
//! context is genuinely needed the variant holds either a typed non-content
//! field ([`TypeName`], [`RecordId`], a byte count, a digest) or a
//! [`Redacted<String>`](crate::redaction::Redacted), whose `Display` and
//! `Debug` both print a placeholder. Because `thiserror` renders `#[error]`
//! through `Display`, redaction happens even when a caller logs with `{}` or
//! `{:?}`.

use thiserror::Error;

use crate::{
    identity::{Namespace, TypeName},
    redaction::{Redacted, SecretKind},
    registry::{Operation, PrimitiveKind, RegistryGeneration},
};

/// Every way a knowledge-plane operation can fail.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum KnowledgeError {
    /// An identity value was empty.
    #[error("{kind} must not be empty")]
    EmptyIdentity {
        /// Stable, non-content identity kind.
        kind: &'static str,
    },
    /// An identity value exceeded its byte bound.
    #[error("{kind} is {found} bytes, exceeding the {limit}-byte bound")]
    IdentityTooLong {
        /// Stable, non-content identity kind.
        kind: &'static str,
        /// Observed byte length.
        found: usize,
        /// Permitted byte length.
        limit: usize,
    },
    /// An identity value carried a character outside its permitted set.
    #[error("{kind} is malformed")]
    MalformedIdentity {
        /// Stable, non-content identity kind.
        kind: &'static str,
    },
    /// A revision counter reached `u64::MAX`.
    #[error("revision counter overflowed")]
    RevisionOverflow,
    /// A compare-and-swap token did not match the observed head.
    #[error("revision conflict: expected head {expected}, found {found}")]
    RevisionConflict {
        /// Asserted head revision; `0` means "asserted absent".
        expected: u64,
        /// Observed head revision; `0` means "absent".
        found: u64,
    },
    /// Canonical JSON encoding rejected the value.
    ///
    /// The reason is a fixed enumeration, never the offending payload.
    #[error("canonical JSON rejected: {reason}")]
    CanonicalJson {
        /// Why encoding failed.
        reason: CanonicalJsonFault,
    },
    /// A payload exceeded the canonical body byte bound.
    #[error("canonical body is {found} bytes, exceeding the {limit}-byte bound")]
    BodyTooLarge {
        /// Observed byte length.
        found: usize,
        /// Permitted byte length.
        limit: usize,
    },
    /// The registry has no manifest for this namespace/type pair.
    #[error("type {namespace}/{type_name} is not registered")]
    UnregisteredType {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
    },
    /// The same namespace/type pair was declared twice in one activation.
    #[error("type {namespace}/{type_name} is declared more than once")]
    DuplicateType {
        /// Namespace of the duplicated manifest.
        namespace: Namespace,
        /// Type name of the duplicated manifest.
        type_name: TypeName,
    },
    /// A manifest declared no schema versions or no allowed operations.
    #[error("manifest for {namespace}/{type_name} is incomplete: {reason}")]
    IncompleteManifest {
        /// Namespace of the rejected manifest.
        namespace: Namespace,
        /// Type name of the rejected manifest.
        type_name: TypeName,
        /// Why the manifest is unusable.
        reason: ManifestFault,
    },
    /// The envelope's schema version is not one the manifest declares.
    #[error("schema version {found} is not registered for {namespace}/{type_name}")]
    UnregisteredSchemaVersion {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
        /// Version carried by the envelope.
        found: u32,
    },
    /// The envelope's primitive does not match the registered manifest.
    #[error("type {namespace}/{type_name} is registered as {registered:?}, not {found:?}")]
    PrimitiveMismatch {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
        /// Primitive the manifest declares.
        registered: PrimitiveKind,
        /// Primitive the envelope carries.
        found: PrimitiveKind,
    },
    /// A required field named by the manifest is absent from the body.
    ///
    /// The *field name* is registry data, never record content, so naming it
    /// leaks nothing about the payload.
    #[error("required field {field} is missing from {namespace}/{type_name}")]
    MissingRequiredField {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
        /// Registered field that was absent.
        field: TypeName,
    },
    /// The envelope's sensitivity exceeds the manifest's ceiling.
    #[error("sensitivity exceeds the registered ceiling for {namespace}/{type_name}")]
    SensitivityAboveCeiling {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
    },
    /// A type carries no classification, so it cannot be admitted.
    ///
    /// Reached when a manifest is absent for a type the caller believes is
    /// registered. There is no `Sensitivity::Unknown`, so unclassified data is
    /// refused rather than defaulted (Addendum §5.4 fail-closed).
    #[error("type {type_name} has no registered sensitivity")]
    UnknownSensitivity {
        /// Type name of the rejected envelope.
        type_name: TypeName,
    },
    /// The requested operation is not declared by the manifest.
    #[error("operation {operation:?} is not allowed for {namespace}/{type_name}")]
    OperationNotAllowed {
        /// Namespace of the rejected envelope.
        namespace: Namespace,
        /// Type name of the rejected envelope.
        type_name: TypeName,
        /// Operation that was refused.
        operation: Operation,
    },
    /// The capability does not authorize this namespace, type, or operation.
    #[error("capability does not authorize {operation:?} on {namespace}/{type_name}")]
    CapabilityRefused {
        /// Namespace of the refused request.
        namespace: Namespace,
        /// Type name of the refused request.
        type_name: TypeName,
        /// Operation that was refused.
        operation: Operation,
    },
    /// The capability's registry generation no longer matches the active one.
    #[error("capability was minted for registry generation {minted}, active is {active}")]
    CapabilityGenerationRetired {
        /// Generation the capability was minted against.
        minted: u64,
        /// Currently active generation.
        active: u64,
    },
    /// The capability's delegation budget is exhausted.
    #[error("capability is not delegable any further")]
    DelegationRefused,
    /// An envelope admitted under a retired generation was replayed.
    #[error("registry generation {stale} was retired; active generation is {active}")]
    RegistryGenerationRetired {
        /// Generation the envelope was admitted under.
        stale: u64,
        /// Currently active generation.
        active: u64,
    },
    /// A rollback named a generation the registry cannot restore.
    #[error("cannot roll back to registry generation {requested}")]
    UnknownRegistryGeneration {
        /// Generation requested by the caller.
        requested: RegistryGeneration,
    },
    /// The envelope's lifecycle state forbids this operation.
    #[error("lifecycle state {state:?} forbids {operation:?}")]
    LifecycleRefused {
        /// State the identity is currently in.
        state: crate::envelope::LifecycleState,
        /// Operation that was refused.
        operation: Operation,
    },
    /// A projection read lagged behind the requested watermark.
    #[error("projection is stale: requested watermark {requested}, projected {projected}")]
    ProjectionStale {
        /// Watermark the caller required.
        requested: u64,
        /// Watermark the projection has reached.
        projected: u64,
    },
    /// The identity is tombstoned and cannot be resurrected.
    #[error("identity is tombstoned and cannot be resurrected")]
    Tombstoned,
    /// Admission found credential-shaped text in the payload.
    ///
    /// Only the *kind* of secret is reported. The matched text is never
    /// carried, so the payload cannot leak through a log of this error.
    #[error("payload for {type_name} carries a {kind} and was refused before durability")]
    SecretBearingPayload {
        /// Type name of the refused envelope.
        type_name: TypeName,
        /// Which denylist family matched.
        kind: SecretKind,
    },
    /// Admission found invisible Unicode or homoglyph confusables.
    #[error("payload for {type_name} carries {count} unsafe-text finding(s)")]
    UnsafeText {
        /// Type name of the refused envelope.
        type_name: TypeName,
        /// How many findings the sanitizer reported.
        count: usize,
    },
    /// An operation failed for a reason whose detail must stay redacted.
    #[error("{operation} failed: {detail}")]
    Redacted {
        /// Stable, non-content operation name.
        operation: &'static str,
        /// Detail that always renders as a placeholder.
        detail: Redacted<String>,
    },
}

/// Fixed enumeration of canonical-JSON encoding faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CanonicalJsonFault {
    /// The value contained a non-finite float (NaN or infinity).
    NonFiniteNumber,
    /// The top-level value was not a JSON object.
    NotAnObject,
    /// Serialization itself failed.
    NotSerializable,
}

impl std::fmt::Display for CanonicalJsonFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NonFiniteNumber => "value carries a non-finite number",
            Self::NotAnObject => "top-level value is not an object",
            Self::NotSerializable => "value is not serializable",
        })
    }
}

/// Fixed enumeration of manifest-validation faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ManifestFault {
    /// No schema version was declared.
    NoSchemaVersions,
    /// No operation was declared.
    NoOperations,
}

impl std::fmt::Display for ManifestFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoSchemaVersions => "no schema version declared",
            Self::NoOperations => "no operation declared",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_detail_never_renders_its_content() {
        let error = KnowledgeError::Redacted {
            operation: "admit",
            detail: Redacted::new("sk-live-0123456789abcdefgh".to_owned()),
        };
        let displayed = error.to_string();
        let debugged = format!("{error:?}");
        assert!(!displayed.contains("sk-live"), "{displayed}");
        assert!(!debugged.contains("sk-live"), "{debugged}");
        assert!(displayed.contains("admit"));
    }
}
