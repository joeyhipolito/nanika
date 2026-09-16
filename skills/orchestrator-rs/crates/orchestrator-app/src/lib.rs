//! Application policies for the Rust orchestrator foundation.

mod audit_chain;
mod audit_read;
mod canonical_event_log;
mod capability;
mod checkpoint_projection;
mod conformance;
mod continuation;
#[cfg(unix)]
mod durable_process_service;
#[cfg(all(unix, feature = "verification-process-canary"))]
mod exact_leaf_authority;
mod fixture_artifact;
mod fixture_authority;
mod fixture_effect_service;
mod fixture_process_service;
mod fixture_sequential_run;
mod fs_util;
mod git_effect_service;
#[cfg(all(unix, feature = "verification-process-canary"))]
mod hermetic_canary_protocol;
#[cfg(all(unix, feature = "verification-process-canary"))]
mod hermetic_canary_root;
#[cfg(all(unix, feature = "verification-process-canary"))]
mod hermetic_process_canary;
mod hermetic_projector;
#[cfg(unix)]
mod hermetic_provider;
mod knowledge_gateway;
mod lifecycle;
mod metrics_owner;
mod metrics_query;
mod metrics_read;
mod mission_service;
#[cfg(unix)]
mod pilot_metrics;
#[cfg(unix)]
mod pilot_process;
#[cfg(unix)]
mod process_receipt;
mod projection;
mod routing;
mod runtime_home;
mod runtime_store;
mod settings_overlay;
mod supervision;
mod usage_multiplexer;
mod worker_spawn;
mod workspace;
mod writer_authority;

pub use audit_chain::{
    AuditChain, AuditChainEntry, AuditChainError, ChainDigest, ChainFault, ChainRange,
    ChainVerification, VerifiedChainHead, audit_chain,
};
pub use audit_read::{
    ALL_METRICS, AuditReadError, AuditReport, ChangeRecord, ConvergenceStatus, DataPoint,
    DecomposerConvergence, MetricName, MissionEvaluation, PhaseEvaluation, Recommendation,
    Regression, Scorecard, ScorecardSummary, TrendLine, build_scorecard, format_scorecard,
    format_scorecard_json, load_reports, load_reports_from_target,
};
pub use canonical_event_log::{
    CanonicalEventLog, CanonicalEventLogError, CanonicalEventLogFault, VerifiedEventProjection,
};
pub use capability::{
    AdditiveFixtureGrant, AuditAppendCapability, CapabilityError, MetricsOwnerCapability,
    OwnerLeaseError,
};
pub use conformance::{
    Baseline, CompatibilityClassification, ConformanceContract, ConformanceLedger,
    ContractDisposition, ContractStatus, Coverage, LedgerScalar, PreservationRule,
    load_bundled_conformance_ledger, load_conformance_ledger,
};
pub use continuation::{
    CONTINUATION_CAPSULE_SCHEMA_VERSION, ContinuationCapsule, ContinuationDecision,
    ContinuationDecisionSummary, ContinuationError, EvidenceDigest, HandoffDigest, RetryPlan,
    select_resume_or_fresh, select_retry, strategy_fingerprint,
};
#[cfg(unix)]
pub use durable_process_service::DurableProcessService;
pub use fixture_artifact::{
    FixtureArtifactAttestor, FixtureArtifactAuthority, FixtureArtifactError,
    FixtureArtifactErrorKind, FixtureArtifactFileIdentity, FixtureArtifactReceipt,
};
pub use fixture_authority::{
    ExecutableCapability, FixtureAdmissionPolicy, FixtureAuthorityError, FreshFixtureAuthority,
    TargetRootAuthority,
};
pub use fixture_effect_service::{
    FixtureArtifactEffectService, FixtureArtifactServiceBuildError, FixtureEvidenceVerifier,
};
pub use fixture_process_service::FixtureProcessService;
pub use fixture_sequential_run::{
    FixtureSequentialRun, FixtureSequentialRunError, FixtureSequentialRuntime,
    FixtureTerminalDisposition, FixtureTerminalRun,
};
pub use git_effect_service::{
    DeniedGitMutationService, GitBarrierDecision, GitBarrierObserver, GitEffectBarrier,
    GitEffectCapability, GitEffectError, GitEffectLedger, GitEffectService, GitIntent, GitReceipt,
    Reconciliation, ReconciliationDisposition, RollbackAction, SLOT_CLEANUP, SLOT_COMMIT,
    SLOT_CREATE_ISOLATION, SLOT_OPEN_PR, SLOT_PUSH, SLOT_REFRESH_BASE, SLOT_ROLLBACK, TrashRoot,
    VerifiedWork,
};
#[cfg(all(unix, feature = "verification-process-canary"))]
pub use hermetic_canary_protocol::{
    HermeticCanaryWorkerProtocolError, run_hermetic_canary_worker_protocol,
};
#[cfg(all(unix, feature = "verification-process-canary"))]
pub use hermetic_process_canary::{
    HERMETIC_RUN_ROOT_ENV, HermeticProcessCanaryError, HermeticProcessCanaryReport,
    run_hermetic_process_canary,
};
#[doc(hidden)]
pub use hermetic_projector::{
    R0JournalCrashCell, R0JournalCrashError, R0JournalCrashGuard, R0JournalRecoveryReport,
    prepare_r0_journal_crash, recover_r0_journal_crash,
};
#[cfg(unix)]
#[doc(hidden)]
pub use hermetic_projector::{
    R0PhaseStartContinuationReport, recover_r0_phase_start_continuation,
    run_r0_phase_start_crash_until_worker_projection_barrier,
};
#[doc(hidden)]
pub use hermetic_provider::{
    R0DurableAttemptError, R0DurableAttemptReport, R0PhaseTerminalCrashGuard,
    R0PhaseTerminalRecoveryReport, R0ProcessCrashCell, R0ProcessCrashGuard,
    R0ProcessCrashRecoveryReport, R0ProcessRecoveryDisposition,
    inspect_r0_phase_start_durable_attempt, prepare_r0_phase_terminal_crash,
    prepare_r0_process_crash, recover_r0_phase_terminal_crash, recover_r0_process_crash,
    run_r0_phase_start_durable_attempt, run_r0_process_crash_until_barrier,
};
pub use knowledge_gateway::{
    AppKnowledgeGateway, AppendReceipt, DeliveryVerdict, DrainReport, EntryRefusal, FileDigest,
    GoAdapterError, GoLearningAdapter, GoLearningAppender, GoLearningReader, GoMemoryAdapter,
    GoMemoryAppender, GoMemoryReader, InsertReceipt, LearningListQuery, LearningPage, LearningRow,
    LearningStats, MAX_ADAPTER_ROWS, MAX_SUPPORTED_LEARNING_SCHEMA_VERSION, MemoryEntry,
    MemoryFileKind, NewLearningRow, PersonaName, ProjectKey, PublicationDrain, RowSetDigest,
    TopQualityQuery, publication_drain,
};
#[cfg(any(test, feature = "test-support"))]
pub use knowledge_gateway::{fixture_production_boundary, open_fixture_runtime_store};
pub use lifecycle::{
    DurableAttemptOutcome, FixtureAttemptRun, FixtureProjectionFault, FixtureWorkerEventSink,
    LifecycleAck, LifecycleCoordinator, LifecycleError, OwnedChildState,
};
pub use metrics_owner::{
    DEFAULT_MISSIONS_LIMIT, EvidenceReconciler, MAX_QUERY_LIMIT, MetricsOwner, MetricsOwnerError,
    MissionTotals, PHASE_METRIC_TYPE, PhaseMetricIntent, PhaseStatus, RecordedPhase, ReportFormat,
    TERMINAL_METRIC_TYPE, TerminalMetricIntent, TokenCounts, clamp_limit, parse_junit_xml,
    parse_tap13, phase_metric_payload, terminal_metric_payload, verification_class_name,
};
#[cfg(any(test, feature = "test-support"))]
pub use metrics_query::{
    CapturingMetricsQueryService, MetricsQueryCall, MetricsQueryFixtureResponses,
};
pub use metrics_query::{
    DEFAULT_TRENDS_DAYS, METRICS_OWNER_QUERY_PROTOCOL, MetricsQueryError, MetricsQueryService,
    MissionsQuery, MissionsResult, PersonaMetricsQuery, PersonaMetricsResult, PhasesQuery,
    PhasesResult, PrivateMissionMetricRow, PrivatePhaseMetricRow, PrivateRunMetricsQuery,
    PrivateRunMetricsResult, RoutingMethodsQuery, RoutingMethodsResult, SkillUsageQuery,
    SkillUsageResult, TrendsQuery, TrendsResult, UnenrolledMetricsQueryService,
};
pub use metrics_read::{
    DayTrend, FALLBACK_ALERT_THRESHOLD, MetricsReadError, MissionSummary, PersonaMetric, PhaseRow,
    RoutingMethodDist, SkillUsage, fallback_rate,
};
pub use mission_service::{AdmittedMission, MissionService, MissionServiceError};
#[cfg(unix)]
pub use pilot_metrics::{
    RUST_PILOT_METRICS_SNAPSHOT_FILE, RustPilotMetricsError, RustPilotMetricsOwner,
    RustPilotMissionMetric, RustPilotPhaseMetric, RustPilotRunMetrics, RustPilotTokenMetric,
    read_rust_pilot_metrics,
};
#[cfg(unix)]
pub use pilot_process::{
    RUST_PILOT_PROCESS_OWNERSHIP_FILE, RustPilotProcessAttempt, RustPilotProcessError,
    RustPilotProcessExecutable, RustPilotProcessExecution, RustPilotProcessOpen,
    RustPilotProcessRecovery, RustPilotProcessReleaseAdmission, RustPilotProcessReleasePermit,
    RustPilotProcessSession, RustPilotRecoveredProcessEvidence,
};
// `PhaseMetricIntent::new` is public and takes an `ExactProcessGroupAbsence`,
// so the witness and the only function that produces one must be nameable by
// out-of-crate callers; otherwise the type would be uninhabitable outside this
// crate and the reaping gate would be untestable from a gate file.
pub use orchestrator_process::{
    CancellationToken, ExactProcessGroupAbsence, KernelProcessIdentity, ProcessError,
    ProcessIdentityError, ProcessInfrastructureFailure, ProcessReport, ProcessSpec,
    ProcessSupervisor, ProcessTermination, RecordedProcessIdentityStatus,
    inspect_recorded_process_identity,
};
pub use projection::project_event;
pub use routing::{
    DispatchDecisionRequest, FailClosedCause, FixedAuthorityInputs,
    ROUTING_DECISION_TRANSITION_KIND, ROUTING_FAIL_CLOSED_TRANSITION_KIND,
    ROUTING_JOURNAL_PAYLOAD_VERSION, ReadCapability, ResolvedRoutingAuthority, RouteIdentity,
    RoutingConfigError, RoutingDecisionJournal, RoutingDispatchError, RoutingDispatchOutcome,
    RoutingJournalRecord, RoutingWarning, decide_dispatch_route, load_routing_map,
    resolve_fixed_authority, routing_map_for_run,
};
pub use runtime_home::{
    AuthorizedFixtureRuntimeHome, AuthorizedIsolatedRuntimeHome, AuthorizedProductionRuntimeHome,
    DirectoryProbe, HomeInputs, HomeSelection, IsolatedFixtureRoot, IsolatedHomeCanaryCapability,
    IsolatedHomeGuards, LiveHomeCanaryCapability, ProductionBoundary, ReadOnlyEventLogAuthority,
    ReadOnlyEventLogRoot, ReadOnlyEventLogTarget, ResolvedRuntimeHome, RuntimeHomeResolver,
    RustPilotRuntimeHome, attest_bundled_helper, bundled_helper_digest,
};
pub use runtime_store::{
    ClaimedOutboxEffect, ClaimedPublication, CommandAcknowledgement, CompatibilityProjection,
    EffectEvidence, EffectEvidenceCode, EffectObservation, EffectOperationSlot, EffectResolution,
    JournalCommit, JournalIntent, OutboxEffect, OutboxEffectKind, OutboxIntent, OutboxState,
    ProcessExecutionIdentity, ProcessNotStartedEvidenceReason, ProcessReleaseAuthorization,
    ProcessUncertaintyEvidence, ProjectionReceipt, PublicationCounts, PublicationIntent,
    PublicationOutcome, PublicationState, ReasoningAssignmentRow, ReasoningAttempt,
    ReasoningCounts, ReasoningCriterionRow, ReasoningEvidenceRow, ReasoningHandoffRow,
    ReasoningIntent, ReasoningReviewRow, ReasoningRevision, ReasoningWrite, RetryAuthorization,
    RuntimeStore, RuntimeStoreError, StartedProcessFailureEvidence, StorageActorAuthority,
};
pub use settings_overlay::{
    OverlayError, OverlayExit, RoleDenyPolicy, SettingsOverlay, WorkerRole,
};
pub use supervision::{
    FixtureProcessAuthority, FixtureProcessReport, FixtureProcessSpec, FixtureProtocolReport,
    SupervisorError, SupervisorLimits, parse_fixture_protocol,
};
pub use usage_multiplexer::{
    BoundUsageObservation, ClaudeStatuslineAdapterPolicyV1, ClaudeStatuslineUsageHandle,
    MAX_RETAINED_OBSERVATIONS_PER_SLOT, MAX_RETAINED_USAGE_OBSERVATIONS, MAX_RETAINED_USAGE_SLOTS,
    PendingDispatch, TrustedUsageError, UsageMultiplexer, UsageMultiplexerError,
    UsageMultiplexerPolicy, UsageObservationOutcome, assemble_routing_input,
};
pub use worker_spawn::{
    ArtifactMeta, ContextBundle, HandoffRecord, SkillRef, WorkerConfig, WorkerDispatch,
    WorkerSpawnError, build_claude_md, build_frontmatter, inject_frontmatter_if_missing,
};
pub use workspace::{
    CheckpointReconciliationDisposition, FixtureProjectionWriter, FixtureWorkspaceSeed,
    OptionalSidecar, ProductionProjectionWriter, SidecarState, VerifiedCheckpointProjection,
    VerifiedWorkspaceBase, WorkerFileKind, WorkspaceAuthority, WorkspaceError,
    WorkspaceFileAuthority, WorkspaceHealth, WorkspaceSeed,
};
pub use writer_authority::{
    LegacyQuiescenceProof, ProductionWriterAuthority, WRITER_LOCK_FILE, WriterAuthorityError,
};

use orchestrator_core::CoreError;
use std::{io, path::PathBuf};
use thiserror::Error;

/// Structured failures at the application boundary.
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// A core domain value failed validation.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The conformance ledger is malformed YAML.
    #[error("cannot parse conformance ledger: {0}")]
    InvalidConformanceLedger(#[from] serde_saphyr::Error),
    /// The ledger schema is newer, older, or otherwise unsupported.
    #[error("unsupported conformance ledger schema {found}; expected {expected}")]
    UnsupportedLedgerSchema {
        /// Version found in the fixture.
        found: u32,
        /// Version supported by this build.
        expected: u32,
    },
    /// The ledger defines the same contract more than once.
    #[error("duplicate conformance contract {0}")]
    DuplicateContract(String),
    /// Coverage refers to an undefined contract.
    #[error("coverage entry {owner:?} refers to undefined contract {contract}")]
    UndefinedContractReference {
        /// Package or command owning the coverage entry.
        owner: String,
        /// Missing contract identifier.
        contract: String,
    },
    /// A normative contract is not reachable from coverage or preservation policy.
    #[error("conformance contract {0} is not accounted for by coverage or preservation")]
    UnaccountedContract(String),
    /// A completed contract does not name the executable gate that promoted it.
    #[error(
        "completed conformance contract {0} must name the executable gate and evidence note that promoted it, and carry no missing observation"
    )]
    UnevidencedContractCompletion(String),
    /// An incomplete contract banks completion evidence it has not earned.
    #[error(
        "incomplete conformance contract {0} must name its missing observation and carry no completion evidence"
    )]
    UnclaimedContractEvidence(String),
    /// A required path input was empty.
    #[error("runtime-home input {name} must not be empty")]
    EmptyRuntimeHomeInput {
        /// Input field or environment variable name.
        name: &'static str,
    },
    /// Preparing an already-authorized isolated runtime home failed.
    #[error("cannot prepare authorized runtime home {path:?}: {source}")]
    PrepareRuntimeHome {
        /// Authorized runtime home path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A path could not be safely resolved at the write-policy boundary.
    #[error("cannot inspect runtime path {path:?}: {source}")]
    InspectRuntimePath {
        /// Path being inspected.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// B5-DESIGN §7.3 D2: the runtime home was not named explicitly.
    #[error("the isolated-home door requires an explicit ALLUKA_HOME; {selection:?} was selected")]
    IsolatedHomeNotExplicit {
        /// The precedence rule that selected the home instead.
        selection: runtime_home::HomeSelection,
    },
    /// B5-DESIGN §7.3 D3: the named home overlaps a live home.
    #[error("the isolated-home door refuses a home overlapping {class}")]
    IsolatedHomeOverlapsLiveHome {
        /// Which live-home class was overlapped.
        class: &'static str,
    },
    /// B5-DESIGN §7.3 D4: the named home overlaps the repository checkout.
    #[error("the isolated-home door refuses a home overlapping the repository checkout")]
    IsolatedHomeOverlapsCheckout,
    /// B5-DESIGN §7.3 D5: the named home is not an exact 0700 owned directory.
    #[error("the isolated-home door refuses a home that is not an exact mode-0700 owned directory")]
    IsolatedHomeNotPrivate,
    /// B5-DESIGN §7.3 D6: the named home already holds content this binary did
    /// not create.
    #[error("the isolated-home door refuses a home holding content it did not create")]
    IsolatedHomeNotFresh,
    /// B5-DESIGN §7.3 D7: the bundled helper's bytes are not the ones the
    /// bundle manifest pinned.
    #[error("the bundled helper digest is {expected}, but the helper on disk hashes to {observed}")]
    BundledHelperDigestMismatch {
        /// The digest the manifest (or the env cross-check) named.
        expected: String,
        /// The digest the helper's bytes actually hash to.
        observed: String,
    },
    /// The unchanged fixture admission policy refused the vetted home.
    ///
    /// The most common cause is the one B5-DESIGN §7.5 names as the owner's
    /// open decision: a home outside the process temporary directory.
    #[error("the fixture admission policy refused the isolated runtime home")]
    IsolatedHomeAdmissionRefused,
    /// The Rust first-use pilot selector was not exactly enabled.
    #[error("Rust pilot requires NANIKA_RUST_FIRST_USE_PILOT=1")]
    RustPilotNotEnabled,
    /// The Rust pilot root failed its private ownership admission.
    #[error("Rust pilot runtime-home admission refused")]
    RustPilotAdmissionRefused,
    /// Production runtime-home authorization failed without exposing its path.
    #[error("production runtime-home authorization failed")]
    ProductionRuntimeHomeAuthorizationFailed,
    /// A production boundary was requested before its authorized home was prepared.
    #[error("production runtime home has not been prepared")]
    ProductionRuntimeHomeNotPrepared,
    /// A production runtime-home filesystem operation failed.
    #[error("production runtime-home operation failed during {operation}")]
    ProductionRuntimeHomeOperation {
        /// Stable, non-sensitive operation name.
        operation: &'static str,
        /// Underlying OS failure, which carries no caller path.
        #[source]
        source: io::Error,
    },
    /// Serialized production initialization could not be acquired or recorded.
    #[error("production runtime-home initialization is unavailable")]
    ProductionRuntimeHomeInitializationUnavailable,
    /// The prepared production home was replaced before boundary admission.
    #[error("production runtime-home identity changed before admission")]
    RuntimeHomeIdentityChanged,
    /// An admitted fixture boundary was unavailable or changed identity.
    #[error("fixture runtime-home authority is unavailable")]
    FixtureRuntimeHomeAuthorityUnavailable,
    /// A test attempted to authorize a path outside its fixture root.
    #[error("test runtime home {path:?} escapes isolated fixture root {fixture_root:?}")]
    RuntimeHomeOutsideFixture {
        /// Resolved candidate after path/symlink normalization.
        path: PathBuf,
        /// Canonical isolated fixture root.
        fixture_root: PathBuf,
    },
}
