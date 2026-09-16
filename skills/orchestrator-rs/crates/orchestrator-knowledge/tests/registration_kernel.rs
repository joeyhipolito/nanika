//! The extension proof for B3-DESIGN §1.
//!
//! Everything here is out-of-crate: this file links `orchestrator-knowledge` as
//! an external crate, so it can only reach the public surface. It registers a
//! synthetic `fixture/widget` type across **all four primitives** and
//! round-trips instances of each without naming a single product concept. If
//! someone adds `match type_name { "learning" => … }` or an `enum
//! DomainRecord` to the crate, a synthetic type stops round-tripping and this
//! file fails.

use std::collections::BTreeSet;

use orchestrator_knowledge::{
    AnyIdentity, BlobEnvelope, BlobId, CanonicalJson, EdgeEnvelope, EdgeId, EventEnvelope,
    ExpectedHead, FieldName, Governance, KnowledgeError, KnowledgeEventId, LifecycleState,
    MediaType, Namespace, Operation, PrimitiveKind, Provenance, RecordEnvelope, RecordId,
    Registrable, RegistryGeneration, RevisionId, SchemaVersion, Sensitivity, TypeManifest,
    TypeName, TypeRegistry,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const NAMESPACE: &str = "fixture";
const WIDGET: &str = "widget";
const WIDGET_EVENT: &str = "widget-observed";
const WIDGET_BLOB: &str = "widget-image";
const WIDGET_EDGE: &str = "widget-supersedes";

fn namespace() -> Result<Namespace, KnowledgeError> {
    Namespace::new(NAMESPACE)
}

fn type_name(value: &str) -> Result<TypeName, KnowledgeError> {
    TypeName::new(value)
}

fn version(value: u32) -> Result<SchemaVersion, KnowledgeError> {
    SchemaVersion::new(value)
}

fn manifest(
    name: &str,
    kind: PrimitiveKind,
    required: &[&str],
) -> Result<TypeManifest, KnowledgeError> {
    Ok(TypeManifest {
        namespace: namespace()?,
        type_name: type_name(name)?,
        kind,
        schema_versions: [version(1)?].into_iter().collect(),
        required_fields: required
            .iter()
            .map(FieldName::new)
            .collect::<Result<BTreeSet<_>, _>>()?,
        sensitivity_ceiling: Sensitivity::Internal,
        allowed_operations: [
            Operation::writing(kind),
            Operation::Get,
            Operation::Query,
            Operation::Traverse,
        ]
        .into_iter()
        .collect(),
    })
}

/// The manifest set a caller supplies. Note that this is plain **data**: no
/// code in `orchestrator-knowledge` knows any of these names.
fn widget_manifests() -> Result<Vec<TypeManifest>, KnowledgeError> {
    Ok(vec![
        manifest(WIDGET, PrimitiveKind::Record, &["serial"])?,
        manifest(WIDGET_EVENT, PrimitiveKind::Event, &["observed_at"])?,
        manifest(WIDGET_BLOB, PrimitiveKind::Blob, &[])?,
        manifest(WIDGET_EDGE, PrimitiveKind::Edge, &[])?,
    ])
}

fn governance(name: &str, generation: RegistryGeneration) -> Result<Governance, KnowledgeError> {
    Ok(Governance {
        namespace: namespace()?,
        type_name: type_name(name)?,
        schema_version: version(1)?,
        provenance: Provenance::new("registration-kernel", Some("mission-fixture"))?,
        sensitivity: Sensitivity::Internal,
        lifecycle: LifecycleState::Active,
        registry_generation: generation,
    })
}

fn record(registry: &TypeRegistry) -> Result<RecordEnvelope, KnowledgeError> {
    Ok(RecordEnvelope::new(
        RecordId::new("fixture/widget/1")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, registry.generation())?,
        CanonicalJson::encode(&json!({"serial": "W-0001", "colour": "green"}))?,
    ))
}

fn event(registry: &TypeRegistry) -> Result<EventEnvelope, KnowledgeError> {
    Ok(EventEnvelope::new(
        KnowledgeEventId::new("fixture/widget-observed/1")?,
        1,
        governance(WIDGET_EVENT, registry.generation())?,
        CanonicalJson::encode(&json!({"observed_at": "2026-08-26T00:00:00Z"}))?,
    ))
}

fn blob(registry: &TypeRegistry) -> Result<BlobEnvelope, KnowledgeError> {
    Ok(BlobEnvelope::new(
        BlobId::of_bytes(b"widget-image-bytes"),
        18,
        MediaType::new("image/png")?,
        governance(WIDGET_BLOB, registry.generation())?,
    ))
}

fn edge(registry: &TypeRegistry) -> Result<EdgeEnvelope, KnowledgeError> {
    Ok(EdgeEnvelope::new(
        EdgeId::new("fixture/widget-supersedes/1")?,
        AnyIdentity::Record(RecordId::new("fixture/widget/2")?),
        AnyIdentity::Record(RecordId::new("fixture/widget/1")?),
        type_name(WIDGET_EDGE)?,
        RevisionId::GENESIS,
        governance(WIDGET_EDGE, registry.generation())?,
    ))
}

#[test]
fn a_synthetic_type_registers_and_round_trips_across_all_four_primitives() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    assert_eq!(registry.generation(), RegistryGeneration::FIRST);
    assert_eq!(registry.registered().len(), 4);

    let admitted_record = registry.admit(record(&registry)?)?;
    assert_eq!(
        admitted_record.envelope().primitive(),
        PrimitiveKind::Record
    );
    assert!(admitted_record.payload_digest().is_some());
    assert_eq!(
        admitted_record.envelope().identity(),
        "fixture/widget/1",
        "round-tripped record identity",
    );

    let admitted_event = registry.admit(event(&registry)?)?;
    assert_eq!(admitted_event.envelope().primitive(), PrimitiveKind::Event);
    assert_eq!(admitted_event.envelope().sequence, 1);

    let admitted_blob = registry.admit(blob(&registry)?)?;
    assert_eq!(admitted_blob.envelope().primitive(), PrimitiveKind::Blob);
    assert!(
        admitted_blob.payload_digest().is_none(),
        "blobs carry no canonical body",
    );

    let admitted_edge = registry.admit(edge(&registry)?)?;
    assert_eq!(admitted_edge.envelope().primitive(), PrimitiveKind::Edge);
    assert_eq!(admitted_edge.envelope().relation, type_name(WIDGET_EDGE)?);
    Ok(())
}

#[test]
fn registration_is_data_so_a_second_unrelated_type_needs_no_code_change() -> TestResult {
    // A completely different vocabulary, registered the same way. If the crate
    // dispatched on type names, this would need a code change; it does not.
    let sprocket = TypeManifest {
        namespace: Namespace::new("other-namespace")?,
        type_name: type_name("sprocket")?,
        kind: PrimitiveKind::Record,
        schema_versions: [version(1)?, version(7)?].into_iter().collect(),
        required_fields: BTreeSet::new(),
        sensitivity_ceiling: Sensitivity::Public,
        allowed_operations: [Operation::Put, Operation::Get].into_iter().collect(),
    };
    let registry = TypeRegistry::activate(vec![sprocket])?;
    let mut governance = governance("sprocket", registry.generation())?;
    governance.namespace = Namespace::new("other-namespace")?;
    governance.sensitivity = Sensitivity::Public;
    governance.schema_version = version(7)?;
    let envelope = RecordEnvelope::new(
        RecordId::new("other/sprocket/1")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance,
        CanonicalJson::encode(&json!({"teeth": 24}))?,
    );
    assert!(registry.admit(envelope).is_ok());
    Ok(())
}

#[test]
fn admission_refuses_an_unregistered_type_and_an_unregistered_schema_version() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;

    let mut unregistered = governance(WIDGET, registry.generation())?;
    unregistered.type_name = type_name("gadget")?;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/gadget/1")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        unregistered,
        CanonicalJson::encode(&json!({"serial": "G-1"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::UnregisteredType { .. })
    ));

    let mut wrong_version = governance(WIDGET, registry.generation())?;
    wrong_version.schema_version = version(2)?;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/widget/9")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        wrong_version,
        CanonicalJson::encode(&json!({"serial": "W-9"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::UnregisteredSchemaVersion { found: 2, .. })
    ));
    Ok(())
}

#[test]
fn admission_refuses_a_primitive_that_does_not_match_its_manifest() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    // `widget` is registered as a Record; presenting it as an Event must fail.
    let envelope = EventEnvelope::new(
        KnowledgeEventId::new("fixture/widget/1")?,
        1,
        governance(WIDGET, registry.generation())?,
        CanonicalJson::encode(&json!({"serial": "W-1"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::PrimitiveMismatch {
            registered: PrimitiveKind::Record,
            found: PrimitiveKind::Event,
            ..
        })
    ));
    Ok(())
}

#[test]
fn admission_refuses_a_missing_required_field() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/widget/3")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, registry.generation())?,
        CanonicalJson::encode(&json!({"colour": "green"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::MissingRequiredField { .. })
    ));
    Ok(())
}

#[test]
fn admission_refuses_sensitivity_above_the_registered_ceiling() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let mut too_sensitive = governance(WIDGET, registry.generation())?;
    too_sensitive.sensitivity = Sensitivity::Secret;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/widget/4")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        too_sensitive,
        CanonicalJson::encode(&json!({"serial": "W-4"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::SensitivityAboveCeiling { .. })
    ));
    Ok(())
}

#[test]
fn admission_refuses_a_secret_bearing_payload_before_it_can_be_made_durable() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/widget/5")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, registry.generation())?,
        CanonicalJson::encode(&json!({
            "serial": "W-5",
            "note": "AKIAIOSFODNN7EXAMPLE",
        }))?,
    );
    let Err(error) = registry.admit(envelope) else {
        return Err("a secret-bearing payload must be refused".into());
    };
    assert!(matches!(error, KnowledgeError::SecretBearingPayload { .. }));
    // The refusal must not carry the secret itself, in either format.
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains("AKIAIOSFODNN7EXAMPLE"),
        "the refusal leaked its payload: {rendered}",
    );
    Ok(())
}

#[test]
fn admission_refuses_invisible_unicode_in_a_body() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let envelope = RecordEnvelope::new(
        RecordId::new("fixture/widget/6")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, registry.generation())?,
        CanonicalJson::encode(&json!({"serial": "W\u{200b}-6"}))?,
    );
    assert!(matches!(
        registry.admit(envelope),
        Err(KnowledgeError::UnsafeText { .. })
    ));
    Ok(())
}

#[test]
fn a_retired_generation_is_rejected_rather_than_silently_accepted() -> TestResult {
    let first = TypeRegistry::activate(widget_manifests()?)?;
    let second = first.activate_next(widget_manifests()?)?;
    assert_eq!(second.generation().get(), 2);

    // An envelope stamped for generation 1 must not be admitted by generation 2.
    let stale = RecordEnvelope::new(
        RecordId::new("fixture/widget/7")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, first.generation())?,
        CanonicalJson::encode(&json!({"serial": "W-7"}))?,
    );
    assert!(matches!(
        second.admit(stale),
        Err(KnowledgeError::RegistryGenerationRetired {
            stale: 1,
            active: 2
        })
    ));

    // After rolling back, generation-2 envelopes are the retired ones.
    let rolled_back = second.rollback_to(first.generation())?;
    assert_eq!(rolled_back.generation(), first.generation());
    let from_retired = RecordEnvelope::new(
        RecordId::new("fixture/widget/8")?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(WIDGET, second.generation())?,
        CanonicalJson::encode(&json!({"serial": "W-8"}))?,
    );
    assert!(matches!(
        rolled_back.admit(from_retired),
        Err(KnowledgeError::RegistryGenerationRetired {
            stale: 2,
            active: 1
        })
    ));
    // ...and generation-1 envelopes work again.
    assert!(rolled_back.admit(record(&rolled_back)?).is_ok());
    Ok(())
}

#[test]
fn rollback_to_a_generation_that_never_existed_is_refused() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let never_activated = registry.activate_next(widget_manifests()?)?.generation();
    assert!(matches!(
        registry.rollback_to(never_activated),
        Err(KnowledgeError::UnknownRegistryGeneration { .. })
    ));
    Ok(())
}

#[test]
fn activation_is_atomic_so_a_bad_manifest_leaves_the_prior_generation_intact() -> TestResult {
    let registry = TypeRegistry::activate(widget_manifests()?)?;
    let incomplete = TypeManifest {
        schema_versions: BTreeSet::new(),
        ..manifest(WIDGET, PrimitiveKind::Record, &[])?
    };
    assert!(matches!(
        registry.activate_next(vec![incomplete]),
        Err(KnowledgeError::IncompleteManifest { .. })
    ));
    // The caller's registry is untouched: still generation 1, still admitting.
    assert_eq!(registry.generation(), RegistryGeneration::FIRST);
    assert!(registry.admit(record(&registry)?).is_ok());

    let duplicated = vec![
        manifest(WIDGET, PrimitiveKind::Record, &[])?,
        manifest(WIDGET, PrimitiveKind::Event, &[])?,
    ];
    assert!(matches!(
        TypeRegistry::activate(duplicated),
        Err(KnowledgeError::DuplicateType { .. })
    ));
    Ok(())
}
