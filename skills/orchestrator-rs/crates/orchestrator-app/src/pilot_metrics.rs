//! Canonical metrics composition for the private durable Rust pilot.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use orchestrator_core::MissionId;
use orchestrator_knowledge::{
    CanonicalJson, Delegation, ExpectedHead, FieldMask, Governance, KnowledgeCapability,
    KnowledgeError, LifecycleState, Namespace, Operation, PrimitiveKind, Provenance,
    RecordEnvelope, RecordId, RegistryGeneration, RevisionId, SchemaVersion, Sensitivity,
    TypeManifest, TypeName, TypeRegistry, Validity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::capability::{CapabilityRoot, MetricsOwnerCapability, OwnerLeaseError};
use crate::knowledge_gateway::{AppKnowledgeGateway, DeliveryVerdict, publication_drain};
use crate::metrics_owner::{
    MetricsOwner, MetricsOwnerError, PHASE_METRIC_TYPE, PhaseMetricIntent, TERMINAL_METRIC_TYPE,
    TerminalMetricIntent, phase_metric_payload, terminal_metric_payload,
};
use crate::runtime_home::ProductionBoundary;
use crate::runtime_store::{
    JournalIntent, PrivateProcessLedgerBoundary, PrivateProcessLedgerStore, RuntimeStore,
    RuntimeStoreError, StorageActorAuthority,
};
use crate::writer_authority::ProductionWriterAuthority;

const METRICS_NAMESPACE: &str = "metrics";
const METRICS_SCHEMA_VERSION: u32 = 1;
const DRAIN_LIMIT: usize = 64;
const PHASE_TRANSITION_KIND: &str = "rust-pilot.phase-terminal";
const TERMINAL_TRANSITION_KIND: &str = "rust-pilot.mission-terminal";
const PRIVATE_METRICS_SCHEMA_VERSION: u32 = 1;
const MAX_PRIVATE_METRICS_BYTES: usize = 8 * 1024 * 1024;
const MAX_PRIVATE_TEXT_BYTES: usize = 256;
const MAX_PRIVATE_SKILLS: usize = 128;
const MAX_PRIVATE_COUNTER: i64 = 9_007_199_254_740_991;
const MAX_PRIVATE_COST_USD: f64 = 1_000_000_000_000.0;
const MAX_PILOT_SEAL_BYTES: usize = 512;

/// Atomic private read model published by the retained metrics owner.
pub const RUST_PILOT_METRICS_SNAPSHOT_FILE: &str = "orchestrator.rust-pilot.metrics.json";

/// Failures at the retained-writer pilot metrics composition boundary.
#[derive(Debug, Error)]
pub enum RustPilotMetricsError {
    /// The supplied writer did not originate at the private Rust pilot door.
    #[error("pilot metrics require the retained RustPilotRuntimeHome writer")]
    WrongWriterOrigin,
    /// The metrics owner lease is already held.
    #[error(transparent)]
    Lease(#[from] OwnerLeaseError),
    /// The canonical metrics owner refused a row.
    #[error(transparent)]
    Metrics(#[from] MetricsOwnerError),
    /// The authoritative runtime store refused a transition or publication.
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    /// Knowledge registration or secret scanning refused a payload.
    #[error(transparent)]
    Knowledge(#[from] KnowledgeError),
    /// A canonical metric payload could not be encoded.
    #[error("pilot metric payload encoding failed")]
    Encoding,
    /// The read-only pilot metrics view refused an unsafe or foreign store.
    #[error("pilot metrics read refused unsafe, malformed, or foreign storage")]
    ReadRefused,
    /// A private metrics artifact could not be created or inspected.
    #[error("private metrics storage failed: {0}")]
    PrivateStorage(#[from] std::io::Error),
    /// Both an operation and its storage shutdown failed.
    #[error("pilot metrics operation failed: {operation}; storage close failed: {close}")]
    OperationAndClose {
        /// Original operation failure.
        operation: String,
        /// Storage shutdown failure.
        close: RuntimeStoreError,
    },
    /// A queued metric could not be delivered and remains retained.
    #[error("pilot metric publication was retained as a dead letter")]
    DeliveryRefused,
}

/// Bounded read-only metrics for one exact private durable run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustPilotRunMetrics {
    /// Mission aggregate, absent before the terminal publication is consumed.
    pub mission: Option<RustPilotMissionMetric>,
    /// Per-phase rows, including rows whose parsed skill list is empty.
    pub phases: Vec<RustPilotPhaseMetric>,
}

/// One mission aggregate in the private pilot view.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustPilotMissionMetric {
    /// Durable mission status.
    pub status: String,
    /// Observed wall-clock duration.
    pub duration_s: i64,
    /// Number of phase rows aggregated.
    pub phases_total: i64,
    /// Completed phase rows.
    pub phases_completed: i64,
    /// Failed phase rows.
    pub phases_failed: i64,
    /// Skipped phase rows.
    pub phases_skipped: i64,
    /// Token total only when every contributing token field was observed.
    pub tokens: Option<RustPilotTokenMetric>,
    /// Cost total only when every contributing provider reported a cost.
    pub cost_usd: Option<f64>,
}

/// One phase metric in the private pilot view.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustPilotPhaseMetric {
    /// Authored phase name.
    pub phase: String,
    /// Recorded terminal status.
    pub status: String,
    /// Authored persona.
    pub persona: String,
    /// Observed runtime/provider family.
    pub provider: String,
    /// Observed model, empty only when the runtime has no model.
    pub model: String,
    /// Observed effort, empty only when the runtime has no effort.
    pub effort: String,
    /// Observed wall-clock duration.
    pub duration_s: i64,
    /// True only for a classified passing gate.
    pub gate_passed: bool,
    /// True only when the durable process protocol confirmed target release.
    pub target_released: bool,
    /// Parsed skills. An empty list is retained data.
    pub parsed_skills: Vec<String>,
    /// Token counts, or `null` when the terminal wire did not expose them.
    pub tokens: Option<RustPilotTokenMetric>,
    /// Reported cost, or `null` when the runtime did not expose cost.
    pub cost_usd: Option<f64>,
}

/// Checked, nonnegative token counts exposed by the read-only view.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustPilotTokenMetric {
    /// Input tokens.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Cache-read tokens.
    pub cache_read: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateMetricsSnapshot {
    schema_version: u32,
    mission_id: String,
    mission_finished: bool,
    metrics: RustPilotRunMetrics,
}

/// One metrics owner bound to the exact retained private pilot writer.
///
/// Phase and terminal records first enter `runtime.db` in the same SQLite
/// transaction as their recoverable publication. The queue is then drained
/// idempotently into the Go-shaped `metrics.db`; reopening redrains any
/// publication admitted before an abrupt owner death.
pub struct RustPilotMetricsOwner {
    boundary: Arc<ProductionBoundary>,
    owner: MetricsOwner,
    gateway: AppKnowledgeGateway,
}

impl RustPilotMetricsOwner {
    /// Assumes the sole metrics-owner lease beneath a retained pilot writer.
    pub fn under_writer(writer: &ProductionWriterAuthority) -> Result<Self, RustPilotMetricsError> {
        let boundary = writer.boundary();
        if !boundary.is_rust_pilot_writer() {
            return Err(RustPilotMetricsError::WrongWriterOrigin);
        }
        prepare_metrics_database(&boundary)?;
        let owner = MetricsOwner::assume(MetricsOwnerCapability::under_writer(writer)?)?;
        Ok(Self {
            boundary,
            owner,
            gateway: metrics_gateway()?,
        })
    }

    /// Redelivers every bounded pending metric publication after reopening.
    pub fn recover_pending(&self, resolved_at_utc: &str) -> Result<(), RustPilotMetricsError> {
        self.with_store(|store| self.drain(store, resolved_at_utc))
    }

    /// Atomically admits one phase transition and its metric publication, then
    /// drains the publication idempotently.
    pub fn record_phase(
        &self,
        intent: &PhaseMetricIntent,
        authoritative_transition: &Value,
        committed_at_utc: &str,
    ) -> Result<(), RustPilotMetricsError> {
        let transition_id = phase_transition_id(&intent.mission, &intent.phase);
        let publication = self.publication(
            &intent.mission,
            Some(&intent.phase),
            intent.logical_attempt,
            PHASE_METRIC_TYPE,
            &format!("{METRICS_NAMESPACE}/{PHASE_METRIC_TYPE}/{}", intent.phase),
            phase_metric_payload(intent),
        )?;
        let transition = JournalIntent::new(
            transition_id,
            Some(intent.mission.clone()),
            PHASE_TRANSITION_KIND,
            authoritative_transition.clone(),
            committed_at_utc,
        )?
        .with_publication(publication)?;
        self.with_store(|store| {
            store.append(&transition)?;
            pilot_metrics_test_barrier("admitted");
            self.drain(store, committed_at_utc)
        })
    }

    /// Atomically admits the mission terminal transition and aggregate metric,
    /// then drains it. Exact replay returns the original transition and leaves
    /// one terminal aggregate.
    pub fn record_terminal(
        &self,
        mission_id: &MissionId,
        intent: &TerminalMetricIntent,
        authoritative_transition: &Value,
        committed_at_utc: &str,
    ) -> Result<(), RustPilotMetricsError> {
        let publication = self.publication(
            mission_id,
            None,
            1,
            TERMINAL_METRIC_TYPE,
            &format!(
                "{METRICS_NAMESPACE}/{TERMINAL_METRIC_TYPE}/{}",
                mission_id.as_str()
            ),
            terminal_metric_payload(intent),
        )?;
        let transition = JournalIntent::new(
            terminal_transition_id(mission_id),
            Some(mission_id.clone()),
            TERMINAL_TRANSITION_KIND,
            authoritative_transition.clone(),
            committed_at_utc,
        )?
        .with_publication(publication)?;
        self.with_store(|store| {
            store.append(&transition)?;
            pilot_metrics_test_barrier("terminal-admitted");
            self.drain(store, committed_at_utc)
        })
    }

    /// Reads back an already-admitted authoritative phase transition without
    /// dispatching provider work.
    pub fn phase_transition(
        &self,
        mission_id: &MissionId,
        phase: &str,
    ) -> Result<Option<Value>, RustPilotMetricsError> {
        self.with_store(|store| {
            Ok(store.transition_payload(&phase_transition_id(mission_id, phase))?)
        })
    }

    /// Reads back an already-admitted authoritative mission terminal without
    /// reconstructing it or running provider work.
    pub fn terminal_transition(
        &self,
        mission_id: &MissionId,
    ) -> Result<Option<Value>, RustPilotMetricsError> {
        self.with_store(|store| Ok(store.transition_payload(&terminal_transition_id(mission_id))?))
    }

    fn publication(
        &self,
        mission_id: &MissionId,
        phase_id: Option<&str>,
        logical_attempt: u32,
        type_name: &str,
        identity: &str,
        payload: Value,
    ) -> Result<crate::PublicationIntent, RustPilotMetricsError> {
        let body = CanonicalJson::encode(&payload)?;
        let canonical = body.as_str().to_owned();
        let envelope = RecordEnvelope::new(
            RecordId::new(identity)?,
            RevisionId::GENESIS,
            ExpectedHead::absent(),
            governance(
                type_name,
                mission_id.as_str(),
                self.gateway.registry().generation(),
            )?,
            body,
        );
        let evidence = self.gateway.admit(envelope)?;
        Ok(self.gateway.publication_intent(
            mission_id,
            phase_id,
            logical_attempt,
            &evidence,
            &canonical,
        )?)
    }

    fn drain(
        &self,
        store: &mut PrivateProcessLedgerStore,
        resolved_at_utc: &str,
    ) -> Result<(), RustPilotMetricsError> {
        let mut consumer_error = None;
        let report = publication_drain().drain(
            store.runtime_store_mut(),
            DRAIN_LIMIT,
            resolved_at_utc,
            |claimed| match self.owner.consume(claimed) {
                Ok(()) => {
                    pilot_metrics_test_barrier("consumed");
                    DeliveryVerdict::Accepted
                }
                Err(error) => {
                    consumer_error = Some(error);
                    DeliveryVerdict::Refused
                }
            },
        )?;
        if let Some(error) = consumer_error {
            return Err(error.into());
        }
        if !report.dead_lettered.is_empty() {
            return Err(RustPilotMetricsError::DeliveryRefused);
        }
        self.publish_private_snapshot()
    }

    fn publish_private_snapshot(&self) -> Result<(), RustPilotMetricsError> {
        self.boundary
            .verify()
            .map_err(|_| RustPilotMetricsError::WrongWriterOrigin)?;
        let Some(source) = self.owner.private_run_metrics(crate::MAX_QUERY_LIMIT)? else {
            return Ok(());
        };
        let mission = source.mission.map(|mission| RustPilotMissionMetric {
            status: mission.status,
            duration_s: mission.duration_s,
            phases_total: mission.phases_total,
            phases_completed: mission.phases_completed,
            phases_failed: mission.phases_failed,
            phases_skipped: mission.phases_skipped,
            tokens: mission.tokens_known.then_some(RustPilotTokenMetric {
                input: mission.tokens_in,
                output: mission.tokens_out,
                cache_creation: mission.tokens_cache_creation,
                cache_read: mission.tokens_cache_read,
            }),
            cost_usd: mission.cost_known.then_some(mission.cost_usd),
        });
        let phases = source
            .phases
            .into_iter()
            .map(|phase| RustPilotPhaseMetric {
                phase: phase.phase,
                status: phase.status,
                persona: phase.persona,
                provider: phase.provider,
                model: phase.model,
                effort: phase.effort,
                duration_s: phase.duration_s,
                gate_passed: phase.gate_passed,
                target_released: phase.target_released,
                parsed_skills: phase.parsed_skills,
                tokens: phase.tokens_known.then_some(RustPilotTokenMetric {
                    input: phase.tokens_in,
                    output: phase.tokens_out,
                    cache_creation: phase.tokens_cache_creation,
                    cache_read: phase.tokens_cache_read,
                }),
                cost_usd: phase.cost_known.then_some(phase.cost_usd),
            })
            .collect();
        let snapshot = PrivateMetricsSnapshot {
            schema_version: PRIVATE_METRICS_SCHEMA_VERSION,
            mission_id: source.mission_id,
            mission_finished: source.mission_finished,
            metrics: RustPilotRunMetrics { mission, phases },
        };
        validate_snapshot(&snapshot, None)?;
        let encoded = serde_json::to_vec(&snapshot).map_err(|_| RustPilotMetricsError::Encoding)?;
        if encoded.len() > MAX_PRIVATE_METRICS_BYTES {
            return Err(RustPilotMetricsError::ReadRefused);
        }
        crate::fs_util::atomic_replace_private(
            self.boundary.directory(),
            Path::new(RUST_PILOT_METRICS_SNAPSHOT_FILE),
            &encoded,
        )?;
        self.boundary
            .verify()
            .map_err(|_| RustPilotMetricsError::WrongWriterOrigin)
    }

    fn with_store<T>(
        &self,
        operation: impl FnOnce(&mut PrivateProcessLedgerStore) -> Result<T, RustPilotMetricsError>,
    ) -> Result<T, RustPilotMetricsError> {
        self.boundary
            .verify()
            .map_err(|_| RustPilotMetricsError::WrongWriterOrigin)?;
        let mut store = RuntimeStore::open_private(
            PrivateProcessLedgerBoundary::new(Arc::clone(&self.boundary)),
            StorageActorAuthority::new(),
        )?;
        let result = operation(&mut store);
        let closed = store.close();
        match (result, closed) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(error), Err(close)) => Err(RustPilotMetricsError::OperationAndClose {
                operation: error.to_string(),
                close,
            }),
        }
    }
}

/// Opens a bounded, read-only metrics view for one sealed private pilot run.
///
/// The function reads only the owner's atomic JSON projection. It never opens
/// SQLite, acquires a writer, creates sidecars, drains a queue, or writes.
/// A sealed older run with no projection returns an empty view.
pub fn read_rust_pilot_metrics(
    root: &Path,
    binary_version: &str,
    mission_id: &MissionId,
    limit: usize,
) -> Result<RustPilotRunMetrics, RustPilotMetricsError> {
    if limit == 0 || limit > crate::MAX_QUERY_LIMIT {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    if binary_version.is_empty()
        || binary_version.len() > MAX_PRIVATE_TEXT_BYTES
        || binary_version.chars().any(char::is_control)
    {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let requested =
        std::fs::symlink_metadata(root).map_err(|_| RustPilotMetricsError::ReadRefused)?;
    if requested.file_type().is_symlink() || !requested.is_dir() {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let canonical = std::fs::canonicalize(root).map_err(|_| RustPilotMetricsError::ReadRefused)?;
    if canonical != root {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let directory = crate::runtime_home::open_canonical_directory(&canonical)
        .map_err(|_| RustPilotMetricsError::ReadRefused)?;
    validate_private_directory_metadata(
        &directory
            .dir_metadata()
            .map_err(|_| RustPilotMetricsError::ReadRefused)?,
    )?;
    let root_identity = crate::fs_util::identity(
        &directory
            .dir_metadata()
            .map_err(|_| RustPilotMetricsError::ReadRefused)?,
    );
    let expected_seal = format!("nanika-rust-pilot-v1\n{binary_version}\n");
    let observed_seal = read_retained_private_file(
        &directory,
        Path::new("orchestrator.rust-pilot.seal"),
        MAX_PILOT_SEAL_BYTES,
    )
    .map_err(|_| RustPilotMetricsError::ReadRefused)?
    .ok_or(RustPilotMetricsError::ReadRefused)?;
    if observed_seal != expected_seal.as_bytes() {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let encoded = read_retained_private_file(
        &directory,
        Path::new(RUST_PILOT_METRICS_SNAPSHOT_FILE),
        MAX_PRIVATE_METRICS_BYTES,
    )
    .map_err(|_| RustPilotMetricsError::ReadRefused)?;
    verify_retained_root(&canonical, &directory, root_identity)?;
    let Some(encoded) = encoded else {
        return Ok(RustPilotRunMetrics {
            mission: None,
            phases: Vec::new(),
        });
    };
    let mut snapshot: PrivateMetricsSnapshot =
        serde_json::from_slice(&encoded).map_err(|_| RustPilotMetricsError::ReadRefused)?;
    validate_snapshot(&snapshot, Some(mission_id))?;
    snapshot.metrics.phases.truncate(limit);
    Ok(snapshot.metrics)
}

fn prepare_metrics_database(
    boundary: &Arc<ProductionBoundary>,
) -> Result<(), RustPilotMetricsError> {
    let database = Path::new("metrics.db");
    match boundary.directory().symlink_metadata(database) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::fs_util::create_private_file(boundary.directory(), database, &[], false)?;
            crate::fs_util::sync_dir(boundary.directory())?;
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => validate_private_file_metadata(&metadata)?,
    }
    Ok(())
}

fn read_retained_private_file(
    directory: &cap_std::fs::Dir,
    name: &Path,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let named_before = match directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    validate_private_file_metadata_io(&named_before)?;
    if named_before.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private metrics file exceeds its bound",
        ));
    }
    let expected_identity = crate::fs_util::identity(&named_before);
    // A replacement FIFO must not block between the name check and open.
    let descriptor = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?;
    let mut file = cap_std::fs::File::from_std(std::fs::File::from(descriptor));
    let opened = file.metadata()?;
    validate_private_file_metadata_io(&opened)?;
    if crate::fs_util::identity(&opened) != expected_identity {
        return Err(std::io::Error::other(
            "private metrics file identity changed",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(limit).min(limit));
    Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private metrics file exceeds its bound",
        ));
    }
    let opened_after = file.metadata()?;
    let named_after = directory.symlink_metadata(name)?;
    validate_private_file_metadata_io(&opened_after)?;
    validate_private_file_metadata_io(&named_after)?;
    if crate::fs_util::identity(&opened_after) != expected_identity
        || crate::fs_util::identity(&named_after) != expected_identity
        || opened_after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    {
        return Err(std::io::Error::other(
            "private metrics file changed during read",
        ));
    }
    Ok(Some(bytes))
}

fn validate_private_directory_metadata(
    metadata: &cap_std::fs::Metadata,
) -> Result<(), RustPilotMetricsError> {
    use cap_std::fs::MetadataExt;

    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    Ok(())
}

fn validate_private_file_metadata(
    metadata: &cap_std::fs::Metadata,
) -> Result<(), RustPilotMetricsError> {
    validate_private_file_metadata_io(metadata).map_err(|_| RustPilotMetricsError::ReadRefused)
}

fn validate_private_file_metadata_io(metadata: &cap_std::fs::Metadata) -> std::io::Result<()> {
    use cap_std::fs::MetadataExt;

    if !metadata.is_file()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private metrics entry has unsafe type, mode, owner, or link count",
        ));
    }
    Ok(())
}

fn verify_retained_root(
    path: &Path,
    directory: &cap_std::fs::Dir,
    expected: crate::fs_util::FileIdentity,
) -> Result<(), RustPilotMetricsError> {
    let retained = directory
        .dir_metadata()
        .map_err(|_| RustPilotMetricsError::ReadRefused)?;
    validate_private_directory_metadata(&retained)?;
    if crate::fs_util::identity(&retained) != expected {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let reopened = crate::runtime_home::open_canonical_directory(path)
        .map_err(|_| RustPilotMetricsError::ReadRefused)?;
    let reopened_metadata = reopened
        .dir_metadata()
        .map_err(|_| RustPilotMetricsError::ReadRefused)?;
    validate_private_directory_metadata(&reopened_metadata)?;
    if crate::fs_util::identity(&reopened_metadata) != expected {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    Ok(())
}

fn validate_snapshot(
    snapshot: &PrivateMetricsSnapshot,
    expected_mission: Option<&MissionId>,
) -> Result<(), RustPilotMetricsError> {
    if snapshot.schema_version != PRIVATE_METRICS_SCHEMA_VERSION
        || snapshot.mission_finished != snapshot.metrics.mission.is_some()
        || snapshot.mission_id.len() > MAX_PRIVATE_TEXT_BYTES
        || MissionId::new(snapshot.mission_id.clone()).is_err()
        || expected_mission.is_some_and(|mission| mission.as_str() != snapshot.mission_id)
        || snapshot.metrics.phases.len() > crate::MAX_QUERY_LIMIT
    {
        return Err(RustPilotMetricsError::ReadRefused);
    }
    let mut names = BTreeSet::new();
    for phase in &snapshot.metrics.phases {
        if !names.insert(phase.phase.as_str())
            || !valid_projection_text(&phase.phase, false)
            || !matches!(phase.status.as_str(), "completed" | "failed" | "skipped")
            || (phase.gate_passed && phase.status != "completed")
            || !valid_projection_text(&phase.persona, true)
            || !valid_projection_text(&phase.provider, true)
            || !valid_projection_text(&phase.model, true)
            || !valid_projection_text(&phase.effort, true)
            || !valid_projection_counter(phase.duration_s)
            || phase.parsed_skills.len() > MAX_PRIVATE_SKILLS
            || phase
                .parsed_skills
                .iter()
                .any(|skill| !valid_projection_text(skill, false))
            || phase.tokens.as_ref().is_some_and(|tokens| {
                !valid_projection_counter(tokens.input)
                    || !valid_projection_counter(tokens.output)
                    || !valid_projection_counter(tokens.cache_creation)
                    || !valid_projection_counter(tokens.cache_read)
            })
            || phase
                .cost_usd
                .is_some_and(|cost| !valid_projection_cost(cost))
        {
            return Err(RustPilotMetricsError::ReadRefused);
        }
    }
    if let Some(mission) = &snapshot.metrics.mission {
        let phases_total = i64::try_from(snapshot.metrics.phases.len()).unwrap_or(i64::MAX);
        if !valid_projection_text(&mission.status, false)
            || !valid_projection_counter(mission.duration_s)
            || !valid_projection_counter(mission.phases_total)
            || !valid_projection_counter(mission.phases_completed)
            || !valid_projection_counter(mission.phases_failed)
            || !valid_projection_counter(mission.phases_skipped)
            || mission.phases_total != phases_total
            || mission.phases_completed
                != snapshot
                    .metrics
                    .phases
                    .iter()
                    .filter(|phase| phase.status == "completed")
                    .count() as i64
            || mission.phases_failed
                != snapshot
                    .metrics
                    .phases
                    .iter()
                    .filter(|phase| phase.status == "failed")
                    .count() as i64
            || mission.phases_skipped
                != snapshot
                    .metrics
                    .phases
                    .iter()
                    .filter(|phase| phase.status == "skipped")
                    .count() as i64
            || mission
                .phases_completed
                .saturating_add(mission.phases_failed)
                .saturating_add(mission.phases_skipped)
                != mission.phases_total
            || mission.tokens.as_ref().is_some_and(|tokens| {
                !valid_projection_counter(tokens.input)
                    || !valid_projection_counter(tokens.output)
                    || !valid_projection_counter(tokens.cache_creation)
                    || !valid_projection_counter(tokens.cache_read)
            })
            || mission
                .cost_usd
                .is_some_and(|cost| !valid_projection_cost(cost))
        {
            return Err(RustPilotMetricsError::ReadRefused);
        }
    }
    Ok(())
}

fn valid_projection_text(value: &str, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty())
        && value.len() <= MAX_PRIVATE_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

const fn valid_projection_counter(value: i64) -> bool {
    value >= 0 && value <= MAX_PRIVATE_COUNTER
}

fn valid_projection_cost(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_PRIVATE_COST_USD).contains(&value)
}

fn phase_transition_id(mission_id: &MissionId, phase: &str) -> String {
    format!("{}:pilot-metric:phase:{phase}", mission_id.as_str())
}

fn terminal_transition_id(mission_id: &MissionId) -> String {
    format!("{}:pilot-metric:terminal", mission_id.as_str())
}

fn metrics_gateway() -> Result<AppKnowledgeGateway, KnowledgeError> {
    let manifests = [PHASE_METRIC_TYPE, TERMINAL_METRIC_TYPE]
        .into_iter()
        .map(|name| {
            Ok(TypeManifest {
                namespace: Namespace::new(METRICS_NAMESPACE)?,
                type_name: TypeName::new(name)?,
                kind: PrimitiveKind::Record,
                schema_versions: [SchemaVersion::new(METRICS_SCHEMA_VERSION)?]
                    .into_iter()
                    .collect(),
                required_fields: BTreeSet::new(),
                sensitivity_ceiling: Sensitivity::Internal,
                allowed_operations: [
                    Operation::writing(PrimitiveKind::Record),
                    Operation::Get,
                    Operation::Query,
                ]
                .into_iter()
                .collect(),
            })
        })
        .collect::<Result<Vec<_>, KnowledgeError>>()?;
    let registry = TypeRegistry::activate(manifests)?;
    let capability = KnowledgeCapability {
        namespace: Namespace::new(METRICS_NAMESPACE)?,
        types: [PHASE_METRIC_TYPE, TERMINAL_METRIC_TYPE]
            .into_iter()
            .map(TypeName::new)
            .collect::<Result<BTreeSet<_>, _>>()?,
        operations: [Operation::Put, Operation::Get, Operation::Query]
            .into_iter()
            .collect(),
        field_mask: FieldMask::All,
        sensitivity_ceiling: Sensitivity::Internal,
        validity: Validity::at(registry.generation()),
        delegation: Delegation::NotDelegable,
    };
    Ok(AppKnowledgeGateway::new(registry, capability))
}

fn governance(
    type_name: &str,
    mission_id: &str,
    generation: RegistryGeneration,
) -> Result<Governance, KnowledgeError> {
    Ok(Governance {
        namespace: Namespace::new(METRICS_NAMESPACE)?,
        type_name: TypeName::new(type_name)?,
        schema_version: SchemaVersion::new(METRICS_SCHEMA_VERSION)?,
        provenance: Provenance::new("rust-first-use-durable-pilot", Some(mission_id))?,
        sensitivity: Sensitivity::Internal,
        lifecycle: LifecycleState::Active,
        registry_generation: generation,
    })
}

#[cfg(feature = "test-support")]
fn pilot_metrics_test_barrier(point: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::time::Duration;

    if std::env::var("NANIKA_PILOT_METRICS_TEST_BARRIER")
        .ok()
        .as_deref()
        != Some(point)
    {
        return;
    }
    let Some(ready) = std::env::var_os("NANIKA_PILOT_METRICS_TEST_READY").map(PathBuf::from) else {
        return;
    };
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(ready)
        .and_then(|mut file| {
            file.write_all(b"ready\n")?;
            file.sync_all()
        });
    if result.is_err() {
        return;
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(not(feature = "test-support"))]
fn pilot_metrics_test_barrier(_point: &str) {}
