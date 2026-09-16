//! Typed publication evidence (B3-DESIGN §2).
//!
//! A [`PublicationEvidence`] binds one admitted envelope to the private
//! execution transition that authorized it. The gateway builds it at admission
//! and the durable queue stores it; the drain re-derives it and compares, so a
//! row whose payload was altered in `runtime.db` cannot be delivered as if it
//! were the admitted one.

use sha2::{Digest, Sha256};

use crate::{
    envelope::{Registrable, Sensitivity},
    identity::{Namespace, SchemaVersion, TypeName},
    registry::{Admitted, PrimitiveKind, RegistryGeneration},
};

/// Stable schema tag mixed into every idempotency key and evidence digest.
///
/// Bump this only when the derivation below changes; a stored key derived under
/// an older tag will then no longer collide with a newly derived one.
pub const PUBLICATION_EVIDENCE_SCHEMA: &str = "nanika-knowledge-publication-v1";

/// Everything the durable queue needs about one admitted envelope, with no
/// storage or filesystem concept anywhere in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationEvidence {
    namespace: Namespace,
    type_name: TypeName,
    schema_version: SchemaVersion,
    primitive: PrimitiveKind,
    sensitivity: Sensitivity,
    generation: RegistryGeneration,
    identity: String,
    payload_digest: String,
}

impl PublicationEvidence {
    /// Derives evidence from an admission receipt.
    ///
    /// A body-less primitive (blob, edge) digests its identity instead, so
    /// every publication has exactly one non-empty `payload_digest`.
    pub fn of<E: Registrable>(admitted: &Admitted<E>) -> Self {
        let envelope = admitted.envelope();
        let governance = envelope.governance();
        let identity = envelope.identity();
        let payload_digest = admitted.payload_digest().map_or_else(
            || hex_sha256(identity.as_bytes()),
            std::borrow::ToOwned::to_owned,
        );
        Self {
            namespace: governance.namespace.clone(),
            type_name: governance.type_name.clone(),
            schema_version: governance.schema_version,
            primitive: envelope.primitive(),
            sensitivity: governance.sensitivity,
            generation: admitted.generation(),
            identity,
            payload_digest,
        }
    }

    /// Namespace of the published envelope.
    #[must_use]
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// Type name of the published envelope.
    #[must_use]
    pub const fn type_name(&self) -> &TypeName {
        &self.type_name
    }

    /// Schema version of the published envelope.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    /// Primitive of the published envelope.
    #[must_use]
    pub const fn primitive(&self) -> PrimitiveKind {
        self.primitive
    }

    /// Sensitivity of the published envelope.
    #[must_use]
    pub const fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    /// Registry generation the envelope was admitted under.
    #[must_use]
    pub const fn generation(&self) -> RegistryGeneration {
        self.generation
    }

    /// Identity text of the published envelope.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// SHA-256 over the canonical body, or over the identity for body-less
    /// primitives.
    #[must_use]
    pub fn payload_digest(&self) -> &str {
        &self.payload_digest
    }

    /// Derives the queue's idempotency key.
    ///
    /// The key binds the authorizing mission, phase, and logical attempt to the
    /// envelope identity and payload digest, so re-publishing the identical fact
    /// from a re-run of the same attempt collides on the queue's `UNIQUE`
    /// constraint rather than enqueuing a second row.
    #[must_use]
    pub fn idempotency_key(
        &self,
        mission_id: &str,
        phase_id: Option<&str>,
        logical_attempt: u32,
    ) -> String {
        let mut digest = Sha256::new();
        for field in [
            PUBLICATION_EVIDENCE_SCHEMA,
            mission_id,
            phase_id.unwrap_or(""),
            &logical_attempt.to_string(),
            self.namespace.as_str(),
            self.type_name.as_str(),
            self.primitive.as_str(),
            &self.identity,
            &self.payload_digest,
        ] {
            digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            digest.update(field.as_bytes());
        }
        hex(&digest.finalize())
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        rendered.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    rendered
}
