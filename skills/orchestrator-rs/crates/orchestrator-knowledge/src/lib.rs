//! Knowledge-plane contract and registration kernel (B3, K1 shape).
//!
//! This crate is a **contract crate, not a store**. It has no `rusqlite`
//! dependency, opens no file, and holds no path: Knowledge-Plane Addendum §5.1
//! forbids creating another canonical silo before Gate K0.5 ratifies
//! `nanika-data/v1`, so durability is provided by `orchestrator-app`'s
//! additive, append-only publication queue inside the existing `runtime.db`
//! (B3-DESIGN Candidate C).
//!
//! What lives here:
//!
//! - [`identity`] — bounded, fallible identifiers, including a `sha256:`
//!   content address that matches the Go `hashPattern`.
//! - [`envelope`] — the four primitives ([`RecordEnvelope`], [`EventEnvelope`],
//!   [`BlobEnvelope`], [`EdgeEnvelope`]) over one [`Governance`] header and a
//!   private [`CanonicalJson`] body.
//! - [`registry`] — [`TypeManifest`] / [`TypeRegistry`]: registration is data,
//!   never code. There is no product name and no per-type dispatch anywhere in
//!   this crate.
//! - [`capability`] — the portable [`KnowledgeCapability`] description.
//! - [`redaction`] — the denylist scanner and the [`Redacted<T>`] wrapper that
//!   makes "secret-bearing errors redact" true by construction.
//! - [`evidence`] — [`PublicationEvidence`], the typed bridge to the durable
//!   queue in `orchestrator-app`.
//! - [`error`] — [`KnowledgeError`], with no variant carrying record content.

pub mod capability;
pub mod envelope;
pub mod error;
pub mod evidence;
pub mod identity;
pub mod redaction;
pub mod registry;

pub use capability::{Delegation, FieldMask, KnowledgeCapability, Validity};
pub use envelope::{
    BlobEnvelope, CanonicalJson, EdgeEnvelope, EventEnvelope, Governance, LifecycleState,
    MediaType, Provenance, RecordEnvelope, Registrable, Sensitivity,
};
pub use error::{CanonicalJsonFault, KnowledgeError, ManifestFault};
pub use evidence::{PUBLICATION_EVIDENCE_SCHEMA, PublicationEvidence};
pub use identity::{
    AnyIdentity, BlobId, EdgeId, ExpectedHead, FieldName, KnowledgeEventId, MAX_IDENTITY_BYTES,
    MAX_LABEL_BYTES, Namespace, RecordId, RevisionId, SchemaVersion, TypeName,
};
pub use redaction::{Redacted, Redactor, SecretKind, SecretVerdict, unsafe_text_findings};
pub use registry::{
    Admitted, Operation, PrimitiveKind, RegistryGeneration, TypeManifest, TypeRegistry,
};
