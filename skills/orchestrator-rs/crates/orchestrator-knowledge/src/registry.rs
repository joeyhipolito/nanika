//! Typed Record/Event/Blob/Edge registration (B3-DESIGN §1 `registry.rs`).
//!
//! **The extension proof.** Registering a new type is *data*, never code: a
//! caller builds a [`TypeManifest`], calls [`TypeRegistry::activate`], and puts
//! envelopes of that type. There is no `enum DomainRecord`, no
//! `match type_name { "learning" => … }`, and no product name anywhere in this
//! crate. `tests/registration_kernel.rs` registers a synthetic `fixture/widget`
//! type across all four primitives and round-trips it, which fails the moment
//! someone adds product dispatch.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use crate::{
    envelope::{LifecycleState, Registrable, Sensitivity},
    error::{KnowledgeError, ManifestFault},
    identity::{FieldName, Namespace, SchemaVersion, TypeName},
    redaction::{Redactor, SecretVerdict, unsafe_text_findings},
};

/// The four knowledge primitives.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PrimitiveKind {
    /// A versioned, revisioned unit of knowledge.
    Record,
    /// An immutable, sequenced fact.
    Event,
    /// A content-addressed byte payload.
    Blob,
    /// A typed relationship between two identities.
    Edge,
}

impl PrimitiveKind {
    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Event => "event",
            Self::Blob => "blob",
            Self::Edge => "edge",
        }
    }
}

/// Operations a capability or manifest may authorize.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Operation {
    /// Create or replace a record.
    Put,
    /// Append an event.
    Append,
    /// Create an edge.
    Link,
    /// Attach a blob.
    Attach,
    /// Read one identity.
    Get,
    /// Read a bounded page.
    Query,
    /// Walk edges.
    Traverse,
}

impl Operation {
    /// Stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Append => "append",
            Self::Link => "link",
            Self::Attach => "attach",
            Self::Get => "get",
            Self::Query => "query",
            Self::Traverse => "traverse",
        }
    }

    /// The write operation that creates each primitive.
    #[must_use]
    pub const fn writing(kind: PrimitiveKind) -> Self {
        match kind {
            PrimitiveKind::Record => Self::Put,
            PrimitiveKind::Event => Self::Append,
            PrimitiveKind::Blob => Self::Attach,
            PrimitiveKind::Edge => Self::Link,
        }
    }
}

/// Monotonic identifier of one activated registry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RegistryGeneration(u64);

impl RegistryGeneration {
    /// The generation of the first activation.
    pub const FIRST: Self = Self(1);

    /// Returns the raw counter.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for RegistryGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Declaration of one knowledge type. This is the unit of extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeManifest {
    /// Namespace the type lives in.
    pub namespace: Namespace,
    /// Name of the type within its namespace.
    pub type_name: TypeName,
    /// Which primitive this type registers as.
    pub kind: PrimitiveKind,
    /// Schema versions the type accepts.
    pub schema_versions: BTreeSet<SchemaVersion>,
    /// Fields every body of this type must carry.
    pub required_fields: BTreeSet<FieldName>,
    /// Highest sensitivity an envelope of this type may declare.
    pub sensitivity_ceiling: Sensitivity,
    /// Operations permitted on this type.
    pub allowed_operations: BTreeSet<Operation>,
}

impl TypeManifest {
    fn validate(&self) -> Result<(), KnowledgeError> {
        if self.schema_versions.is_empty() {
            return Err(KnowledgeError::IncompleteManifest {
                namespace: self.namespace.clone(),
                type_name: self.type_name.clone(),
                reason: ManifestFault::NoSchemaVersions,
            });
        }
        if self.allowed_operations.is_empty() {
            return Err(KnowledgeError::IncompleteManifest {
                namespace: self.namespace.clone(),
                type_name: self.type_name.clone(),
                reason: ManifestFault::NoOperations,
            });
        }
        Ok(())
    }
}

/// An envelope that passed [`TypeRegistry::admit`].
///
/// The inner envelope is private and the type is non-`Clone`, so an admission
/// receipt cannot be duplicated or fabricated: the only way to obtain one is to
/// pass a live registry check.
#[derive(Debug)]
pub struct Admitted<E: Registrable> {
    envelope: E,
    generation: RegistryGeneration,
    payload_digest: Option<String>,
}

impl<E: Registrable> Admitted<E> {
    /// Borrows the admitted envelope.
    pub const fn envelope(&self) -> &E {
        &self.envelope
    }

    /// The generation this envelope was admitted under.
    #[must_use]
    pub const fn generation(&self) -> RegistryGeneration {
        self.generation
    }

    /// SHA-256 over the canonical body, when the primitive carries one.
    #[must_use]
    pub fn payload_digest(&self) -> Option<&str> {
        self.payload_digest.as_deref()
    }

    /// Consumes the receipt and returns the envelope.
    pub fn into_envelope(self) -> E {
        self.envelope
    }
}

/// The manifest index for one generation: namespace/type pair to manifest.
type ManifestIndex = Arc<BTreeMap<(Namespace, TypeName), TypeManifest>>;

/// Every generation this lineage has activated, retained for
/// [`TypeRegistry::rollback_to`].
type GenerationHistory = Arc<BTreeMap<RegistryGeneration, ManifestIndex>>;

/// Immutable set of registered types at one generation.
///
/// There is no mutable handle and no interior mutability: [`Self::activate`]
/// either validates the whole manifest set and returns a new registry, or fails
/// and leaves the caller's existing generation intact. A half-applied
/// generation is unrepresentable.
#[derive(Clone, Debug)]
pub struct TypeRegistry {
    generation: RegistryGeneration,
    manifests: ManifestIndex,
    history: GenerationHistory,
}

impl TypeRegistry {
    /// Validates and activates a manifest set as generation 1.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::DuplicateType`] when a namespace/type pair
    /// repeats, and [`KnowledgeError::IncompleteManifest`] when any manifest
    /// declares no schema version or no operation.
    pub fn activate(manifests: Vec<TypeManifest>) -> Result<Self, KnowledgeError> {
        let indexed = Self::index(manifests)?;
        let generation = RegistryGeneration::FIRST;
        let mut history = BTreeMap::new();
        history.insert(generation, Arc::clone(&indexed));
        Ok(Self {
            generation,
            manifests: indexed,
            history: Arc::new(history),
        })
    }

    /// Validates and activates a successor generation, retaining this one for
    /// [`Self::rollback_to`].
    ///
    /// # Errors
    /// Same as [`Self::activate`].
    pub fn activate_next(&self, manifests: Vec<TypeManifest>) -> Result<Self, KnowledgeError> {
        let indexed = Self::index(manifests)?;
        let generation = self.generation.next();
        let mut history = (*self.history).clone();
        history.insert(generation, Arc::clone(&indexed));
        Ok(Self {
            generation,
            manifests: indexed,
            history: Arc::new(history),
        })
    }

    fn index(manifests: Vec<TypeManifest>) -> Result<ManifestIndex, KnowledgeError> {
        let mut indexed = BTreeMap::new();
        for manifest in manifests {
            manifest.validate()?;
            let key = (manifest.namespace.clone(), manifest.type_name.clone());
            if indexed.insert(key, manifest.clone()).is_some() {
                return Err(KnowledgeError::DuplicateType {
                    namespace: manifest.namespace,
                    type_name: manifest.type_name,
                });
            }
        }
        Ok(Arc::new(indexed))
    }

    /// The currently active generation.
    #[must_use]
    pub const fn generation(&self) -> RegistryGeneration {
        self.generation
    }

    /// Returns a registry restored to a retained prior generation.
    ///
    /// The restored registry's generation is the *prior* one, so envelopes
    /// stamped with the rolled-back generation are refused by [`Self::admit`]
    /// with [`KnowledgeError::RegistryGenerationRetired`] rather than silently
    /// accepted.
    ///
    /// # Errors
    /// Returns [`KnowledgeError::UnknownRegistryGeneration`] when the requested
    /// generation was never activated on this lineage.
    pub fn rollback_to(&self, prior: RegistryGeneration) -> Result<Self, KnowledgeError> {
        let manifests = self
            .history
            .get(&prior)
            .ok_or(KnowledgeError::UnknownRegistryGeneration { requested: prior })?;
        Ok(Self {
            generation: prior,
            manifests: Arc::clone(manifests),
            history: Arc::clone(&self.history),
        })
    }

    /// Returns the manifest for one namespace/type pair.
    #[must_use]
    pub fn manifest(&self, namespace: &Namespace, type_name: &TypeName) -> Option<&TypeManifest> {
        self.manifests.get(&(namespace.clone(), type_name.clone()))
    }

    /// Lists every registered namespace/type pair, in stable order.
    #[must_use]
    pub fn registered(&self) -> Vec<(Namespace, TypeName)> {
        self.manifests.keys().cloned().collect()
    }

    /// Admits one envelope against the active generation.
    ///
    /// Validation order is deliberately fail-closed: generation, registration,
    /// primitive, schema version, sensitivity, lifecycle, required fields, then
    /// the redaction and unsafe-text scan. **Secret scanning happens here, before
    /// durability** — a secret-bearing payload is refused rather than written and
    /// filtered later (B3-DESIGN §7 rule 2).
    ///
    /// # Errors
    /// Returns the corresponding [`KnowledgeError`] variant for each check.
    pub fn admit<E: Registrable>(&self, envelope: E) -> Result<Admitted<E>, KnowledgeError> {
        let governance = envelope.governance();
        if governance.registry_generation != self.generation {
            return Err(KnowledgeError::RegistryGenerationRetired {
                stale: governance.registry_generation.get(),
                active: self.generation.get(),
            });
        }
        let Some(manifest) = self.manifest(&governance.namespace, &governance.type_name) else {
            return Err(KnowledgeError::UnregisteredType {
                namespace: governance.namespace.clone(),
                type_name: governance.type_name.clone(),
            });
        };
        let found = envelope.primitive();
        if manifest.kind != found {
            return Err(KnowledgeError::PrimitiveMismatch {
                namespace: governance.namespace.clone(),
                type_name: governance.type_name.clone(),
                registered: manifest.kind,
                found,
            });
        }
        if !manifest
            .schema_versions
            .contains(&governance.schema_version)
        {
            return Err(KnowledgeError::UnregisteredSchemaVersion {
                namespace: governance.namespace.clone(),
                type_name: governance.type_name.clone(),
                found: governance.schema_version.get(),
            });
        }
        if governance.sensitivity > manifest.sensitivity_ceiling {
            return Err(KnowledgeError::SensitivityAboveCeiling {
                namespace: governance.namespace.clone(),
                type_name: governance.type_name.clone(),
            });
        }
        let writing = Operation::writing(found);
        if !manifest.allowed_operations.contains(&writing) {
            return Err(KnowledgeError::OperationNotAllowed {
                namespace: governance.namespace.clone(),
                type_name: governance.type_name.clone(),
                operation: writing,
            });
        }
        match governance.lifecycle {
            LifecycleState::Active => {}
            LifecycleState::Tombstoned => return Err(KnowledgeError::Tombstoned),
            state => {
                return Err(KnowledgeError::LifecycleRefused {
                    state,
                    operation: writing,
                });
            }
        }

        let redactor = Redactor::new();
        let payload_digest = match envelope.body() {
            None => None,
            Some(body) => {
                for field in &manifest.required_fields {
                    if !body.has_field(field.as_str()) {
                        return Err(KnowledgeError::MissingRequiredField {
                            namespace: governance.namespace.clone(),
                            type_name: governance.type_name.clone(),
                            field: TypeName::new(field.as_str())?,
                        });
                    }
                }
                let mut findings = 0usize;
                for leaf in body.string_leaves() {
                    if let SecretVerdict::Bearing(kind) = redactor.scan(&leaf) {
                        return Err(KnowledgeError::SecretBearingPayload {
                            type_name: governance.type_name.clone(),
                            kind,
                        });
                    }
                    findings += unsafe_text_findings(&leaf);
                }
                if findings > 0 {
                    return Err(KnowledgeError::UnsafeText {
                        type_name: governance.type_name.clone(),
                        count: findings,
                    });
                }
                Some(body.digest())
            }
        };
        // Blob and edge envelopes carry no body, so their only free text is the
        // identity itself; scan it so a credential cannot ride in as an id.
        if payload_digest.is_none() {
            let identity = envelope.identity();
            if let SecretVerdict::Bearing(kind) = redactor.scan(&identity) {
                return Err(KnowledgeError::SecretBearingPayload {
                    type_name: governance.type_name.clone(),
                    kind,
                });
            }
        }
        Ok(Admitted {
            generation: self.generation,
            payload_digest,
            envelope,
        })
    }
}
