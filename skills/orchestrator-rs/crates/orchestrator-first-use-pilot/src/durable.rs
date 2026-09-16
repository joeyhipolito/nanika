//! Phase-boundary durability for the opt-in authored mission pilot.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use getrandom::fill as random_fill;
use orchestrator_app::{
    IsolatedHomeGuards, PhaseMetricIntent, PhaseStatus as MetricPhaseStatus,
    ProductionWriterAuthority, RecordedProcessIdentityStatus, RustPilotMetricsOwner,
    RustPilotProcessAttempt, RustPilotProcessError, RustPilotProcessExecutable,
    RustPilotProcessOpen, RustPilotProcessRecovery, RustPilotProcessReleaseAdmission,
    RustPilotProcessReleasePermit, RustPilotProcessSession, RustPilotRecoveredProcessEvidence,
    RustPilotRuntimeHome, TerminalMetricIntent, TokenCounts, inspect_recorded_process_identity,
    read_rust_pilot_metrics,
};
use orchestrator_core::{
    EventId, MissionId, MissionState, MissionStatus, PhaseDefinition, PhaseId, PhaseStatus,
    ReducerInput, ReducerTransition, reduce,
};
use orchestrator_daemon::MissionCancellationAcknowledgement;
use orchestrator_process::{CancellationToken, ProcessSupervisor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::authored_cycle::{self, AuthoredMission, PhaseEnvironment, PhaseRole};
use super::*;

const MANIFEST_SCHEMA: &str = "nanika.rust-first-use-durable-manifest.v1";
const JOURNAL_SCHEMA: &str = "nanika.rust-first-use-durable-record.v1";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 512;
const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
const STATUS_READ_ATTEMPTS: usize = 12;
const STATUS_RETRY_DELAY: Duration = Duration::from_millis(10);
const CANCELLATION_REQUEST_REASON: &str = "operator cancellation requested";
const CANCELLATION_TERMINAL_REASON: &str =
    "operator cancellation reached terminal after owned cleanup";
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableManifest {
    schema: String,
    mission_id: String,
    mission_text: String,
    mission_digest: String,
    source: SourceBinding,
    plan: PlanBinding,
    routes: Vec<RouteBinding>,
    provider: ProviderBinding,
    verifier: VerifierBinding,
    output_root: String,
    output_directory: DirectoryBinding,
    snapshot: SnapshotBinding,
    #[serde(default)]
    execution_environment: Option<Vec<EnvironmentBinding>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentBinding {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceBinding {
    canonical_path: String,
    head: String,
    directory: DirectoryBinding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PlanBinding {
    execution_order: Vec<String>,
    phases: Vec<PhaseBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PhaseBinding {
    id: String,
    name: String,
    objective: String,
    persona: String,
    role: String,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RouteBinding {
    phase_id: String,
    runtime: String,
    tier: Option<String>,
    persona: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    selection_reason: Option<String>,
    argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderBinding {
    executable: String,
    #[serde(default)]
    executable_id: String,
    version: String,
    model_override: String,
    timeout_secs: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VerifierBinding {
    argv: Vec<String>,
    #[serde(default)]
    executable_id: String,
    timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DirectoryBinding {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotBinding {
    baseline: String,
    workspace: String,
    baseline_directory: DirectoryBinding,
    workspace_directory: DirectoryBinding,
    baseline_digest: String,
    workspace_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalEnvelope {
    previous_digest: String,
    digest: String,
    record: JournalRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalRecord {
    schema: String,
    mission_id: String,
    sequence: i64,
    event_id: String,
    timestamp: String,
    kind: RecordKind,
    phase_id: Option<String>,
    role: Option<String>,
    status: Option<String>,
    reason: Option<String>,
    result: Option<Value>,
    baseline_digest: Option<String>,
    workspace_digest: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordKind {
    MissionStarted,
    PhaseStarted,
    PhaseTerminal,
    PhaseSkippedObservation,
    Paused,
    CancellationRequested,
    MissionTerminal,
}

struct DurableStore {
    progress: super::progress::Progress,
    root: PathBuf,
    root_identity: (u64, u64),
    authority: Arc<ProductionWriterAuthority>,
    metrics: RustPilotMetricsOwner,
    previous_digest: String,
    next_sequence: i64,
    mission_started_at: Option<String>,
    cancellation_requested: bool,
    mission_terminal: bool,
    // Live admission stays closed if terminal publication returns an uncertain error.
    terminal_admission_closed: bool,
}

type SharedDurableStore = Arc<Mutex<DurableStore>>;

struct DurableCancellationRecorder {
    store: SharedDurableStore,
    mission_id: String,
    cancellation: CancellationToken,
}

impl super::cancellation::CancellationRecorder for DurableCancellationRecorder {
    fn request(&mut self, mission_id: &str) -> MissionCancellationAcknowledgement {
        if mission_id != self.mission_id {
            return MissionCancellationAcknowledgement::UnknownMission;
        }
        let mut store = lock(&self.store);
        if store.mission_terminal {
            return MissionCancellationAcknowledgement::Rejected;
        }
        if store.cancellation_requested {
            return MissionCancellationAcknowledgement::AlreadyRequested;
        }
        if store.terminal_admission_closed {
            return MissionCancellationAcknowledgement::Rejected;
        }
        let request = record(
            &self.mission_id,
            0,
            RecordKind::CancellationRequested,
            None,
            None,
            Some("requested"),
            Some(CANCELLATION_REQUEST_REASON),
            None,
            None,
        );
        match store.append(request) {
            Ok(_) => {
                self.cancellation.cancel();
                MissionCancellationAcknowledgement::NewlyRequested
            }
            Err(_) => MissionCancellationAcknowledgement::Rejected,
        }
    }
}

struct DurableReleaseAdmission {
    store: SharedDurableStore,
}

struct DurableReleasePermit<'a> {
    _store: std::sync::MutexGuard<'a, DurableStore>,
}

impl RustPilotProcessReleasePermit for DurableReleasePermit<'_> {}

impl RustPilotProcessReleaseAdmission for DurableReleaseAdmission {
    fn admit_release(&self) -> Option<Box<dyn RustPilotProcessReleasePermit + '_>> {
        let store = lock(&self.store);
        if store.cancellation_requested || store.mission_terminal || store.terminal_admission_closed
        {
            None
        } else {
            Some(Box::new(DurableReleasePermit { _store: store }))
        }
    }
}

pub(crate) struct DurableProcessOwner {
    progress: super::progress::Progress,
    authority: Arc<ProductionWriterAuthority>,
    mission_id: MissionId,
    environment: Vec<EnvironmentBinding>,
    executable_ids: BTreeMap<PathBuf, String>,
    unresolved: Arc<AtomicBool>,
    release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
}

pub(crate) struct DurablePilotProcessService {
    progress: super::progress::Progress,
    owner: Arc<ProductionWriterAuthority>,
    mission_id: MissionId,
    phase_id: String,
    logical_attempt: u32,
    executable: Mutex<Option<RustPilotProcessExecutable>>,
    executable_id: String,
    cwd_relative: PathBuf,
    environment: Vec<EnvironmentBinding>,
    cancellation: CancellationToken,
    observation: Mutex<Option<Observation>>,
    errors: DurableProcessErrors,
    unresolved: Arc<AtomicBool>,
    release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
}

struct DurableProcessErrors {
    preflight: ProcessServiceError,
    consumed: ProcessServiceError,
    execution: ProcessServiceError,
}

struct RecoveredPhaseProcess {
    classification: RustPilotProcessRecovery,
    evidence: RustPilotRecoveredProcessEvidence,
}

impl DurableProcessOwner {
    fn new(
        authority: Arc<ProductionWriterAuthority>,
        mission_id: MissionId,
        environment: Vec<EnvironmentBinding>,
        executable_ids: BTreeMap<PathBuf, String>,
    ) -> Self {
        Self {
            progress: super::progress::Progress::default(),
            authority,
            mission_id,
            environment,
            executable_ids,
            unresolved: Arc::new(AtomicBool::new(false)),
            release_admission: None,
        }
    }

    fn with_progress(mut self, progress: super::progress::Progress) -> Self {
        self.progress = progress;
        self
    }

    fn with_release_admission(
        mut self,
        release_admission: Arc<dyn RustPilotProcessReleaseAdmission>,
    ) -> Self {
        self.release_admission = Some(release_admission);
        self
    }

    pub(crate) fn service(
        &self,
        phase_id: &str,
        executable: &Path,
        cwd_relative: PathBuf,
        shell_config: Option<PathBuf>,
        cancellation: CancellationToken,
    ) -> Result<DurablePilotProcessService, String> {
        self.progress.stage("process_preparing", Some(phase_id));
        DurablePilotProcessService::new(
            Arc::clone(&self.authority),
            self.mission_id.clone(),
            phase_id,
            1,
            executable,
            cwd_relative,
            self.environment.clone(),
            shell_config,
            cancellation,
            self.executable_ids.get(executable).cloned(),
            Arc::clone(&self.unresolved),
            self.release_admission.clone(),
        )
        .map(|mut service| {
            service.progress = self.progress.clone();
            service
        })
        .map_err(|error| error.to_string())
    }

    fn is_unresolved(&self) -> bool {
        self.unresolved.load(Ordering::Acquire)
    }
}

impl DurablePilotProcessService {
    #[expect(
        clippy::too_many_arguments,
        reason = "durable process admission keeps every authority binding explicit"
    )]
    fn new(
        owner: Arc<ProductionWriterAuthority>,
        mission_id: MissionId,
        phase_id: &str,
        logical_attempt: u32,
        executable: &Path,
        cwd_relative: PathBuf,
        mut environment: Vec<EnvironmentBinding>,
        shell_config: Option<PathBuf>,
        cancellation: CancellationToken,
        expected_executable_id: Option<String>,
        unresolved: Arc<AtomicBool>,
        release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
    ) -> Result<Self, RustPilotProcessError> {
        let executable = RustPilotProcessExecutable::open(executable)?;
        let executable_id = executable.logical_id().to_owned();
        if expected_executable_id
            .as_deref()
            .is_some_and(|expected| !expected.is_empty() && expected != executable_id)
        {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        if let Some(shell_config) = shell_config {
            let shell_config = shell_config
                .to_str()
                .ok_or(RustPilotProcessError::CwdAdmission)?;
            environment.extend([
                EnvironmentBinding {
                    name: "ZDOTDIR".to_owned(),
                    value: shell_config.to_owned(),
                },
                EnvironmentBinding {
                    name: "BASH_ENV".to_owned(),
                    value: "/dev/null".to_owned(),
                },
                EnvironmentBinding {
                    name: "ENV".to_owned(),
                    value: "/dev/null".to_owned(),
                },
            ]);
        }
        let error = |kind, detail: &str| {
            ProcessServiceError::new(kind, detail)
                .map_err(|_| RustPilotProcessError::DispatchFailed)
        };
        Ok(Self {
            progress: super::progress::Progress::default(),
            owner,
            mission_id,
            phase_id: phase_id.to_owned(),
            logical_attempt,
            executable: Mutex::new(Some(executable)),
            executable_id,
            cwd_relative,
            environment,
            cancellation,
            observation: Mutex::new(None),
            errors: DurableProcessErrors {
                preflight: error(
                    ProcessServiceErrorKind::InvalidRequest,
                    "preflight proof did not bind to the durable pilot service",
                )?,
                consumed: error(
                    ProcessServiceErrorKind::InvalidRequest,
                    "durable pilot process service was already consumed",
                )?,
                execution: error(
                    ProcessServiceErrorKind::OutcomeIndeterminate,
                    "durable pilot process execution or recovery was unresolved",
                )?,
            },
            unresolved,
            release_admission,
        })
    }

    fn exact_request(
        &self,
        request: &ProcessRequest,
    ) -> Result<ProcessRequest, ProcessServiceError> {
        let mut exact = ProcessRequest::new(
            request.purpose(),
            request.executable_id(),
            request.working_root(),
        )
        .map_err(|_| self.errors.execution.clone())?;
        for argument in request.expose_arguments() {
            exact = exact
                .with_argument(argument)
                .map_err(|_| self.errors.execution.clone())?;
        }
        for (name, value) in request.expose_environment() {
            exact = exact
                .with_environment(name, value)
                .map_err(|_| self.errors.execution.clone())?;
        }
        if let Some(stdin) = request.expose_stdin() {
            exact = exact
                .with_stdin(stdin.to_vec())
                .map_err(|_| self.errors.execution.clone())?;
        }
        exact = exact
            .with_max_output_bytes(request.max_output_bytes())
            .map_err(|_| self.errors.execution.clone())?;
        if request.truncated_output_acknowledged() {
            exact = exact.with_truncated_output_acknowledged();
        }
        Ok(exact)
    }

    fn open(
        &self,
        request: &ProcessRequest,
    ) -> Result<(RustPilotProcessOpen, ProcessRequest), ProcessServiceError> {
        let exact = self.exact_request(request)?;
        let executable = lock(&self.executable)
            .take()
            .ok_or_else(|| self.errors.consumed.clone())?;
        let attempt = RustPilotProcessAttempt::new(
            self.mission_id.clone(),
            &self.phase_id,
            self.logical_attempt,
        )
        .map_err(|_| self.errors.execution.clone())?;
        let opened = RustPilotProcessSession::open_with_cancellation_and_admission(
            &self.owner,
            attempt,
            &exact,
            executable,
            self.cwd_relative.clone(),
            self.cancellation.clone(),
            self.release_admission.clone(),
        )
        .map_err(|_| self.errors.execution.clone())?;
        Ok((opened, exact))
    }

    fn recover(&self, request: &ProcessRequest) -> Result<RecoveredPhaseProcess, String> {
        let (opened, exact) = self.open(request).map_err(|error| error.to_string())?;
        let classification = match opened {
            RustPilotProcessOpen::Ready(session) => {
                session.close().map_err(|error| error.to_string())?;
                RustPilotProcessRecovery::RecoveredBeforeStartNotStarted
            }
            RustPilotProcessOpen::Recovered(RustPilotProcessRecovery::PreviouslyResolved) => {
                RustPilotProcessRecovery::ReleasedLostOutcomeUncertain
            }
            RustPilotProcessOpen::Recovered(recovery) => recovery,
        };
        let attempt = RustPilotProcessAttempt::new(
            self.mission_id.clone(),
            &self.phase_id,
            self.logical_attempt,
        )
        .map_err(|error| error.to_string())?;
        let evidence =
            RustPilotProcessSession::recovered_metric_evidence(&self.owner, attempt, &exact)
                .map_err(|error| error.to_string())?;
        Ok(RecoveredPhaseProcess {
            classification,
            evidence,
        })
    }
}

impl Cancellation for DurablePilotProcessService {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl ProcessService for DurablePilotProcessService {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        let preflight = preflight
            .bind(self, request)
            .ok_or_else(|| self.errors.preflight.clone())?;
        let (opened, _) = self.open(request)?;
        match opened {
            RustPilotProcessOpen::Ready(session) => {
                session.finish_preflight(request, preflight).map_err(|_| {
                    self.unresolved.store(true, Ordering::Release);
                    self.errors.execution.clone()
                })
            }
            RustPilotProcessOpen::Recovered(_) => {
                self.unresolved.store(true, Ordering::Release);
                Err(self.errors.execution.clone())
            }
        }
    }

    fn execute(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        let (opened, exact) = self.open(request)?;
        let RustPilotProcessOpen::Ready(session) = opened else {
            self.unresolved.store(true, Ordering::Release);
            return Err(self.errors.execution.clone());
        };
        let executed = if self.progress.enabled() {
            self.progress
                .stage("process_dispatch", Some(&self.phase_id));
            self.progress
                .forward(&self.phase_id, |output| {
                    session.execute_with_output(&exact, budget, output)
                })
                .map_err(|_| {
                    self.unresolved.store(true, Ordering::Release);
                    self.errors.execution.clone()
                })?
        } else {
            session.execute(&exact, budget)
        };
        let execution = executed.map_err(|_| {
            self.unresolved.store(true, Ordering::Release);
            self.errors.execution.clone()
        })?;
        let receipt = execution.receipt().clone();
        *lock(&self.observation) = Some(durable_observation(&exact, &execution));
        Ok(receipt)
    }
}

impl ObservedProcessService for DurablePilotProcessService {
    fn executable_id(&self) -> &str {
        &self.executable_id
    }

    fn process_environment(&self) -> Vec<(String, String)> {
        self.environment
            .iter()
            .map(|binding| (binding.name.clone(), binding.value.clone()))
            .collect()
    }

    fn take_observation(&self) -> Option<Observation> {
        lock(&self.observation).take()
    }
}

fn durable_observation(
    request: &ProcessRequest,
    execution: &orchestrator_app::RustPilotProcessExecution,
) -> Observation {
    let receipt = execution.receipt();
    let identity = execution.post_release_identity().map(|identity| {
        json!({
            "pid": identity.pid(),
            "process_group_id": identity.process_group_id(),
            "process_start_identity": identity.process_start_identity(),
        })
    });
    let absence = execution.group_absence().map(|absence| {
        json!({
            "pid": absence.pid(),
            "process_group_id": absence.process_group_id(),
            "process_start_identity": absence.process_start_identity(),
        })
    });
    Observation {
        stdout: receipt.expose_stdout().to_vec(),
        stderr: receipt.expose_stderr().to_vec(),
        summary: json!({
            "argv_after_executable": request.expose_arguments(),
            "working_root": request.working_root(),
            "termination": format!("{:?}", receipt.termination()),
            "not_started_reason": execution.not_started_reason().map(|reason| format!("{reason:?}")),
            "cleanup_complete": receipt.ownership_released(),
            "stdout_bytes": receipt.expose_stdout().len(),
            "stderr_bytes": receipt.expose_stderr().len(),
            "stdout_discarded_bytes": receipt.stdout_discarded(),
            "stderr_discarded_bytes": receipt.stderr_discarded(),
            "elapsed_ms": u64::try_from(receipt.elapsed().as_millis()).unwrap_or(u64::MAX),
            "post_release_identity": identity,
            "group_absence": absence,
        }),
    }
}

struct Replayed {
    state: MissionState,
    results: BTreeMap<usize, Value>,
    previous_digest: String,
    next_sequence: i64,
    terminal: Option<(bool, &'static str, String, PathBuf)>,
    expected_digests: Option<(String, String)>,
    last_sequence: i64,
    last_timestamp: String,
    last_kind: RecordKind,
    first_timestamp: String,
    cancellation_requested: bool,
}

struct InspectionRoot {
    requested: PathBuf,
    canonical: PathBuf,
    root: File,
    root_identity: (u64, u64),
    journal: File,
    journal_identity: (u64, u64),
}

trait StatusReadObserver {
    fn after_retention(&mut self, _root: &InspectionRoot) -> Result<(), PilotError> {
        Ok(())
    }
    fn before_enumeration(
        &mut self,
        _root: &InspectionRoot,
        _observation: usize,
    ) -> Result<(), PilotError> {
        Ok(())
    }
    fn after_enumeration(
        &mut self,
        _root: &InspectionRoot,
        _observation: usize,
    ) -> Result<(), PilotError> {
        Ok(())
    }
    fn before_record_read(
        &mut self,
        _root: &InspectionRoot,
        _observation: usize,
        _index: usize,
    ) -> Result<(), PilotError> {
        Ok(())
    }
    fn after_record_read(
        &mut self,
        _root: &InspectionRoot,
        _observation: usize,
        _index: usize,
    ) -> Result<(), PilotError> {
        Ok(())
    }
}

struct ProductionStatusReadObserver;

impl StatusReadObserver for ProductionStatusReadObserver {}

pub(crate) fn status(options: &PilotOptions) -> Result<Value, PilotError> {
    status_with_observer(options, &mut ProductionStatusReadObserver)
}

fn status_with_observer(
    options: &PilotOptions,
    observer: &mut impl StatusReadObserver,
) -> Result<Value, PilotError> {
    let retained = InspectionRoot::open(&options.output_dir)?;
    observer.after_retention(&retained)?;
    let manifest_path = retained.canonical.join("manifest.json");
    let manifest_bytes = retained.read_root_file(
        Path::new("manifest.json"),
        &manifest_path,
        MAX_MANIFEST_BYTES,
    )?;
    let manifest: DurableManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| {
        PilotError::Composition("durable manifest is malformed or not closed-schema".to_owned())
    })?;
    validate_manifest_contract(&manifest, &retained.canonical)?;
    if manifest.output_directory != binding(retained.root_identity) {
        return Err(PilotError::Composition(
            "durable output directory binding is mismatched".to_owned(),
        ));
    }
    let mission = authored_cycle::parse_mission(&manifest.mission_text).map_err(|error| {
        PilotError::Composition(format!("saved mission no longer compiles: {error}"))
    })?;
    let inspected_options = options_from_manifest(&manifest, &retained.canonical)?;
    validate_manifest_derived(&manifest, &mission, &inspected_options)?;
    let replayed = replay_inspected(
        &manifest,
        &mission,
        &retained,
        &sha256(&manifest_bytes),
        observer,
    )?;
    let recorded_status = replayed
        .terminal
        .as_ref()
        .map(|(_, status, _, _)| *status)
        .or_else(|| {
            replayed
                .cancellation_requested
                .then_some("cancellation_requested")
        })
        .or_else(|| (replayed.last_kind == RecordKind::Paused).then_some("paused"))
        .or_else(|| {
            replayed
                .state
                .phases()
                .any(|phase| phase.status == PhaseStatus::Running)
                .then_some("unresolved")
        })
        .unwrap_or("in_progress");
    let phases = mission.execution_order.iter().map(|index| {
        let phase = &mission.phases[*index];
        let state = replayed.state.phase(&phase.id);
        let route = &manifest.routes[*index];
        json!({
            "name": phase.name,
            "role": mission.roles[*index].as_str(),
            "recorded_status": state.map(|value| phase_status(value.status)).unwrap_or("unknown"),
            "model": route.model,
            "effort": route.effort,
        })
    }).collect::<Vec<_>>();
    let metrics_mission = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let metrics = read_rust_pilot_metrics(
        &retained.canonical,
        env!("CARGO_PKG_VERSION"),
        &metrics_mission,
        mission
            .phases
            .len()
            .clamp(1, orchestrator_app::MAX_QUERY_LIMIT),
    )
    .map_err(|error| PilotError::Composition(format!("metrics view refused: {error}")))?;
    retained.verify()?;
    Ok(json!({
        "schema": "nanika.rust-first-use-durable-status.v1",
        "mission_id": manifest.mission_id,
        "mission_digest": manifest.mission_digest,
        "recorded_status": recorded_status,
        "last_journal_sequence": replayed.last_sequence,
        "last_journal_timestamp": replayed.last_timestamp,
        "total_phase_count": mission.phases.len(),
        "completed_phase_count": replayed.state.phases().filter(|phase| phase.status == PhaseStatus::Completed).count(),
        "failed_phase_count": replayed.state.phases().filter(|phase| phase.status == PhaseStatus::Failed).count(),
        "pending_phase_count": replayed.state.phases().filter(|phase| phase.status == PhaseStatus::Pending).count(),
        "skipped_phase_count": replayed.state.phases().filter(|phase| phase.status == PhaseStatus::Skipped).count(),
        "unresolved_phase_count": replayed.state.phases().filter(|phase| phase.status == PhaseStatus::Running).count(),
        "phases": phases,
        "metrics": metrics,
        "execution_liveness": "not_checked",
        "durability": "not_attested"
    }))
}

pub(crate) fn run(
    options: &PilotOptions,
    cancellation: CancellationToken,
    progress: super::progress::Progress,
) -> Result<PilotSummary, PilotError> {
    progress.stage("preparing_snapshot", None);
    let source = read_prompt_with_limit(&options.prompt_file, MAX_CODE_PROMPT_BYTES)?;
    let mission = authored_cycle::parse_mission(&source).map_err(PilotError::Composition)?;
    validate_stop_phase(options.stop_after_phase.as_deref(), &mission)?;
    let root = resolve_output_root(&options.output_dir)?;
    if root.symlink_metadata().is_ok() {
        return Err(output_error(
            &root,
            "already exists; durable run requires a fresh directory",
        ));
    }
    let repo = options
        .repo
        .as_deref()
        .ok_or_else(|| PilotError::Composition("durable authored run lost --repo".to_owned()))?;
    let canonical_repo = fs::canonicalize(repo).map_err(|error| PilotError::Repository {
        path: repo.to_path_buf(),
        reason: error.to_string(),
    })?;
    let authority = acquire(&root, &canonical_repo)?;
    let mut store = DurableStore::fresh(root.clone(), authority)?;
    store.progress = progress;
    store.create_dir(Path::new("shell-config"))?;
    store.create_dir(Path::new("journal"))?;

    let supervisor = ProcessSupervisor::process_wide()
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let snapshot = snapshot::prepare(&canonical_repo, &root, &supervisor, &cancellation)
        .map_err(PilotError::Composition)?;
    create_phase_directories(&mut store, &mission)?;
    let mut durable_options = options.clone();
    durable_options.codex_executable = resolve_executable(&options.codex_executable)?;
    let verifier = durable_options
        .verification_argv
        .first()
        .ok_or_else(|| PilotError::Composition("verification argv is empty".to_owned()))?;
    durable_options.verification_argv[0] = resolve_executable(verifier)?;
    let version = probe_runtime_version(
        &durable_options,
        &snapshot.workspace,
        &cancellation,
        &supervisor,
    )?;
    let manifest = build_manifest(
        &durable_options,
        &source,
        &mission,
        &root,
        &snapshot,
        &version,
    )?;
    let manifest_bytes = json_bytes(&root.join("manifest.json"), &manifest)?;
    store.create_file(
        Path::new("manifest.json"),
        &manifest_bytes,
        MAX_MANIFEST_BYTES as usize,
    )?;
    store.previous_digest = sha256(&manifest_bytes);

    let mission_id = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let mut state = initial_state(&mission_id, &mission)?;
    let started = store.append(record(
        &manifest.mission_id,
        0,
        RecordKind::MissionStarted,
        None,
        None,
        None,
        None,
        None,
        None,
    ))?;
    state = apply(&state, &started)?;
    execute(
        &durable_options,
        &manifest,
        &mission,
        &snapshot,
        &supervisor,
        &cancellation,
        store,
        state,
        BTreeMap::new(),
    )
}

pub(crate) fn resume(
    options: &PilotOptions,
    cancellation: CancellationToken,
    progress: super::progress::Progress,
) -> Result<PilotSummary, PilotError> {
    progress.stage("reading_saved_run", None);
    let requested = options.output_dir.symlink_metadata().map_err(|error| {
        output_error(
            &options.output_dir,
            &format!("cannot inspect durable run: {error}"),
        )
    })?;
    if requested.file_type().is_symlink() || !requested.is_dir() {
        return Err(output_error(
            &options.output_dir,
            "durable run must be a real directory, not a symlink",
        ));
    }
    let root = fs::canonicalize(&options.output_dir).map_err(|error| {
        output_error(
            &options.output_dir,
            &format!("cannot open durable run: {error}"),
        )
    })?;
    if let Some(checkout) = root
        .ancestors()
        .find(|directory| directory.join(".git").symlink_metadata().is_ok())
    {
        return Err(output_error(
            &options.output_dir,
            &format!("is inside Git checkout {}", checkout.display()),
        ));
    }
    let manifest_path = root.join("manifest.json");
    let manifest_bytes = read_bounded_regular(&manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest: DurableManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| {
        PilotError::Composition("durable manifest is malformed or not closed-schema".to_owned())
    })?;
    validate_manifest_for_resume(&manifest, &root)?;
    let source_path = PathBuf::from(&manifest.source.canonical_path);
    let authority = acquire(&root, &source_path)?;
    let retained_manifest = read_bounded_regular(&manifest_path, MAX_MANIFEST_BYTES)?;
    if retained_manifest != manifest_bytes {
        return Err(PilotError::Composition(
            "durable manifest changed during private writer admission".to_owned(),
        ));
    }
    validate_manifest_for_resume(&manifest, &root)?;
    let manifest_digest = sha256(&manifest_bytes);
    let mission = authored_cycle::parse_mission(&manifest.mission_text).map_err(|error| {
        PilotError::Composition(format!("saved mission no longer compiles: {error}"))
    })?;
    let resumed_options = options_from_manifest(&manifest, &root)?;
    validate_manifest_derived(&manifest, &mission, &resumed_options)?;
    let replayed = replay(&manifest, &mission, &root, &manifest_digest)?;
    if let Some((completed, status, reason, result_path)) = replayed.terminal {
        return Ok(PilotSummary {
            completed,
            status,
            reason,
            result_path,
        });
    }
    let mut store = DurableStore::reopen(root.clone(), authority, manifest_digest)?;
    store.progress = progress;
    store.mission_started_at = Some(replayed.first_timestamp.clone());
    store.cancellation_requested = replayed.cancellation_requested;
    // Older ambient runs have no process admission proof. An empty new ledger
    // cannot establish that their interrupted child never started.
    if (manifest.provider.executable_id.is_empty() || manifest.verifier.executable_id.is_empty())
        && replayed
            .state
            .phases()
            .any(|phase| phase.status == PhaseStatus::Running)
    {
        return Err(PilotError::Composition(
            "recovery required: legacy interrupted phase has no durable process admission proof; refusing redispatch".to_owned(),
        ));
    }
    let mission_id = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let supervisor = ProcessSupervisor::process_wide()
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let snapshot = snapshot::RepositorySnapshot::reopen(
        source_path,
        manifest.source.head.clone(),
        PathBuf::from(&manifest.snapshot.baseline),
        PathBuf::from(&manifest.snapshot.workspace),
        (
            manifest.snapshot.baseline_directory.device,
            manifest.snapshot.baseline_directory.inode,
        ),
        (
            manifest.snapshot.workspace_directory.device,
            manifest.snapshot.workspace_directory.inode,
        ),
        &supervisor,
    )
    .map_err(PilotError::Composition)?;
    if let Some(authoritative) =
        store
            .metrics
            .terminal_transition(&mission_id)
            .map_err(|error| {
                PilotError::Composition(format!("terminal metric recovery failed: {error}"))
            })?
    {
        let recovered: JournalRecord = serde_json::from_value(authoritative).map_err(|_| {
            PilotError::Composition(
                "authoritative mission terminal is malformed or foreign".to_owned(),
            )
        })?;
        if recovered.kind != RecordKind::MissionTerminal
            || recovered.sequence != 0
            || !recovered.event_id.is_empty()
            || recovered.phase_id.is_some()
            || recovered.role.is_some()
            || recovered.mission_id != manifest.mission_id
        {
            return Err(PilotError::Composition(
                "authoritative mission terminal binding is invalid".to_owned(),
            ));
        }
        validate_record_shape(&recovered)?;
        if replayed
            .state
            .phases()
            .any(|phase| phase.status == PhaseStatus::Running)
        {
            return Err(PilotError::Composition(
                "authoritative mission terminal conflicts with an unresolved phase".to_owned(),
            ));
        }
        snapshot
            .verify_source(&supervisor)
            .map_err(PilotError::Composition)?;
        let observed = snapshot.state_digests().map_err(PilotError::Composition)?;
        if recovered.baseline_digest.as_deref() != Some(observed.0.as_str())
            || recovered.workspace_digest.as_deref() != Some(observed.1.as_str())
        {
            return Err(PilotError::Composition(
                "workspace changed after the authoritative mission terminal".to_owned(),
            ));
        }
        let mut preview = recovered.clone();
        preview.sequence = replayed.next_sequence;
        preview.event_id = format!("durable-event-{:06}", replayed.next_sequence);
        let preview_state = apply(&replayed.state, &preview)?;
        let (completed, status, reason) = validate_mission_terminal(
            &preview,
            &mission,
            &preview_state,
            &replayed.results,
            replayed.cancellation_requested,
            false,
        )?;
        store.previous_digest = replayed.previous_digest;
        store.next_sequence = replayed.next_sequence;
        let terminal = store.append(recovered)?;
        return Ok(PilotSummary {
            completed,
            status,
            reason,
            result_path: record_path(&store.root, terminal.sequence),
        });
    }
    if let Some(index) = mission.execution_order.iter().copied().find(|index| {
        replayed
            .state
            .phase(&mission.phases[*index].id)
            .is_some_and(|phase| phase.status == PhaseStatus::Running)
    }) {
        let phase = &mission.phases[index];
        if let Some(authoritative) = store
            .metrics
            .phase_transition(&mission_id, phase.name.as_str())
            .map_err(|error| {
                PilotError::Composition(format!("phase metric recovery failed: {error}"))
            })?
        {
            let recovered: JournalRecord = serde_json::from_value(authoritative).map_err(|_| {
                PilotError::Composition(
                    "authoritative phase transition is malformed or foreign".to_owned(),
                )
            })?;
            if recovered.kind != RecordKind::PhaseTerminal
                || recovered.sequence != 0
                || !recovered.event_id.is_empty()
                || recovered.phase_id.as_deref() != Some(phase.id.as_str())
            {
                return Err(PilotError::Composition(
                    "authoritative phase transition binding is invalid".to_owned(),
                ));
            }
            snapshot
                .verify_source(&supervisor)
                .map_err(PilotError::Composition)?;
            let observed = snapshot.state_digests().map_err(PilotError::Composition)?;
            if recovered.baseline_digest.as_deref() != Some(observed.0.as_str())
                || recovered.workspace_digest.as_deref() != Some(observed.1.as_str())
            {
                return Err(PilotError::Composition(
                    "workspace changed after the authoritative phase transition".to_owned(),
                ));
            }
            store.previous_digest = replayed.previous_digest;
            store.next_sequence = replayed.next_sequence;
            let persisted = store.append(recovered)?;
            let state = apply(&replayed.state, &persisted)?;
            let mut results = replayed.results;
            let result = persisted.result.clone().ok_or_else(|| {
                PilotError::Composition(
                    "authoritative phase transition lost its terminal result".to_owned(),
                )
            })?;
            results.insert(index, result);
            return execute(
                &resumed_options,
                &manifest,
                &mission,
                &snapshot,
                &supervisor,
                &cancellation,
                store,
                state,
                results,
            );
        }
        store.previous_digest = replayed.previous_digest;
        store.next_sequence = replayed.next_sequence;
        let execution_environment = match &manifest.execution_environment {
            Some(environment) => environment.clone(),
            None => capture_execution_environment()?,
        };
        let owner = DurableProcessOwner::new(
            Arc::clone(&store.authority),
            mission_id,
            execution_environment,
            executable_ids(&manifest),
        )
        .with_progress(store.progress.clone());
        let recovery = recover_interrupted_process(
            &resumed_options,
            &mission,
            index,
            &snapshot,
            &owner,
            &cancellation,
            &supervisor,
        )
        .map_err(|error| {
            PilotError::Composition(format!(
                "recovery required: interrupted durable process is unresolved: {error}"
            ))
        })?;
        let role = mission.roles[index];
        let result = recovery_result(phase, role, &recovery);
        if result
            .pointer("/process/cleanup_complete")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(PilotError::Composition(
                "recovery required: interrupted durable process cleanup is unresolved".to_owned(),
            ));
        }
        snapshot
            .verify_source(&supervisor)
            .map_err(PilotError::Composition)?;
        let digests = snapshot.state_digests().map_err(PilotError::Composition)?;
        let terminal_record = record(
            &manifest.mission_id,
            0,
            RecordKind::PhaseTerminal,
            Some(phase.id.as_str()),
            Some(role.as_str()),
            Some("failed"),
            result["reason"].as_str(),
            Some(result.clone()),
            Some(&digests),
        );
        let authoritative = serde_json::to_value(&terminal_record)
            .map_err(|error| PilotError::Composition(error.to_string()))?;
        if recovery.evidence.group_absence().is_some() {
            let metric = phase_metric_intent(&manifest, phase, role, &result)?;
            store
                .metrics
                .record_phase(&metric, &authoritative, &terminal_record.timestamp)
                .map_err(|error| {
                    PilotError::Composition(format!("phase metric failed: {error}"))
                })?;
        }
        let terminal = store.append(terminal_record)?;
        let state = apply(&replayed.state, &terminal)?;
        let mut results = replayed.results;
        results.insert(index, result);
        return execute(
            &resumed_options,
            &manifest,
            &mission,
            &snapshot,
            &supervisor,
            &cancellation,
            store,
            state,
            results,
        );
    }
    let observed = snapshot.state_digests().map_err(PilotError::Composition)?;
    let expected = replayed.expected_digests.unwrap_or_else(|| {
        (
            manifest.snapshot.baseline_digest.clone(),
            manifest.snapshot.workspace_digest.clone(),
        )
    });
    if observed != expected {
        return Err(PilotError::Composition(
            "saved baseline or workspace changed after the last durable phase boundary".to_owned(),
        ));
    }
    if !replayed.cancellation_requested {
        let version = probe_runtime_version(
            &resumed_options,
            &snapshot.workspace,
            &cancellation,
            &supervisor,
        )?;
        if version != manifest.provider.version {
            return Err(PilotError::Composition(format!(
                "provider version changed from {:?} to {:?}",
                manifest.provider.version, version
            )));
        }
    }
    store.previous_digest = replayed.previous_digest;
    store.next_sequence = replayed.next_sequence;
    execute(
        &resumed_options,
        &manifest,
        &mission,
        &snapshot,
        &supervisor,
        &cancellation,
        store,
        replayed.state,
        replayed.results,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the durable boundary is intentionally explicit"
)]
fn execute(
    options: &PilotOptions,
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    snapshot: &snapshot::RepositorySnapshot,
    supervisor: &ProcessSupervisor,
    cancellation: &CancellationToken,
    store: DurableStore,
    mut state: MissionState,
    mut results: BTreeMap<usize, Value>,
) -> Result<PilotSummary, PilotError> {
    let mission_id = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let execution_environment = match &manifest.execution_environment {
        Some(environment) => environment.clone(),
        None => capture_execution_environment()?,
    };
    let store = Arc::new(Mutex::new(store));
    let (authority, progress, root) = {
        let retained = lock(&store);
        (
            Arc::clone(&retained.authority),
            retained.progress.clone(),
            retained.root.clone(),
        )
    };
    let release_admission: Arc<dyn RustPilotProcessReleaseAdmission> =
        Arc::new(DurableReleaseAdmission {
            store: Arc::clone(&store),
        });
    let process_owner = DurableProcessOwner::new(
        authority,
        mission_id.clone(),
        execution_environment,
        executable_ids(manifest),
    )
    .with_progress(progress)
    .with_release_admission(release_admission);
    let _daemon = super::cancellation::start(
        options,
        Box::new(DurableCancellationRecorder {
            store: Arc::clone(&store),
            mission_id: manifest.mission_id.clone(),
            cancellation: cancellation.clone(),
        }),
        cancellation.clone(),
    )
    .map_err(|error| PilotError::Composition(format!("cancellation control refused: {error}")))?;
    let environment = PhaseEnvironment {
        options,
        snapshot,
        observed_version: &manifest.provider.version,
        cancellation,
        supervisor,
        shell_config: Some(root.join("shell-config")),
        durable_processes: Some(&process_owner),
    };
    loop {
        if lock(&store).cancellation_requested {
            return finish_cancelled(
                manifest,
                mission,
                snapshot,
                &store,
                &state,
                &results,
                &mission_id,
            );
        }
        let index = match authored_cycle::next_phase_action(mission, &results) {
            authored_cycle::PhaseAction::Complete => break,
            authored_cycle::PhaseAction::Skip {
                index,
                failed_dependencies: _,
            } => {
                let phase = &mission.phases[index];
                let phase_state = state.phase(&phase.id).ok_or_else(|| {
                    PilotError::Composition("compiled phase disappeared".to_owned())
                })?;
                if phase_state.status != PhaseStatus::Skipped {
                    return Err(PilotError::Composition(
                        "core reducer and authored dependency selection disagree on a skipped phase"
                            .to_owned(),
                    ));
                }
                let skipped = skipped_record(phase, mission.roles[index], mission, &state);
                let digests = snapshot.state_digests().map_err(PilotError::Composition)?;
                let persisted = lock(&store).append(record(
                    &manifest.mission_id,
                    0,
                    RecordKind::PhaseSkippedObservation,
                    Some(phase.id.as_str()),
                    Some(mission.roles[index].as_str()),
                    Some("skipped"),
                    skipped["reason"].as_str(),
                    Some(skipped.clone()),
                    Some(&digests),
                ))?;
                state = apply(&state, &persisted)?;
                results.insert(index, skipped);
                continue;
            }
            authored_cycle::PhaseAction::Execute(index) => index,
        };
        let phase = &mission.phases[index];
        let phase_state = state
            .phase(&phase.id)
            .ok_or_else(|| PilotError::Composition("compiled phase disappeared".to_owned()))?;
        if phase_state.status != PhaseStatus::Pending {
            return Err(PilotError::Composition(
                "core reducer and authored phase selection disagree on the next phase".to_owned(),
            ));
        }

        let started = {
            let mut retained = lock(&store);
            retained.verify()?;
            if retained.cancellation_requested {
                drop(retained);
                return finish_cancelled(
                    manifest,
                    mission,
                    snapshot,
                    &store,
                    &state,
                    &results,
                    &mission_id,
                );
            }
            retained.append(record(
                &manifest.mission_id,
                0,
                RecordKind::PhaseStarted,
                Some(phase.id.as_str()),
                Some(mission.roles[index].as_str()),
                Some("started"),
                None,
                None,
                None,
            ))?
        };
        state = apply(&state, &started)?;
        if let Some(route) = manifest
            .routes
            .iter()
            .find(|route| route.phase_id == phase.id.as_str())
        {
            lock(&store).progress.emit(json!({"kind":"phase_route", "phase_id":phase.id.as_str(), "phase":phase.name, "model":route.model, "effort":route.effort, "runtime":route.runtime}));
        }

        let outcome = match mission.roles[index] {
            PhaseRole::Code => authored_cycle::run_code_phase(
                phase,
                &authored_cycle::phase_directory(&root, index, phase),
                &environment,
            ),
            PhaseRole::Review => authored_cycle::run_review_phase(
                mission,
                index,
                &authored_cycle::phase_directory(&root, index, phase),
                &environment,
            ),
            PhaseRole::Verification => authored_cycle::run_verification_phase(
                phase,
                &authored_cycle::phase_directory(&root, index, phase),
                &environment,
            ),
        };
        if process_owner.is_unresolved() {
            return Err(PilotError::Composition(
                "recovery required: durable process outcome or cleanup is unresolved".to_owned(),
            ));
        }
        let preparation_refusal = outcome
            .as_ref()
            .err()
            .is_some_and(|failure| failure.is_preparation_refusal());
        let mut result = outcome.unwrap_or_else(|failure| {
            failure.record.unwrap_or_else(|| {
                json!({
                    "phase": phase.name,
                    "id": phase.id.as_str(),
                    "role": mission.roles[index].as_str(),
                    "persona": phase.persona,
                    "objective": phase.objective,
                    "status": "failed",
                    "reason": failure.reason,
                    "skipped_dependencies": [],
                    "provider_dispatched": false,
                })
            })
        });
        if let Err(reason) = snapshot.verify_source(supervisor) {
            result["status"] = Value::String("failed".to_owned());
            result["reason"] = Value::String(reason);
            result["source_repository_preserved"] = Value::Bool(false);
        }
        let not_started = confirmed_not_started_reason(&result).map(str::to_owned);
        if let Some(reason) = &not_started {
            result["status"] = json!("failed");
            result["provider_dispatched"] = json!(false);
            result["reason"] = json!(format!(
                "process confirmed not started ({reason}): {}",
                result["reason"]
                    .as_str()
                    .unwrap_or("provider launch failed")
            ));
        }
        let passed = result["status"] == "passed";
        let digests = snapshot.state_digests().map_err(PilotError::Composition)?;
        let terminal_record = record(
            &manifest.mission_id,
            0,
            RecordKind::PhaseTerminal,
            Some(phase.id.as_str()),
            Some(mission.roles[index].as_str()),
            Some(if passed { "passed" } else { "failed" }),
            result["reason"].as_str(),
            Some(result.clone()),
            Some(&digests),
        );
        // A trusted preparation refusal or a durably confirmed no-start has no
        // executed-phase metric. Preserve its failed terminal in the journal.
        // Missing observations after entering execution still require recovery.
        if !preparation_refusal && not_started.is_none() {
            let metric = phase_metric_intent(manifest, phase, mission.roles[index], &result)?;
            let authoritative = serde_json::to_value(&terminal_record)
                .map_err(|error| PilotError::Composition(error.to_string()))?;
            lock(&store)
                .metrics
                .record_phase(&metric, &authoritative, &terminal_record.timestamp)
                .map_err(|error| {
                    PilotError::Composition(format!("phase metric failed: {error}"))
                })?;
        }
        let terminal = lock(&store).append(terminal_record)?;
        state = apply(&state, &terminal)?;
        results.insert(index, result);

        if lock(&store).cancellation_requested {
            return finish_cancelled(
                manifest,
                mission,
                snapshot,
                &store,
                &state,
                &results,
                &mission_id,
            );
        }

        if passed && options.stop_after_phase.as_deref() == Some(phase.name.as_str()) {
            let mut retained = lock(&store);
            if retained.cancellation_requested {
                drop(retained);
                return finish_cancelled(
                    manifest,
                    mission,
                    snapshot,
                    &store,
                    &state,
                    &results,
                    &mission_id,
                );
            }
            let pause = retained.append(record(
                &manifest.mission_id,
                0,
                RecordKind::Paused,
                Some(phase.id.as_str()),
                Some(mission.roles[index].as_str()),
                Some("paused"),
                Some("requested durable phase boundary reached"),
                None,
                Some(&digests),
            ))?;
            return Ok(PilotSummary {
                completed: false,
                status: "paused",
                reason: "requested durable phase boundary reached".to_owned(),
                result_path: record_path(&root, pause.sequence),
            });
        }
    }

    let failed = state
        .phases()
        .any(|phase| phase.status == PhaseStatus::Failed);
    let all_passed = state
        .phases()
        .all(|phase| phase.status == PhaseStatus::Completed);
    let completed = all_passed && !failed;
    let reason = if completed {
        "completed".to_owned()
    } else {
        results
            .values()
            .find(|value| value["status"] == "failed")
            .and_then(|value| value["reason"].as_str())
            .unwrap_or("one or more authored phases did not pass")
            .to_owned()
    };
    let digests = snapshot.state_digests().map_err(PilotError::Composition)?;
    let ordered_results = ordered_results(mission, &state, &results);
    let summary = json!({
        "schema": RESULT_SCHEMA,
        "command": "resume-capable-run",
        "input": "authored-mission",
        "status": if completed { "completed" } else { "failed" },
        "completed": completed,
        "reason": reason,
        "phases": ordered_results,
        "provider_completed": ordered_results.iter().filter(|phase| phase["role"] != "verification").all(|phase| phase["status"] == "passed"),
        "tests_verified": ordered_results.iter().filter(|phase| phase["role"] == "verification").all(|phase| phase["status"] == "passed"),
    });
    let terminal_record = record(
        &manifest.mission_id,
        0,
        RecordKind::MissionTerminal,
        None,
        None,
        Some(if completed { "completed" } else { "failed" }),
        Some(&reason),
        Some(summary),
        Some(&digests),
    );
    let started_at = lock(&store).mission_started_at.clone().ok_or_else(|| {
        PilotError::Composition("durable mission start timestamp is missing".to_owned())
    })?;
    let duration_s = elapsed_rfc3339_seconds(&started_at, &terminal_record.timestamp)?;
    let metric = TerminalMetricIntent {
        mission: manifest.mission_id.clone(),
        domain: "local-pilot".to_owned(),
        task: manifest.mission_text.clone(),
        started_at,
        finished_at: terminal_record.timestamp.clone(),
        duration_s,
        status: if completed {
            "completed".to_owned()
        } else {
            "failed".to_owned()
        },
        decomp_source: "authored-phase-lines".to_owned(),
    };

    let mut retained = lock(&store);
    if retained.cancellation_requested {
        drop(retained);
        return finish_cancelled(
            manifest,
            mission,
            snapshot,
            &store,
            &state,
            &results,
            &mission_id,
        );
    }
    let terminal = retained.publish_terminal(&mission_id, &metric, terminal_record)?;
    drop(retained);
    state = apply(&state, &terminal)?;
    if completed && state.status() != MissionStatus::Completed
        || !completed && state.status() != MissionStatus::Failed
    {
        return Err(PilotError::Composition(
            "core reducer disagreed with durable terminal outcome".to_owned(),
        ));
    }
    Ok(PilotSummary {
        completed,
        status: if completed { "completed" } else { "failed" },
        reason,
        result_path: record_path(&root, terminal.sequence),
    })
}

fn finish_cancelled(
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    snapshot: &snapshot::RepositorySnapshot,
    store: &SharedDurableStore,
    state: &MissionState,
    results: &BTreeMap<usize, Value>,
    mission_id: &MissionId,
) -> Result<PilotSummary, PilotError> {
    if state
        .phases()
        .any(|phase| phase.status == PhaseStatus::Running)
    {
        return Err(PilotError::Composition(
            "recovery required: cancelled mission still has an unresolved phase".to_owned(),
        ));
    }
    let digests = snapshot.state_digests().map_err(PilotError::Composition)?;
    let mut terminal_record = record(
        &manifest.mission_id,
        0,
        RecordKind::MissionTerminal,
        None,
        None,
        Some("cancelled"),
        Some(CANCELLATION_TERMINAL_REASON),
        None,
        Some(&digests),
    );
    let (started_at, next_sequence) = {
        let retained = lock(store);
        let started_at = retained.mission_started_at.clone().ok_or_else(|| {
            PilotError::Composition("durable mission start timestamp is missing".to_owned())
        })?;
        (started_at, retained.next_sequence)
    };
    let mut preview = terminal_record.clone();
    preview.sequence = next_sequence;
    preview.event_id = format!("durable-event-{next_sequence:06}");
    let terminal_state = apply(state, &preview)?;
    let ordered = ordered_results(mission, &terminal_state, results);
    terminal_record.result = Some(json!({
        "schema": RESULT_SCHEMA,
        "command": "resume-capable-run",
        "input": "authored-mission",
        "status": "cancelled",
        "completed": false,
        "reason": CANCELLATION_TERMINAL_REASON,
        "phases": ordered,
        "provider_completed": false,
        "tests_verified": false,
    }));
    let duration_s = elapsed_rfc3339_seconds(&started_at, &terminal_record.timestamp)?;
    let metric = TerminalMetricIntent {
        mission: manifest.mission_id.clone(),
        domain: "local-pilot".to_owned(),
        task: manifest.mission_text.clone(),
        started_at,
        finished_at: terminal_record.timestamp.clone(),
        duration_s,
        status: "cancelled".to_owned(),
        decomp_source: "authored-phase-lines".to_owned(),
    };

    let mut retained = lock(store);
    if !retained.cancellation_requested {
        return Err(PilotError::Composition(
            "cancelled terminal lost its durable cancellation request".to_owned(),
        ));
    }
    if retained.mission_terminal {
        return Err(PilotError::Composition(
            "durable mission terminal was already committed".to_owned(),
        ));
    }
    if retained.next_sequence != next_sequence {
        return Err(PilotError::Composition(
            "durable journal advanced while preparing cancelled terminal".to_owned(),
        ));
    }
    let terminal = retained.publish_terminal(mission_id, &metric, terminal_record)?;
    let root = retained.root.clone();
    drop(retained);
    if terminal_state.status() != MissionStatus::Cancelled {
        return Err(PilotError::Composition(
            "core reducer disagreed with durable cancelled terminal".to_owned(),
        ));
    }
    Ok(PilotSummary {
        completed: false,
        status: "cancelled",
        reason: CANCELLATION_TERMINAL_REASON.to_owned(),
        result_path: record_path(&root, terminal.sequence),
    })
}

fn recover_interrupted_process(
    options: &PilotOptions,
    mission: &AuthoredMission,
    index: usize,
    snapshot: &snapshot::RepositorySnapshot,
    owner: &DurableProcessOwner,
    cancellation: &CancellationToken,
    supervisor: &ProcessSupervisor,
) -> Result<RecoveredPhaseProcess, PilotError> {
    let phase = &mission.phases[index];
    let role = mission.roles[index];
    if role == PhaseRole::Verification {
        let executable = options
            .verification_argv
            .first()
            .ok_or_else(|| PilotError::Composition("verification argv is empty".to_owned()))?;
        let service = owner
            .service(
                phase.id.as_str(),
                Path::new(executable),
                PathBuf::from("workspace"),
                None,
                cancellation.clone(),
            )
            .map_err(PilotError::Composition)?;
        let report_path =
            authored_cycle::phase_directory(&options.output_dir, index, phase).join("report.json");
        let request = cycle::durable_verification_request(
            options,
            &snapshot.workspace,
            &report_path,
            &service,
        )
        .map_err(PilotError::Composition)?;
        return service.recover(&request).map_err(PilotError::Composition);
    }

    let mut phase_options = options.clone();
    phase_options.command = if role == PhaseRole::Code {
        PilotCommand::Code
    } else {
        PilotCommand::Review
    };
    phase_options.persona = Some(phase.persona.clone());
    let route = routing::select(&phase_options, &phase.objective);
    let prompt = if role == PhaseRole::Code {
        phase.objective.clone()
    } else {
        let diff = snapshot.diff(supervisor).map_err(PilotError::Composition)?;
        let applicable = authored_cycle::applicable_review_task(mission, index);
        let task = format!(
            "Review objective:\n{}\n\nApplicable authored coding tasks:\n{}",
            phase.objective, applicable
        );
        cycle::build_review_prompt(snapshot, &task, &diff).map_err(PilotError::Composition)?
    };
    let shell_config = (role == PhaseRole::Code).then(|| options.output_dir.join("shell-config"));
    let service = owner
        .service(
            phase.id.as_str(),
            Path::new(&options.codex_executable),
            PathBuf::from("workspace"),
            shell_config,
            cancellation.clone(),
        )
        .map_err(PilotError::Composition)?;
    let executor = codex::CodexExecutor::new_with_process_binding(
        if role == PhaseRole::Code {
            codex::Mode::Code
        } else {
            codex::Mode::Review
        },
        service.executable_id(),
        service.process_environment(),
    )
    .map_err(|error| PilotError::Composition(error.to_string()))?;
    let execution_request = phase_execution_request(
        &phase_options,
        &prompt,
        &route,
        &snapshot.workspace,
        phase.id.as_str(),
        if role == PhaseRole::Code {
            "implementer"
        } else {
            "reviewer"
        },
    )?;
    let request = executor
        .process_request(&execution_request)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    service.recover(&request).map_err(PilotError::Composition)
}

fn recovery_result(
    phase: &orchestrator_core::AuthoredPhase,
    role: PhaseRole,
    recovery: &RecoveredPhaseProcess,
) -> Value {
    let (reason, dispatched) = match recovery.classification {
        RustPilotProcessRecovery::RecoveredBeforeStartNotStarted => (
            "interrupted durable phase was confirmed not started; refusing automatic dispatch",
            false,
        ),
        RustPilotProcessRecovery::ReleasedLostOutcomeUncertain
        | RustPilotProcessRecovery::PreviouslyResolved => (
            "released child outcome was lost or uncertain; refusing duplicate dispatch",
            true,
        ),
    };
    let group_absence = recovery.evidence.group_absence().map(|absence| {
        json!({
            "pid": absence.pid(),
            "process_group_id": absence.process_group_id(),
            "process_start_identity": absence.process_start_identity(),
        })
    });
    json!({
        "phase": phase.name,
        "id": phase.id.as_str(),
        "role": role.as_str(),
        "persona": phase.persona,
        "objective": phase.objective,
        "status": "failed",
        "reason": reason,
        "skipped_dependencies": [],
        "provider_dispatched": dispatched && role != PhaseRole::Verification,
        "process": {
            "recovery": format!("{:?}", recovery.classification),
            "automatic_redispatch": false,
            "cleanup_complete": group_absence.is_some() || !dispatched,
            "group_absence": group_absence,
            "post_release_identity": if recovery.evidence.target_released() {
                recovery.evidence.group_absence().map(|absence| json!({
                    "pid": absence.pid(),
                    "process_group_id": absence.process_group_id(),
                    "process_start_identity": absence.process_start_identity(),
                }))
            } else {
                None
            },
        },
    })
}

fn confirmed_not_started_reason(result: &Value) -> Option<&str> {
    let process = result.get("process")?;
    if process.get("cleanup_complete").and_then(Value::as_bool) != Some(true)
        || !process.get("post_release_identity")?.is_null()
    {
        return None;
    }
    process.get("not_started_reason")?.as_str()
}

fn phase_metric_intent(
    manifest: &DurableManifest,
    phase: &orchestrator_core::AuthoredPhase,
    role: PhaseRole,
    result: &Value,
) -> Result<PhaseMetricIntent, PilotError> {
    let process = result
        .get("process")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PilotError::Composition(format!(
                "phase {:?} has no durable process observation for metrics",
                phase.name
            ))
        })?;
    if process.get("cleanup_complete").and_then(Value::as_bool) != Some(true) {
        return Err(PilotError::Composition(format!(
            "phase {:?} has no completed owned-process cleanup for metrics",
            phase.name
        )));
    }
    let absence = process
        .get("group_absence")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PilotError::Composition(format!(
                "phase {:?} has no exact owned group-absence witness for metrics",
                phase.name
            ))
        })?;
    let pid = checked_u32(absence.get("pid"), "metric process PID")?;
    let process_group_id = checked_u32(absence.get("process_group_id"), "metric process-group ID")?;
    let start_identity = absence
        .get("process_start_identity")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PilotError::Composition("metric process-start identity is missing".to_owned())
        })?;
    let reaping = match inspect_recorded_process_identity(pid, process_group_id, start_identity)
        .map_err(|error| PilotError::Composition(format!("metric cleanup proof failed: {error}")))?
    {
        RecordedProcessIdentityStatus::ExactGroupAbsent(reaping) => reaping,
        RecordedProcessIdentityStatus::ExactLive
        | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
            return Err(PilotError::Composition(format!(
                "phase {:?} still has a live owned process group; refusing its metric",
                phase.name
            )));
        }
    };
    let mission_id = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let route = manifest
        .routes
        .iter()
        .find(|route| route.phase_id == phase.id.as_str())
        .ok_or_else(|| PilotError::Composition("metric route binding is missing".to_owned()))?;
    let passed = result.get("status").and_then(Value::as_str) == Some("passed");
    let mut intent = PhaseMetricIntent::new(mission_id, phase.name.clone(), 1, reaping);
    intent.persona = phase.persona.clone();
    intent.selection_method = "durable-authored-route".to_owned();
    intent.status = if passed {
        MetricPhaseStatus::Completed
    } else {
        MetricPhaseStatus::Failed
    };
    let elapsed_ms = result
        .get("elapsed_ms")
        .or_else(|| process.get("elapsed_ms"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    intent.duration_s = i64::try_from(elapsed_ms / 1_000)
        .map_err(|_| PilotError::Composition("metric duration exceeds i64".to_owned()))?;
    intent.gate_passed = if role == PhaseRole::Verification {
        passed
            && result.get("classification").and_then(Value::as_str) == Some("Classified(Pass)")
            && result
                .get("report")
                .and_then(|report| report.get("executed"))
                .and_then(Value::as_u64)
                .is_some_and(|executed| executed > 0)
    } else {
        passed
    };
    intent.provider = route.runtime.clone();
    intent.model = route.model.clone().unwrap_or_default();
    intent.effort = route.effort.clone().unwrap_or_default();
    if let Some(usage) = result.get("usage").and_then(Value::as_object) {
        intent.tokens = TokenCounts {
            input: checked_i64(usage.get("input_tokens"), "input tokens")?,
            output: checked_i64(usage.get("output_tokens"), "output tokens")?,
            cache_creation: checked_i64(
                usage.get("cache_write_input_tokens"),
                "cache-creation tokens",
            )?,
            cache_read: checked_i64(usage.get("cached_input_tokens"), "cache-read tokens")?,
        };
        intent.tokens_known = true;
    } else {
        intent.tokens_known = false;
    }
    intent.cost_known = false;
    intent.target_released = process
        .get("post_release_identity")
        .is_some_and(|identity| !identity.is_null());
    intent.worker_name = phase.id.as_str().to_owned();
    intent.error_type = if passed {
        String::new()
    } else {
        result
            .get("classification")
            .or_else(|| result.get("termination"))
            .and_then(Value::as_str)
            .unwrap_or("phase_failed")
            .to_owned()
    };
    intent.error_message = if passed {
        String::new()
    } else {
        result
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("phase failed")
            .to_owned()
    };
    Ok(intent)
}

fn checked_u32(value: Option<&Value>, field: &str) -> Result<u32, PilotError> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| PilotError::Composition(format!("{field} is invalid")))
}

fn checked_i64(value: Option<&Value>, field: &str) -> Result<i64, PilotError> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| PilotError::Composition(format!("{field} is unavailable or too large")))
}

fn elapsed_rfc3339_seconds(started: &str, finished: &str) -> Result<i64, PilotError> {
    let started = rfc3339_epoch_seconds(started)?;
    let finished = rfc3339_epoch_seconds(finished)?;
    finished
        .checked_sub(started)
        .filter(|duration| *duration >= 0)
        .ok_or_else(|| PilotError::Composition("mission duration is negative".to_owned()))
}

fn rfc3339_epoch_seconds(value: &str) -> Result<i64, PilotError> {
    let valid_suffix = value.get(19..).is_some_and(|suffix| {
        suffix == "Z"
            || suffix
                .strip_prefix('.')
                .and_then(|s| s.strip_suffix('Z'))
                .is_some_and(|fraction| {
                    !fraction.is_empty()
                        && fraction.len() <= 9
                        && fraction.bytes().all(|byte| byte.is_ascii_digit())
                })
    });
    if !value.is_ascii()
        || !valid_suffix
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value.as_bytes().get(10) != Some(&b'T')
        || value.as_bytes().get(13) != Some(&b':')
        || value.as_bytes().get(16) != Some(&b':')
    {
        return Err(PilotError::Composition(
            "mission timestamp is not canonical RFC3339-Z".to_owned(),
        ));
    }
    let number = |range: std::ops::Range<usize>| -> Result<i64, PilotError> {
        value[range]
            .parse::<i64>()
            .map_err(|_| PilotError::Composition("mission timestamp is malformed".to_owned()))
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(PilotError::Composition(
            "mission timestamp is out of range".to_owned(),
        ));
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let epoch = days
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(hour * 3_600 + minute * 60 + second))
        .ok_or_else(|| PilotError::Composition("mission timestamp exceeds i64".to_owned()))?;
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > days_in_month {
        return Err(PilotError::Composition(
            "mission timestamp names an invalid calendar day".to_owned(),
        ));
    }
    Ok(epoch)
}

fn build_manifest(
    options: &PilotOptions,
    source: &str,
    mission: &AuthoredMission,
    root: &Path,
    snapshot: &snapshot::RepositorySnapshot,
    version: &str,
) -> Result<DurableManifest, PilotError> {
    let mission_id = random_mission_id()?;
    let (baseline_identity, workspace_identity) = snapshot.directory_identities();
    let (baseline_digest, workspace_digest) =
        snapshot.state_digests().map_err(PilotError::Composition)?;
    Ok(DurableManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        mission_id,
        mission_text: source.to_owned(),
        mission_digest: sha256(source.as_bytes()),
        source: SourceBinding {
            canonical_path: utf8(&snapshot.source, "source repository")?,
            head: snapshot.head.clone(),
            directory: binding(real_directory_identity(&snapshot.source)?),
        },
        plan: plan_binding(mission),
        routes: route_bindings(options, mission),
        provider: ProviderBinding {
            executable: options
                .codex_executable
                .to_str()
                .ok_or_else(|| {
                    PilotError::Composition("provider executable is not UTF-8".to_owned())
                })?
                .to_owned(),
            executable_id: pinned_executable_id(Path::new(&options.codex_executable))?,
            version: version.to_owned(),
            model_override: options.model.clone(),
            timeout_secs: options.timeout.as_secs(),
        },
        verifier: VerifierBinding {
            argv: options
                .verification_argv
                .iter()
                .map(|value| {
                    value.to_str().map(str::to_owned).ok_or_else(|| {
                        PilotError::Composition("verifier argv is not UTF-8".to_owned())
                    })
                })
                .collect::<Result<_, _>>()?,
            executable_id: pinned_executable_id(Path::new(
                options.verification_argv.first().ok_or_else(|| {
                    PilotError::Composition("verification argv is empty".to_owned())
                })?,
            ))?,
            timeout_secs: options.verification_timeout.as_secs(),
        },
        output_root: utf8(root, "output root")?,
        output_directory: binding(directory_identity(root)?),
        snapshot: SnapshotBinding {
            baseline: utf8(&snapshot.baseline, "snapshot baseline")?,
            workspace: utf8(&snapshot.workspace, "snapshot workspace")?,
            baseline_directory: DirectoryBinding {
                device: baseline_identity.0,
                inode: baseline_identity.1,
            },
            workspace_directory: DirectoryBinding {
                device: workspace_identity.0,
                inode: workspace_identity.1,
            },
            baseline_digest,
            workspace_digest,
        },
        execution_environment: Some(capture_execution_environment()?),
    })
}

fn plan_binding(mission: &AuthoredMission) -> PlanBinding {
    PlanBinding {
        execution_order: mission
            .execution_order
            .iter()
            .map(|index| mission.phases[*index].id.as_str().to_owned())
            .collect(),
        phases: mission
            .phases
            .iter()
            .zip(&mission.roles)
            .map(|(phase, role)| PhaseBinding {
                id: phase.id.as_str().to_owned(),
                name: phase.name.clone(),
                objective: phase.objective.clone(),
                persona: phase.persona.clone(),
                role: role.as_str().to_owned(),
                dependencies: phase
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.as_str().to_owned())
                    .collect(),
            })
            .collect(),
    }
}

fn route_bindings(options: &PilotOptions, mission: &AuthoredMission) -> Vec<RouteBinding> {
    mission
        .phases
        .iter()
        .zip(&mission.roles)
        .map(|(phase, role)| {
            if *role == PhaseRole::Verification {
                RouteBinding {
                    phase_id: phase.id.as_str().to_owned(),
                    runtime: "trusted-local-exec".to_owned(),
                    tier: None,
                    persona: None,
                    model: None,
                    effort: None,
                    selection_reason: None,
                    argv: options
                        .verification_argv
                        .iter()
                        .map(|value| value.to_string_lossy().into_owned())
                        .collect(),
                }
            } else {
                let mut phase_options = options.clone();
                phase_options.persona = Some(phase.persona.clone());
                let route = routing::select(&phase_options, &phase.objective);
                RouteBinding {
                    phase_id: phase.id.as_str().to_owned(),
                    runtime: options.runtime.clone(),
                    tier: Some(route.tier.as_str().to_owned()),
                    persona: Some(route.persona),
                    model: Some(route.model),
                    effort: Some(route.effort.as_str().to_owned()),
                    selection_reason: Some(route.selection_reason.to_owned()),
                    argv: Vec::new(),
                }
            }
        })
        .collect()
}

fn options_from_manifest(
    manifest: &DurableManifest,
    root: &Path,
) -> Result<PilotOptions, PilotError> {
    Ok(PilotOptions {
        command: PilotCommand::Run,
        prompt_file: PathBuf::new(),
        output_dir: root.to_path_buf(),
        repo: Some(PathBuf::from(&manifest.source.canonical_path)),
        model: manifest.provider.model_override.clone(),
        persona: None,
        timeout: Duration::from_secs(manifest.provider.timeout_secs),
        claude_executable: OsString::from("claude"),
        runtime: "codex".to_owned(),
        codex_executable: OsString::from(&manifest.provider.executable),
        verification_argv: manifest.verifier.argv.iter().map(OsString::from).collect(),
        verification_timeout: Duration::from_secs(manifest.verifier.timeout_secs),
        authored_mission: true,
        durable: true,
        stop_after_phase: None,
        mission_id: None,
        progress_log: None,
        observe_follow: false,
        observe_format: super::observe::OutputFormat::Text,
    })
}

fn validate_manifest_contract(manifest: &DurableManifest, root: &Path) -> Result<(), PilotError> {
    if manifest.schema != MANIFEST_SCHEMA
        || sha256(manifest.mission_text.as_bytes()) != manifest.mission_digest
        || Path::new(&manifest.output_root) != root
        || manifest.mission_id.len() != 27
        || !manifest.mission_id.starts_with("rust-durable-")
        || !manifest.mission_id["rust-durable-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || manifest.provider.timeout_secs == 0
        || manifest.provider.timeout_secs > MAX_TIMEOUT.as_secs()
        || manifest.verifier.timeout_secs == 0
        || manifest.verifier.timeout_secs > MAX_TIMEOUT.as_secs()
        || manifest.verifier.argv.is_empty()
        || manifest.provider.executable.is_empty()
        || manifest.provider.version.is_empty()
        || !valid_optional_executable_id(&manifest.provider.executable_id)
        || !valid_optional_executable_id(&manifest.verifier.executable_id)
        || !manifest
            .execution_environment
            .as_deref()
            .is_none_or(valid_execution_environment)
        || !valid_sha256(&manifest.snapshot.baseline_digest)
        || !valid_sha256(&manifest.snapshot.workspace_digest)
        || Path::new(&manifest.snapshot.baseline) != root.join("source-head")
        || Path::new(&manifest.snapshot.workspace) != root.join("workspace")
    {
        return Err(PilotError::Composition(
            "durable manifest is corrupt or mismatched".to_owned(),
        ));
    }
    Ok(())
}

fn valid_optional_executable_id(value: &str) -> bool {
    value.is_empty()
        || value
            .strip_prefix("rust-pilot-exec-v2-")
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            })
}

fn valid_execution_environment(environment: &[EnvironmentBinding]) -> bool {
    let allowed = INHERITED_ENVIRONMENT.into_iter().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    environment.len() <= INHERITED_ENVIRONMENT.len()
        && environment.iter().all(|binding| {
            allowed.contains(binding.name.as_str())
                && seen.insert(binding.name.as_str())
                && !binding.value.as_bytes().contains(&0)
        })
}

fn validate_manifest_for_resume(manifest: &DurableManifest, root: &Path) -> Result<(), PilotError> {
    validate_manifest_contract(manifest, root)?;
    if binding(directory_identity(root)?) != manifest.output_directory {
        return Err(PilotError::Composition(
            "durable manifest is corrupt or mismatched".to_owned(),
        ));
    }
    validate_manifest_source(manifest)
}

fn validate_manifest_source(manifest: &DurableManifest) -> Result<(), PilotError> {
    match real_directory_identity(Path::new(&manifest.source.canonical_path)) {
        Ok(source_identity) if binding(source_identity) == manifest.source.directory => {}
        Ok(_) => {
            return Err(PilotError::Composition(
                "durable source binding is mismatched".to_owned(),
            ));
        }
        Err(PilotError::Artifact { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn validate_manifest_derived(
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    options: &PilotOptions,
) -> Result<(), PilotError> {
    if manifest.plan != plan_binding(mission) || manifest.routes != route_bindings(options, mission)
    {
        return Err(PilotError::Composition(
            "saved compiled plan or selected routes do not match the immutable mission inputs"
                .to_owned(),
        ));
    }
    Ok(())
}

fn replay(
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    root: &Path,
    manifest_digest: &str,
) -> Result<Replayed, PilotError> {
    let records = read_records(root, manifest, manifest_digest)?;
    replay_records(manifest, mission, records, manifest_digest)
}

fn replay_inspected(
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    root: &InspectionRoot,
    manifest_digest: &str,
    observer: &mut impl StatusReadObserver,
) -> Result<Replayed, PilotError> {
    let records = read_stable_inspected_records(root, manifest, manifest_digest, observer)?;
    replay_records(manifest, mission, records, manifest_digest)
}

fn replay_records(
    manifest: &DurableManifest,
    mission: &AuthoredMission,
    records: Vec<(JournalEnvelope, PathBuf)>,
    manifest_digest: &str,
) -> Result<Replayed, PilotError> {
    let next_sequence = i64::try_from(records.len())
        .map_err(|_| PilotError::Composition("durable journal is too large".to_owned()))?
        + 1;
    let mission_id = MissionId::new(&manifest.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let mut state = initial_state(&mission_id, mission)?;
    let mut results = BTreeMap::new();
    let mut terminal = None;
    let mut cancellation_requested = false;
    let mut expected_digests = None;
    let mut previous_digest = manifest_digest.to_owned();
    let last = records
        .last()
        .ok_or_else(|| PilotError::Composition("durable journal count is invalid".to_owned()))?;
    let last_timestamp = last.0.record.timestamp.clone();
    let last_kind = last.0.record.kind;
    let first_timestamp = records
        .first()
        .map(|record| record.0.record.timestamp.clone())
        .ok_or_else(|| PilotError::Composition("durable journal is empty".to_owned()))?;
    for (envelope, path) in records {
        if terminal.is_some() {
            return Err(PilotError::Composition(
                "durable journal contains records after mission terminal".to_owned(),
            ));
        }
        validate_record_binding(&envelope.record, mission)?;
        let had_running_phase = state
            .phases()
            .any(|phase| phase.status == PhaseStatus::Running);
        state = apply(&state, &envelope.record)?;
        match envelope.record.kind {
            RecordKind::PhaseTerminal | RecordKind::PhaseSkippedObservation => {
                let phase_id = envelope.record.phase_id.as_deref().ok_or_else(|| {
                    PilotError::Composition("journal phase binding is missing".to_owned())
                })?;
                let index = mission
                    .phases
                    .iter()
                    .position(|phase| phase.id.as_str() == phase_id)
                    .ok_or_else(|| {
                        PilotError::Composition("journal names an unknown phase".to_owned())
                    })?;
                let result = envelope.record.result.clone().ok_or_else(|| {
                    PilotError::Composition("journal terminal result is missing".to_owned())
                })?;
                if results.insert(index, result).is_some() {
                    return Err(PilotError::Composition(
                        "journal repeats a terminal phase".to_owned(),
                    ));
                }
                let expected = if envelope.record.kind == RecordKind::PhaseSkippedObservation {
                    PhaseStatus::Skipped
                } else if envelope.record.status.as_deref() == Some("passed") {
                    PhaseStatus::Completed
                } else {
                    PhaseStatus::Failed
                };
                if state
                    .phase(&mission.phases[index].id)
                    .map(|phase| phase.status)
                    != Some(expected)
                {
                    return Err(PilotError::Composition(
                        "journal phase observation disagrees with the core reducer".to_owned(),
                    ));
                }
            }
            RecordKind::MissionTerminal => {
                if terminal.is_some() {
                    return Err(PilotError::Composition(
                        "journal repeats mission terminal".to_owned(),
                    ));
                }
                let (completed, status, reason) = validate_mission_terminal(
                    &envelope.record,
                    mission,
                    &state,
                    &results,
                    cancellation_requested,
                    had_running_phase,
                )?;
                terminal = Some((completed, status, reason, path));
            }
            RecordKind::Paused => {
                let phase_id = envelope.record.phase_id.as_deref().ok_or_else(|| {
                    PilotError::Composition("journal pause phase is missing".to_owned())
                })?;
                let phase = mission
                    .phases
                    .iter()
                    .find(|phase| phase.id.as_str() == phase_id)
                    .ok_or_else(|| {
                        PilotError::Composition("journal pause phase is unknown".to_owned())
                    })?;
                if state.phase(&phase.id).map(|phase| phase.status) != Some(PhaseStatus::Completed)
                {
                    return Err(PilotError::Composition(
                        "journal pause is not after a passed durable phase".to_owned(),
                    ));
                }
            }
            RecordKind::CancellationRequested => {
                if cancellation_requested {
                    return Err(PilotError::Composition(
                        "journal repeats the cancellation request".to_owned(),
                    ));
                }
                cancellation_requested = true;
            }
            RecordKind::MissionStarted | RecordKind::PhaseStarted => {}
        }
        if let (Some(baseline), Some(workspace)) = (
            envelope.record.baseline_digest.clone(),
            envelope.record.workspace_digest.clone(),
        ) {
            expected_digests = Some((baseline, workspace));
        }
        previous_digest = envelope.digest;
    }
    Ok(Replayed {
        state,
        results,
        previous_digest,
        next_sequence,
        terminal,
        expected_digests,
        last_sequence: next_sequence - 1,
        last_timestamp,
        last_kind,
        first_timestamp,
        cancellation_requested,
    })
}

fn validate_mission_terminal(
    record: &JournalRecord,
    mission: &AuthoredMission,
    terminal_state: &MissionState,
    results: &BTreeMap<usize, Value>,
    cancellation_requested: bool,
    had_running_phase: bool,
) -> Result<(bool, &'static str, String), PilotError> {
    let status = record
        .status
        .as_deref()
        .ok_or_else(|| PilotError::Composition("journal terminal status is missing".to_owned()))?;
    let (completed, expected_state) = match status {
        "completed" => (true, MissionStatus::Completed),
        "failed" => (false, MissionStatus::Failed),
        "cancelled" => (false, MissionStatus::Cancelled),
        _ => {
            return Err(PilotError::Composition(
                "journal terminal status is invalid".to_owned(),
            ));
        }
    };
    if status == "cancelled" && !cancellation_requested {
        return Err(PilotError::Composition(
            "cancelled mission terminal has no preceding durable cancellation request".to_owned(),
        ));
    }
    if status == "cancelled" && had_running_phase {
        return Err(PilotError::Composition(
            "cancelled mission terminal precedes attempted-phase recovery".to_owned(),
        ));
    }
    if status != "cancelled" && cancellation_requested {
        return Err(PilotError::Composition(
            "non-cancelled mission terminal follows a durable cancellation request".to_owned(),
        ));
    }
    if terminal_state.status() != expected_state {
        return Err(PilotError::Composition(
            "journal mission terminal disagrees with the core reducer".to_owned(),
        ));
    }
    let ordered = if status == "cancelled" {
        mission
            .execution_order
            .iter()
            .map(|index| {
                results.get(index).cloned().unwrap_or_else(|| {
                    skipped_record(
                        &mission.phases[*index],
                        mission.roles[*index],
                        mission,
                        terminal_state,
                    )
                })
            })
            .collect::<Vec<_>>()
    } else {
        mission
            .execution_order
            .iter()
            .map(|index| results.get(index).cloned())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                PilotError::Composition(
                    "journal mission terminal is missing a phase result".to_owned(),
                )
            })?
    };
    let provider_completed = status != "cancelled"
        && ordered
            .iter()
            .filter(|phase| phase["role"] != "verification")
            .all(|phase| phase["status"] == "passed");
    let tests_verified = status != "cancelled"
        && ordered
            .iter()
            .filter(|phase| phase["role"] == "verification")
            .all(|phase| phase["status"] == "passed");
    let reason = record.reason.clone().unwrap_or_default();
    let expected_result = json!({
        "schema": RESULT_SCHEMA,
        "command": "resume-capable-run",
        "input": "authored-mission",
        "status": status,
        "completed": completed,
        "reason": reason,
        "phases": ordered,
        "provider_completed": provider_completed,
        "tests_verified": tests_verified,
    });
    if record.result.as_ref() != Some(&expected_result) {
        return Err(PilotError::Composition(
            "journal mission result does not match its terminal record".to_owned(),
        ));
    }
    Ok((
        completed,
        match status {
            "completed" => "completed",
            "failed" => "failed",
            "cancelled" => "cancelled",
            _ => unreachable!(),
        },
        reason,
    ))
}

fn validate_record_binding(
    record: &JournalRecord,
    mission: &AuthoredMission,
) -> Result<(), PilotError> {
    let Some(phase_id) = record.phase_id.as_deref() else {
        if record.role.is_some() {
            return Err(PilotError::Composition(
                "mission journal record has a phase role".to_owned(),
            ));
        }
        return Ok(());
    };
    let index = mission
        .phases
        .iter()
        .position(|phase| phase.id.as_str() == phase_id)
        .ok_or_else(|| PilotError::Composition("journal names an unknown phase".to_owned()))?;
    if record.role.as_deref() != Some(mission.roles[index].as_str()) {
        return Err(PilotError::Composition(
            "journal phase role does not match the plan".to_owned(),
        ));
    }
    if let Some(result) = &record.result {
        if result["id"] != phase_id
            || result["phase"] != mission.phases[index].name
            || result["role"] != mission.roles[index].as_str()
            || result["persona"] != mission.phases[index].persona
            || result["objective"] != mission.phases[index].objective
            || result["status"].as_str() != record.status.as_deref()
            || result["reason"].as_str() != record.reason.as_deref()
        {
            return Err(PilotError::Composition(
                "journal phase result does not match its sequence and plan binding".to_owned(),
            ));
        }
    }
    Ok(())
}

struct InspectedRecordSet {
    records: Vec<(JournalEnvelope, PathBuf)>,
    names: Vec<String>,
    fingerprints: Vec<String>,
}

enum InspectedReadFailure {
    Retryable(PilotError),
    Fatal(PilotError),
}

fn read_stable_inspected_records(
    root: &InspectionRoot,
    manifest: &DurableManifest,
    manifest_digest: &str,
    observer: &mut impl StatusReadObserver,
) -> Result<Vec<(JournalEnvelope, PathBuf)>, PilotError> {
    // The writer reserves the final record name with create_new before filling
    // and syncing it. Repeated equal reads provide a validated snapshot of
    // visible records; they do not attest that an active writer completed fsync.
    let mut last_retry = None;
    let mut observation = 0;
    for attempt in 0..STATUS_READ_ATTEMPTS {
        let first_observation = observation;
        observation += 1;
        let first = match read_inspected_records_once(
            root,
            manifest,
            manifest_digest,
            observer,
            first_observation,
        ) {
            Ok(records) => records,
            Err(InspectedReadFailure::Fatal(error)) => return Err(error),
            Err(InspectedReadFailure::Retryable(error)) => {
                last_retry = Some(error);
                if attempt + 1 < STATUS_READ_ATTEMPTS {
                    thread::sleep(STATUS_RETRY_DELAY);
                    continue;
                }
                break;
            }
        };
        thread::sleep(STATUS_RETRY_DELAY);
        let second_observation = observation;
        observation += 1;
        let second = match read_inspected_records_once(
            root,
            manifest,
            manifest_digest,
            observer,
            second_observation,
        ) {
            Ok(records) => records,
            Err(InspectedReadFailure::Fatal(error)) => return Err(error),
            Err(InspectedReadFailure::Retryable(error)) => {
                last_retry = Some(error);
                continue;
            }
        };
        if first.names == second.names && first.fingerprints == second.fingerprints {
            root.verify()?;
            return Ok(first.records);
        }
        last_retry = Some(PilotError::Composition(
            "durable journal changed during status inspection".to_owned(),
        ));
    }
    let detail = last_retry
        .map(|error| error.to_string())
        .unwrap_or_else(|| "no stable observation".to_owned());
    Err(PilotError::Composition(format!(
        "durable journal is busy or its trailing append did not stabilize: {detail}"
    )))
}

fn read_inspected_records_once(
    root: &InspectionRoot,
    manifest: &DurableManifest,
    manifest_digest: &str,
    observer: &mut impl StatusReadObserver,
    observation: usize,
) -> Result<InspectedRecordSet, InspectedReadFailure> {
    root.verify().map_err(InspectedReadFailure::Fatal)?;
    observer
        .before_enumeration(root, observation)
        .map_err(InspectedReadFailure::Fatal)?;
    let names = inspected_record_names(root).map_err(InspectedReadFailure::Fatal)?;
    observer
        .after_enumeration(root, observation)
        .map_err(InspectedReadFailure::Fatal)?;
    root.verify().map_err(InspectedReadFailure::Fatal)?;
    if names.is_empty() {
        return Err(InspectedReadFailure::Retryable(PilotError::Composition(
            "durable journal count is invalid".to_owned(),
        )));
    }
    if names.len() > MAX_RECORDS {
        return Err(InspectedReadFailure::Fatal(PilotError::Composition(
            "durable journal count is invalid".to_owned(),
        )));
    }
    let mut records = Vec::with_capacity(names.len());
    let mut fingerprints = Vec::with_capacity(names.len());
    let mut previous = manifest_digest.to_owned();
    for (index, name) in names.iter().enumerate() {
        let expected_name = format!("{:06}.json", index + 1);
        if name != &expected_name {
            return Err(InspectedReadFailure::Fatal(PilotError::Composition(
                "durable journal sequence has a gap".to_owned(),
            )));
        }
        observer
            .before_record_read(root, observation, index)
            .map_err(InspectedReadFailure::Fatal)?;
        let display = root.canonical.join("journal").join(name);
        let read = root.read_journal_file(Path::new(name), &display, MAX_RECORD_BYTES);
        observer
            .after_record_read(root, observation, index)
            .map_err(InspectedReadFailure::Fatal)?;
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(error) if index + 1 == names.len() && unstable_trailing_read(&error) => {
                return Err(InspectedReadFailure::Retryable(error));
            }
            Err(error) => return Err(InspectedReadFailure::Fatal(error)),
        };
        let envelope: JournalEnvelope = serde_json::from_slice(&bytes).map_err(|_| {
            let error = PilotError::Composition("durable journal record is malformed".to_owned());
            if index + 1 == names.len() {
                InspectedReadFailure::Retryable(error)
            } else {
                InspectedReadFailure::Fatal(error)
            }
        })?;
        validate_envelope(&envelope, manifest, &previous, index + 1)
            .map_err(InspectedReadFailure::Fatal)?;
        previous = envelope.digest.clone();
        records.push((envelope, display));
        fingerprints.push(sha256(&bytes));
    }
    root.verify().map_err(InspectedReadFailure::Fatal)?;
    Ok(InspectedRecordSet {
        records,
        names,
        fingerprints,
    })
}

fn unstable_trailing_read(error: &PilotError) -> bool {
    match error {
        PilotError::Composition(message) => {
            message.contains("changed while reading") || message.contains("grew while reading")
        }
        PilotError::Artifact { source, .. } => source.kind() == io::ErrorKind::NotFound,
        _ => false,
    }
}

fn inspected_record_names(root: &InspectionRoot) -> Result<Vec<String>, PilotError> {
    let journal = root.canonical.join("journal");
    let mut names = BTreeSet::new();
    // Open a fresh directory stream relative to the retained descriptor. A
    // fresh open-file description gives every observation an independent
    // enumeration offset without consulting the journal pathname.
    let journal_reader = rustix::fs::openat(
        &root.journal,
        Path::new("."),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
    .map_err(|source| PilotError::Artifact {
        path: journal.clone(),
        source,
    })?;
    let entries = rustix::fs::Dir::read_from(&journal_reader)
        .map_err(io::Error::from)
        .map_err(|source| PilotError::Artifact {
            path: journal.clone(),
            source,
        })?;
    for entry in entries {
        let entry = entry
            .map_err(io::Error::from)
            .map_err(|source| PilotError::Artifact {
                path: journal.clone(),
                source,
            })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if names.len() == MAX_RECORDS {
            return Err(PilotError::Composition(
                "durable journal count is invalid".to_owned(),
            ));
        }
        let name = String::from_utf8(bytes.to_vec())
            .map_err(|_| PilotError::Composition("durable journal name is not UTF-8".to_owned()))?;
        if name.len() != 11
            || !name.ends_with(".json")
            || !name[..6].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(PilotError::Composition(
                "durable journal contains an unknown entry".to_owned(),
            ));
        }
        if !names.insert(name) {
            return Err(PilotError::Composition(
                "durable journal contains duplicate names".to_owned(),
            ));
        }
    }
    Ok(names.into_iter().collect())
}

fn read_records(
    root: &Path,
    manifest: &DurableManifest,
    manifest_digest: &str,
) -> Result<Vec<(JournalEnvelope, PathBuf)>, PilotError> {
    let count = read_record_count(root)?;
    if count == 0 || count > MAX_RECORDS {
        return Err(PilotError::Composition(
            "durable journal count is invalid".to_owned(),
        ));
    }
    let mut records = Vec::with_capacity(count);
    let mut previous = manifest_digest.to_owned();
    for sequence in 1..=count {
        let path = root.join("journal").join(format!("{sequence:06}.json"));
        let bytes = read_bounded_regular(&path, MAX_RECORD_BYTES)?;
        let envelope: JournalEnvelope = serde_json::from_slice(&bytes).map_err(|_| {
            PilotError::Composition("durable journal record is malformed".to_owned())
        })?;
        validate_envelope(&envelope, manifest, &previous, sequence)?;
        previous = envelope.digest.clone();
        records.push((envelope, path));
    }
    Ok(records)
}

fn validate_envelope(
    envelope: &JournalEnvelope,
    manifest: &DurableManifest,
    previous: &str,
    sequence: usize,
) -> Result<(), PilotError> {
    let expected_sequence = i64::try_from(sequence).unwrap_or(i64::MAX);
    let record_bytes = serde_json::to_vec(&envelope.record)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let expected_digest = chained_digest(previous, &record_bytes);
    if envelope.previous_digest != previous
        || envelope.digest != expected_digest
        || envelope.record.schema != JOURNAL_SCHEMA
        || envelope.record.mission_id != manifest.mission_id
        || envelope.record.sequence != expected_sequence
        || envelope.record.event_id != format!("durable-event-{expected_sequence:06}")
    {
        return Err(PilotError::Composition(
            "durable journal chain, sequence, or mission binding is corrupt".to_owned(),
        ));
    }
    validate_record_shape(&envelope.record)
}

fn read_record_count(root: &Path) -> Result<usize, PilotError> {
    let journal = root.join("journal");
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&journal).map_err(|error| PilotError::Artifact {
        path: journal.clone(),
        source: error,
    })? {
        let entry = entry.map_err(|error| PilotError::Artifact {
            path: journal.clone(),
            source: error,
        })?;
        if names.len() == MAX_RECORDS {
            return Err(PilotError::Composition(
                "durable journal count is invalid".to_owned(),
            ));
        }
        let kind = entry.file_type().map_err(|error| PilotError::Artifact {
            path: entry.path(),
            source: error,
        })?;
        if !kind.is_file() {
            return Err(PilotError::Composition(
                "durable journal contains a non-file".to_owned(),
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| PilotError::Composition("durable journal name is not UTF-8".to_owned()))?;
        if name.len() != 11
            || !name.ends_with(".json")
            || !name[..6].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(PilotError::Composition(
                "durable journal contains an unknown entry".to_owned(),
            ));
        }
        names.insert(name);
    }
    for (index, name) in names.iter().enumerate() {
        if name != &format!("{:06}.json", index + 1) {
            return Err(PilotError::Composition(
                "durable journal sequence has a gap".to_owned(),
            ));
        }
    }
    Ok(names.len())
}

fn validate_record_shape(record: &JournalRecord) -> Result<(), PilotError> {
    let phase = record.phase_id.is_some();
    let digests = record.baseline_digest.as_deref().is_some_and(valid_sha256)
        && record.workspace_digest.as_deref().is_some_and(valid_sha256);
    let no_digests = record.baseline_digest.is_none() && record.workspace_digest.is_none();
    let valid = match record.kind {
        RecordKind::MissionStarted => {
            !phase
                && record.status.is_none()
                && record.reason.is_none()
                && record.result.is_none()
                && no_digests
        }
        RecordKind::PhaseStarted => {
            phase
                && record.status.as_deref() == Some("started")
                && record.reason.is_none()
                && record.result.is_none()
                && no_digests
        }
        RecordKind::PhaseTerminal => {
            phase
                && matches!(record.status.as_deref(), Some("passed" | "failed"))
                && record.result.is_some()
                && digests
        }
        RecordKind::PhaseSkippedObservation => {
            phase
                && record.status.as_deref() == Some("skipped")
                && record.reason.is_some()
                && record.result.is_some()
                && digests
        }
        RecordKind::Paused => {
            phase
                && record.status.as_deref() == Some("paused")
                && record.reason.is_some()
                && record.result.is_none()
                && digests
        }
        RecordKind::CancellationRequested => {
            !phase
                && record.status.as_deref() == Some("requested")
                && record.reason.as_deref() == Some(CANCELLATION_REQUEST_REASON)
                && record.result.is_none()
                && no_digests
        }
        RecordKind::MissionTerminal => {
            !phase
                && matches!(
                    record.status.as_deref(),
                    Some("completed" | "failed" | "cancelled")
                )
                && record.reason.is_some()
                && record.result.is_some()
                && digests
        }
    };
    if valid {
        Ok(())
    } else {
        Err(PilotError::Composition(
            "durable journal record shape is invalid".to_owned(),
        ))
    }
}

fn apply(state: &MissionState, record: &JournalRecord) -> Result<MissionState, PilotError> {
    let mission_id = MissionId::new(&record.mission_id)
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let phase_id = record
        .phase_id
        .as_ref()
        .map(PhaseId::new)
        .transpose()
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    let transition = match record.kind {
        RecordKind::MissionStarted => ReducerTransition::MissionStarted,
        RecordKind::PhaseStarted => ReducerTransition::PhaseStarted,
        RecordKind::PhaseTerminal if record.status.as_deref() == Some("passed") => {
            ReducerTransition::PhaseCompleted
        }
        RecordKind::PhaseTerminal => ReducerTransition::PhaseFailed {
            error: record
                .reason
                .clone()
                .unwrap_or_else(|| "phase failed".to_owned()),
        },
        RecordKind::MissionTerminal if record.status.as_deref() == Some("completed") => {
            ReducerTransition::MissionCompleted
        }
        RecordKind::MissionTerminal if record.status.as_deref() == Some("cancelled") => {
            ReducerTransition::MissionCancelled {
                reason: record
                    .reason
                    .clone()
                    .unwrap_or_else(|| CANCELLATION_TERMINAL_REASON.to_owned()),
            }
        }
        RecordKind::MissionTerminal => ReducerTransition::MissionFailed,
        RecordKind::PhaseSkippedObservation
        | RecordKind::Paused
        | RecordKind::CancellationRequested => ReducerTransition::Unknown {
            event_type: format!("durable_{:?}", record.kind),
            data: Value::Null,
        },
    };
    let input = ReducerInput {
        event_id: EventId::new(&record.event_id)
            .map_err(|error| PilotError::Composition(error.to_string()))?,
        sequence: record.sequence,
        timestamp: record.timestamp.clone(),
        mission_id,
        phase_id,
        worker_id: None,
        data: Value::Null,
        extra: BTreeMap::new(),
        transition,
    };
    reduce(state, &input)
        .map(|reduction| reduction.state)
        .map_err(|error| {
            PilotError::Composition(format!("durable reducer replay refused: {error}"))
        })
}

fn initial_state(
    mission_id: &MissionId,
    mission: &AuthoredMission,
) -> Result<MissionState, PilotError> {
    let definitions = mission
        .phases
        .iter()
        .map(|phase| PhaseDefinition {
            id: phase.id.clone(),
            dependencies: phase.dependencies.clone(),
        })
        .collect();
    MissionState::new(mission_id.clone(), definitions)
        .map_err(|error| PilotError::Composition(error.to_string()))
}

#[expect(
    clippy::too_many_arguments,
    reason = "journal fields remain explicit at each durability boundary"
)]
fn record(
    mission_id: &str,
    sequence: i64,
    kind: RecordKind,
    phase_id: Option<&str>,
    role: Option<&str>,
    status: Option<&str>,
    reason: Option<&str>,
    result: Option<Value>,
    digests: Option<&(String, String)>,
) -> JournalRecord {
    JournalRecord {
        schema: JOURNAL_SCHEMA.to_owned(),
        mission_id: mission_id.to_owned(),
        sequence,
        event_id: String::new(),
        timestamp: rfc3339_now(),
        kind,
        phase_id: phase_id.map(str::to_owned),
        role: role.map(str::to_owned),
        status: status.map(str::to_owned),
        reason: reason.map(str::to_owned),
        result,
        baseline_digest: digests.map(|value| value.0.clone()),
        workspace_digest: digests.map(|value| value.1.clone()),
    }
}

impl DurableStore {
    fn publish_terminal(
        &mut self,
        mission_id: &MissionId,
        metric: &TerminalMetricIntent,
        terminal: JournalRecord,
    ) -> Result<JournalRecord, PilotError> {
        if self.mission_terminal || self.terminal_admission_closed {
            return Err(PilotError::Composition(
                "terminal publication already began; durable recovery is required".to_owned(),
            ));
        }
        let authoritative = serde_json::to_value(&terminal)
            .map_err(|error| PilotError::Composition(error.to_string()))?;
        // record_terminal can commit its authoritative transition and then fail
        // while draining or projecting it. Close live admission before entering
        // that operation, and retain the gate on every error until this owner
        // exits. Reopen resolves the persisted outcome; this flag claims no commit.
        self.terminal_admission_closed = true;
        self.metrics
            .record_terminal(mission_id, metric, &authoritative, &terminal.timestamp)
            .map_err(|error| PilotError::Composition(format!("terminal metric failed: {error}")))?;
        self.mission_terminal = true;
        self.append(terminal)
    }

    fn fresh(root: PathBuf, authority: ProductionWriterAuthority) -> Result<Self, PilotError> {
        let root_identity = directory_identity(&root)?;
        let metrics = RustPilotMetricsOwner::under_writer(&authority)
            .map_err(|error| PilotError::Composition(format!("metrics owner refused: {error}")))?;
        let store = Self {
            root,
            root_identity,
            authority: Arc::new(authority),
            metrics,
            previous_digest: String::new(),
            next_sequence: 1,
            mission_started_at: None,
            cancellation_requested: false,
            mission_terminal: false,
            terminal_admission_closed: false,
            progress: super::progress::Progress::default(),
        };
        store
            .metrics
            .recover_pending(&rfc3339_now())
            .map_err(|error| {
                PilotError::Composition(format!("metrics recovery failed: {error}"))
            })?;
        Ok(store)
    }

    fn reopen(
        root: PathBuf,
        authority: ProductionWriterAuthority,
        manifest_digest: String,
    ) -> Result<Self, PilotError> {
        let root_identity = directory_identity(&root)?;
        let metrics = RustPilotMetricsOwner::under_writer(&authority)
            .map_err(|error| PilotError::Composition(format!("metrics owner refused: {error}")))?;
        let store = Self {
            root,
            root_identity,
            authority: Arc::new(authority),
            metrics,
            previous_digest: manifest_digest,
            next_sequence: 1,
            mission_started_at: None,
            cancellation_requested: false,
            mission_terminal: false,
            terminal_admission_closed: false,
            progress: super::progress::Progress::default(),
        };
        store
            .metrics
            .recover_pending(&rfc3339_now())
            .map_err(|error| {
                PilotError::Composition(format!("metrics recovery failed: {error}"))
            })?;
        Ok(store)
    }

    fn verify(&self) -> Result<(), PilotError> {
        self.authority.boundary().verify().map_err(|error| {
            PilotError::Composition(format!("retained durable boundary changed: {error}"))
        })?;
        if directory_identity(&self.root)? != self.root_identity {
            return Err(PilotError::Composition(
                "durable output root was replaced".to_owned(),
            ));
        }
        Ok(())
    }

    fn create_dir(&mut self, relative: &Path) -> Result<(), PilotError> {
        self.verify()?;
        validate_relative(relative)?;
        let path = self.root.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| PilotError::Composition("durable directory has no parent".to_owned()))?;
        validate_real_ancestors(&self.root, parent)?;
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(&path)
            .map_err(|source| PilotError::Artifact {
                path: path.clone(),
                source,
            })?;
        sync_directory(parent)?;
        self.verify()
    }

    fn create_file(&self, relative: &Path, bytes: &[u8], maximum: usize) -> Result<(), PilotError> {
        self.verify()?;
        validate_relative(relative)?;
        if bytes.len() > maximum || bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(PilotError::Composition(
                "durable artifact exceeds its bound".to_owned(),
            ));
        }
        let path = self.root.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| PilotError::Composition("durable artifact has no parent".to_owned()))?;
        validate_real_ancestors(&self.root, parent)?;
        write_artifact(&path, bytes)?;
        self.verify()
    }

    fn append(&mut self, mut record: JournalRecord) -> Result<JournalRecord, PilotError> {
        if self.next_sequence <= 0
            || usize::try_from(self.next_sequence).map_or(true, |value| value > MAX_RECORDS)
        {
            return Err(PilotError::Composition(
                "durable journal is full".to_owned(),
            ));
        }
        record.sequence = self.next_sequence;
        record.event_id = format!("durable-event-{:06}", record.sequence);
        let record_bytes = serde_json::to_vec(&record)
            .map_err(|error| PilotError::Composition(error.to_string()))?;
        let digest = chained_digest(&self.previous_digest, &record_bytes);
        let envelope = JournalEnvelope {
            previous_digest: self.previous_digest.clone(),
            digest: digest.clone(),
            record: record.clone(),
        };
        let path = PathBuf::from("journal").join(format!("{:06}.json", record.sequence));
        let bytes = json_bytes(&self.root.join(&path), &envelope)?;
        self.create_file(&path, &bytes, MAX_RECORD_BYTES as usize)?;
        if record.kind == RecordKind::MissionStarted {
            self.mission_started_at = Some(record.timestamp.clone());
        }
        if record.kind == RecordKind::CancellationRequested {
            self.cancellation_requested = true;
        }
        if record.kind == RecordKind::MissionTerminal {
            self.mission_terminal = true;
        }
        self.previous_digest = digest;
        self.next_sequence += 1;
        self.progress.emit(json!({"kind":"journal", "sequence":record.sequence, "event":record.kind, "phase_id":record.phase_id, "role":record.role, "status":record.status}));
        Ok(record)
    }
}

fn acquire(root: &Path, repository: &Path) -> Result<ProductionWriterAuthority, PilotError> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        PilotError::Composition("HOME is required for private writer admission".to_owned())
    })?;
    let guards = IsolatedHomeGuards {
        user_home: home,
        repository_checkout: repository.to_path_buf(),
    };
    RustPilotRuntimeHome::acquire(root, &guards, env!("CARGO_PKG_VERSION")).map_err(|error| {
        PilotError::Composition(format!("private writer admission refused: {error}"))
    })
}

fn capture_execution_environment() -> Result<Vec<EnvironmentBinding>, PilotError> {
    INHERITED_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .map(|(name, value)| {
            let value = value.into_string().map_err(|_| {
                PilotError::Composition(format!("environment variable {name} is not UTF-8"))
            })?;
            Ok(EnvironmentBinding {
                name: name.to_owned(),
                value,
            })
        })
        .collect()
}

fn pinned_executable_id(path: &Path) -> Result<String, PilotError> {
    RustPilotProcessExecutable::open(path)
        .map(|executable| executable.logical_id().to_owned())
        .map_err(|error| PilotError::Composition(format!("executable admission refused: {error}")))
}

fn executable_ids(manifest: &DurableManifest) -> BTreeMap<PathBuf, String> {
    let mut ids = BTreeMap::new();
    if !manifest.provider.executable_id.is_empty() {
        ids.insert(
            PathBuf::from(&manifest.provider.executable),
            manifest.provider.executable_id.clone(),
        );
    }
    if !manifest.verifier.executable_id.is_empty() {
        if let Some(executable) = manifest.verifier.argv.first() {
            ids.insert(
                PathBuf::from(executable),
                manifest.verifier.executable_id.clone(),
            );
        }
    }
    ids
}

fn resolve_executable(requested: &std::ffi::OsStr) -> Result<OsString, PilotError> {
    let requested_path = Path::new(requested);
    let candidates = if requested_path.components().count() > 1 {
        vec![requested_path.to_path_buf()]
    } else {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join(requested_path))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    for candidate in candidates {
        let Ok(canonical) = fs::canonicalize(candidate) else {
            continue;
        };
        let Ok(metadata) = canonical.symlink_metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok(canonical.into_os_string());
        }
    }
    Err(PilotError::Composition(format!(
        "provider executable {:?} is not a real executable file",
        requested
    )))
}

fn create_phase_directories(
    store: &mut DurableStore,
    mission: &AuthoredMission,
) -> Result<(), PilotError> {
    store.create_dir(Path::new("phases"))?;
    for (index, phase) in mission.phases.iter().enumerate() {
        let path = authored_cycle::phase_directory(&store.root, index, phase);
        let relative = path.strip_prefix(&store.root).map_err(|_| {
            PilotError::Composition("phase directory escaped durable root".to_owned())
        })?;
        store.create_dir(relative)?;
    }
    Ok(())
}

fn validate_stop_phase(stop: Option<&str>, mission: &AuthoredMission) -> Result<(), PilotError> {
    if let Some(stop) = stop {
        if !mission.phases.iter().any(|phase| phase.name == stop) {
            return Err(PilotError::Usage(format!(
                "--stop-after-phase names unknown phase {stop:?}"
            )));
        }
    }
    Ok(())
}

fn skipped_record(
    phase: &orchestrator_core::AuthoredPhase,
    role: PhaseRole,
    mission: &AuthoredMission,
    state: &MissionState,
) -> Value {
    let reason = state
        .phase(&phase.id)
        .and_then(|value| value.skip_reason.as_deref())
        .unwrap_or("dependency did not pass");
    json!({
        "phase": phase.name,
        "id": phase.id.as_str(),
        "role": role.as_str(),
        "persona": phase.persona,
        "objective": phase.objective,
        "status": "skipped",
        "reason": reason,
        "skipped_dependencies": authored_cycle::dependency_names(phase, &mission.phases).into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "provider_dispatched": false,
    })
}

fn ordered_results(
    mission: &AuthoredMission,
    state: &MissionState,
    results: &BTreeMap<usize, Value>,
) -> Vec<Value> {
    mission
        .execution_order
        .iter()
        .map(|index| {
            results.get(index).cloned().unwrap_or_else(|| {
                skipped_record(
                    &mission.phases[*index],
                    mission.roles[*index],
                    mission,
                    state,
                )
            })
        })
        .collect()
}

fn validate_relative(path: &Path) -> Result<(), PilotError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PilotError::Composition(
            "durable path is not confined".to_owned(),
        ));
    }
    Ok(())
}

fn validate_real_ancestors(root: &Path, path: &Path) -> Result<(), PilotError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PilotError::Composition("durable parent escaped its root".to_owned()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(PilotError::Composition(
                "durable parent is not confined".to_owned(),
            ));
        };
        current.push(name);
        let metadata = current
            .symlink_metadata()
            .map_err(|source| PilotError::Artifact {
                path: current.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PilotError::Composition(format!(
                "durable parent {} is not a real directory",
                current.display()
            )));
        }
    }
    Ok(())
}

fn directory_identity(path: &Path) -> Result<(u64, u64), PilotError> {
    let identity = real_directory_identity(path)?;
    let metadata = path
        .symlink_metadata()
        .map_err(|source| PilotError::Artifact {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.permissions().mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(PilotError::Composition(format!(
            "durable directory {} is not an owned private real directory",
            path.display()
        )));
    }
    Ok(identity)
}

fn real_directory_identity(path: &Path) -> Result<(u64, u64), PilotError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|source| PilotError::Artifact {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PilotError::Composition(format!(
            "directory {} is not a real directory",
            path.display()
        )));
    }
    Ok((metadata.dev(), metadata.ino()))
}

const fn binding(identity: (u64, u64)) -> DirectoryBinding {
    DirectoryBinding {
        device: identity.0,
        inode: identity.1,
    }
}

fn sync_directory(path: &Path) -> Result<(), PilotError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| PilotError::Artifact {
            path: path.to_path_buf(),
            source,
        })
}

impl InspectionRoot {
    fn open(requested: &Path) -> Result<Self, PilotError> {
        let requested_metadata = requested.symlink_metadata().map_err(|error| {
            output_error(requested, &format!("cannot inspect durable run: {error}"))
        })?;
        validate_private_directory_metadata(&requested_metadata, requested)?;
        let canonical = fs::canonicalize(requested).map_err(|error| {
            output_error(requested, &format!("cannot open durable run: {error}"))
        })?;
        let root = open_directory_nofollow(&canonical).map_err(|source| PilotError::Artifact {
            path: canonical.clone(),
            source,
        })?;
        let root_metadata = root.metadata().map_err(|source| PilotError::Artifact {
            path: canonical.clone(),
            source,
        })?;
        validate_private_directory_metadata(&root_metadata, &canonical)?;
        let root_identity = metadata_identity(&root_metadata);
        if metadata_identity(&requested_metadata) != root_identity {
            return Err(PilotError::Composition(
                "durable output root changed while it was opened".to_owned(),
            ));
        }
        let journal_path = canonical.join("journal");
        let journal = open_directory_at(&root, Path::new("journal")).map_err(|source| {
            PilotError::Artifact {
                path: journal_path.clone(),
                source,
            }
        })?;
        let journal_metadata = journal.metadata().map_err(|source| PilotError::Artifact {
            path: journal_path.clone(),
            source,
        })?;
        validate_private_directory_metadata(&journal_metadata, &journal_path)?;
        let retained = Self {
            requested: requested.to_path_buf(),
            canonical,
            root,
            root_identity,
            journal_identity: metadata_identity(&journal_metadata),
            journal,
        };
        retained.verify()?;
        Ok(retained)
    }

    fn read_root_file(
        &self,
        name: &Path,
        display: &Path,
        maximum: u64,
    ) -> Result<Vec<u8>, PilotError> {
        read_bounded_regular_at(&self.root, name, display, maximum)
    }

    fn read_journal_file(
        &self,
        name: &Path,
        display: &Path,
        maximum: u64,
    ) -> Result<Vec<u8>, PilotError> {
        read_bounded_regular_at(&self.journal, name, display, maximum)
    }

    fn verify(&self) -> Result<(), PilotError> {
        let root_metadata = self
            .root
            .metadata()
            .map_err(|source| PilotError::Artifact {
                path: self.canonical.clone(),
                source,
            })?;
        validate_private_directory_metadata(&root_metadata, &self.canonical)?;
        if metadata_identity(&root_metadata) != self.root_identity {
            return Err(PilotError::Composition(
                "retained durable output root changed identity".to_owned(),
            ));
        }
        let journal_path = self.canonical.join("journal");
        let journal_metadata = self
            .journal
            .metadata()
            .map_err(|source| PilotError::Artifact {
                path: journal_path.clone(),
                source,
            })?;
        validate_private_directory_metadata(&journal_metadata, &journal_path)?;
        if metadata_identity(&journal_metadata) != self.journal_identity {
            return Err(PilotError::Composition(
                "retained durable journal directory changed identity".to_owned(),
            ));
        }

        let named_root =
            open_directory_nofollow(&self.canonical).map_err(|source| PilotError::Artifact {
                path: self.canonical.clone(),
                source,
            })?;
        let named_root_metadata = named_root
            .metadata()
            .map_err(|source| PilotError::Artifact {
                path: self.canonical.clone(),
                source,
            })?;
        validate_private_directory_metadata(&named_root_metadata, &self.canonical)?;
        if metadata_identity(&named_root_metadata) != self.root_identity {
            return Err(PilotError::Composition(
                "durable output root was replaced".to_owned(),
            ));
        }
        let named_journal =
            open_directory_at(&named_root, Path::new("journal")).map_err(|source| {
                PilotError::Artifact {
                    path: journal_path.clone(),
                    source,
                }
            })?;
        let named_journal_metadata =
            named_journal
                .metadata()
                .map_err(|source| PilotError::Artifact {
                    path: journal_path.clone(),
                    source,
                })?;
        validate_private_directory_metadata(&named_journal_metadata, &journal_path)?;
        if metadata_identity(&named_journal_metadata) != self.journal_identity {
            return Err(PilotError::Composition(
                "durable journal directory was replaced".to_owned(),
            ));
        }
        let requested_metadata =
            self.requested
                .symlink_metadata()
                .map_err(|source| PilotError::Artifact {
                    path: self.requested.clone(),
                    source,
                })?;
        validate_private_directory_metadata(&requested_metadata, &self.requested)?;
        if metadata_identity(&requested_metadata) != self.root_identity
            || fs::canonicalize(&self.requested).map_err(|source| PilotError::Artifact {
                path: self.requested.clone(),
                source,
            })? != self.canonical
        {
            return Err(PilotError::Composition(
                "requested durable output binding changed".to_owned(),
            ));
        }
        let seal_path = self.canonical.join("orchestrator.rust-pilot.seal");
        let seal =
            self.read_root_file(Path::new("orchestrator.rust-pilot.seal"), &seal_path, 512)?;
        let expected = format!("nanika-rust-pilot-v1\n{}\n", env!("CARGO_PKG_VERSION"));
        if seal != expected.as_bytes() {
            return Err(PilotError::Composition(
                "durable output ownership seal is invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)
}

fn open_directory_at(directory: &File, name: &Path) -> io::Result<File> {
    validate_relative(name).map_err(|error| io::Error::other(error.to_string()))?;
    rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

fn validate_private_directory_metadata(
    metadata: &fs::Metadata,
    path: &Path,
) -> Result<(), PilotError> {
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(PilotError::Composition(format!(
            "durable directory {} is not an owned private real directory",
            path.display()
        )));
    }
    Ok(())
}

fn metadata_identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn read_bounded_regular_at(
    directory: &File,
    name: &Path,
    display: &Path,
    maximum: u64,
) -> Result<Vec<u8>, PilotError> {
    validate_relative(name)?;
    let file = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
    .map_err(|source| PilotError::Artifact {
        path: display.to_path_buf(),
        source,
    })?;
    read_bounded_open_file(file, display, maximum)
}

fn read_bounded_regular(path: &Path, maximum: u64) -> Result<Vec<u8>, PilotError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|source| PilotError::Artifact {
            path: path.to_path_buf(),
            source,
        })?;
    read_bounded_open_file(file, path, maximum)
}

fn read_bounded_open_file(
    mut file: File,
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>, PilotError> {
    let metadata = file.metadata().map_err(|source| PilotError::Artifact {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > maximum
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(PilotError::Composition(format!(
            "durable file {} failed type, mode, or size validation",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| PilotError::Artifact {
            path: path.to_path_buf(),
            source,
        })?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(PilotError::Composition(
            "durable file grew while reading".to_owned(),
        ));
    }
    let after = file.metadata().map_err(|source| PilotError::Artifact {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata_identity(&after) != metadata_identity(&metadata)
        || after.len() != metadata.len()
        || after.mtime() != metadata.mtime()
        || after.mtime_nsec() != metadata.mtime_nsec()
        || u64::try_from(bytes.len()).ok() != Some(metadata.len())
    {
        return Err(PilotError::Composition(format!(
            "durable file {} changed while reading",
            path.display()
        )));
    }
    Ok(bytes)
}

const fn phase_status(status: PhaseStatus) -> &'static str {
    match status {
        PhaseStatus::Pending => "pending",
        PhaseStatus::Running => "unresolved",
        PhaseStatus::Completed => "completed",
        PhaseStatus::Failed => "failed",
        PhaseStatus::Skipped => "skipped",
    }
}

fn json_bytes(path: &Path, value: &impl Serialize) -> Result<Vec<u8>, PilotError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| PilotError::Artifact {
        path: path.to_path_buf(),
        source: io::Error::other(error),
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn random_mission_id() -> Result<String, PilotError> {
    let mut bytes = [0_u8; 7];
    random_fill(&mut bytes).map_err(|error| {
        PilotError::Composition(format!("generating mission identity: {error}"))
    })?;
    Ok(format!(
        "rust-durable-{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn chained_digest(previous: &str, record: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(previous.as_bytes());
    digest.update([0]);
    digest.update(record);
    format!("{:x}", digest.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn utf8(path: &Path, label: &str) -> Result<String, PilotError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| PilotError::Composition(format!("{label} is not UTF-8")))
}

fn output_error(path: &Path, reason: &str) -> PilotError {
    PilotError::OutputDirectory {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    }
}

fn record_path(root: &Path, sequence: i64) -> PathBuf {
    root.join("journal").join(format!("{sequence:06}.json"))
}

#[cfg(test)]
mod status_overlap_tests {
    use std::error::Error;
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    #[test]
    fn terminal_projection_error_keeps_cancellation_and_release_admission_closed() -> TestResult {
        use super::super::cancellation::CancellationRecorder;

        const CHILD: &str = "NANIKA_TERMINAL_GATE_TEST_CHILD";
        if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
            let output = Command::new(std::env::current_exe()?)
                .args([
                    "durable::status_overlap_tests::terminal_projection_error_keeps_cancellation_and_release_admission_closed",
                    "--exact",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("NANIKA_RUST_FIRST_USE_PILOT", "1")
                .output()?;
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
        let fixture = StatusFixture::new()?;
        let authority = acquire(&fixture.root, &std::env::current_dir()?)?;
        let mut retained = DurableStore::fresh(fixture.root.clone(), authority)?;
        let first: JournalEnvelope = serde_json::from_slice(&fixture.first_record)?;
        retained.previous_digest = first.digest;
        retained.next_sequence = 2;
        retained.mission_started_at = Some(fixture.first_record_timestamp.clone());
        let store = Arc::new(Mutex::new(retained));
        let release = DurableReleaseAdmission {
            store: Arc::clone(&store),
        };
        assert!(release.admit_release().is_some());

        let mission = MissionId::new("rust-durable-00000000000000")?;
        let terminal = record(
            mission.as_str(),
            0,
            RecordKind::MissionTerminal,
            None,
            None,
            Some("failed"),
            Some("fixture failure"),
            Some(json!({"status":"failed"})),
            Some(&("0".repeat(64), "0".repeat(64))),
        );
        let authoritative = serde_json::to_value(&terminal)?;
        let metric = TerminalMetricIntent {
            mission: mission.as_str().to_owned(),
            domain: "local-pilot".to_owned(),
            task: MISSION.to_owned(),
            started_at: fixture.first_record_timestamp.clone(),
            finished_at: terminal.timestamp.clone(),
            duration_s: 0,
            status: "failed".to_owned(),
            decomp_source: "authored-phase-lines".to_owned(),
        };
        let projection = fixture
            .root
            .join(orchestrator_app::RUST_PILOT_METRICS_SNAPSHOT_FILE);
        if projection.exists() {
            fs::remove_file(&projection)?;
        }
        create_private_directory(&projection)?;
        // The real metrics transaction commits, but its atomic JSON projection
        // cannot replace a directory. Journal authority itself remains valid.
        let error = lock(&store)
            .publish_terminal(&mission, &metric, terminal)
            .err()
            .ok_or("projection unexpectedly succeeded")?;
        assert!(error.to_string().contains("terminal metric failed"));
        assert_eq!(
            lock(&store).metrics.terminal_transition(&mission)?,
            Some(authoritative.clone())
        );
        let cancellation = CancellationToken::new();
        let mut recorder = DurableCancellationRecorder {
            store: Arc::clone(&store),
            mission_id: mission.as_str().to_owned(),
            cancellation: cancellation.clone(),
        };
        assert_eq!(
            recorder.request(mission.as_str()),
            MissionCancellationAcknowledgement::Rejected
        );
        assert!(!cancellation.is_cancelled());
        assert!(release.admit_release().is_none());
        assert_eq!(
            fs::read(fixture.root.join("journal/000001.json"))?,
            fixture.first_record
        );
        assert_eq!(fs::read_dir(fixture.root.join("journal"))?.count(), 1);

        fs::remove_dir(&projection)?;
        lock(&store).metrics.recover_pending(&rfc3339_now())?;
        assert!(projection.is_file());
        assert_eq!(
            lock(&store).metrics.terminal_transition(&mission)?,
            Some(authoritative)
        );
        // Publication recovery does not reopen this live owner's admission.
        assert_eq!(
            recorder.request(mission.as_str()),
            MissionCancellationAcknowledgement::Rejected
        );
        Ok(())
    }

    #[test]
    fn metric_duration_accepts_the_pilots_fractional_timestamp() -> TestResult {
        let now = rfc3339_now();
        assert_eq!(elapsed_rfc3339_seconds(&now, &now)?, 0);
        assert_eq!(
            elapsed_rfc3339_seconds("2024-02-28T23:59:59Z", "2024-02-29T00:00:01.123456789Z",)?,
            2,
        );
        Ok(())
    }

    #[test]
    fn metric_duration_refuses_invalid_calendar_and_fractional_timestamps() {
        for timestamp in [
            "2023-02-29T00:00:00Z",
            "2024-04-31T00:00:00Z",
            "2024-02-29T00:00:00.Z",
            "2024-02-29T00:00:00.1234567890Z",
            "2024-02-29T00:00:00.aZ",
            "2024-02-29T00:00:00.éZ",
        ] {
            assert!(rfc3339_epoch_seconds(timestamp).is_err(), "{timestamp}");
        }
    }

    const MISSION: &str = "PHASE: code | OBJECTIVE: Implement it | PERSONA: engineer | ROLE: code\n\
PHASE: review | OBJECTIVE: Review it | PERSONA: reviewer | ROLE: review | DEPENDS: code\n\
PHASE: verify | OBJECTIVE: Verify it | PERSONA: operator-verifier | ROLE: verification | DEPENDS: review\n";

    struct StatusFixture {
        base: PathBuf,
        root: PathBuf,
        options: PilotOptions,
        first_record: Vec<u8>,
        first_record_timestamp: String,
        replacement_first_record: Vec<u8>,
        second_record: Vec<u8>,
    }

    impl StatusFixture {
        fn new() -> TestResult<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = fs::canonicalize(std::env::temp_dir())?.join(format!(
                "nanika-status-overlap-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = base.join("run");
            fs::create_dir(&base)?;
            create_private_directory(&root)?;
            create_private_directory(&root.join("journal"))?;
            write_private(
                &root.join("orchestrator.rust-pilot.seal"),
                format!("nanika-rust-pilot-v1\n{}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
            )?;

            let mission = authored_cycle::parse_mission(MISSION)?;
            let source = base.join("original-source");
            let manifest_options = PilotOptions {
                command: PilotCommand::Run,
                prompt_file: PathBuf::new(),
                output_dir: root.clone(),
                repo: Some(source.clone()),
                model: String::new(),
                persona: None,
                timeout: Duration::from_secs(30),
                claude_executable: OsString::from("claude"),
                runtime: "codex".to_owned(),
                codex_executable: OsString::from("unused-provider"),
                verification_argv: vec![OsString::from("unused-verifier")],
                verification_timeout: Duration::from_secs(30),
                authored_mission: true,
                durable: true,
                stop_after_phase: None,
                mission_id: None,
                progress_log: None,
                observe_follow: false,
                observe_format: super::observe::OutputFormat::Text,
            };
            let zero_digest = "0".repeat(64);
            let manifest = DurableManifest {
                schema: MANIFEST_SCHEMA.to_owned(),
                mission_id: "rust-durable-00000000000000".to_owned(),
                mission_text: MISSION.to_owned(),
                mission_digest: sha256(MISSION.as_bytes()),
                source: SourceBinding {
                    canonical_path: utf8(&source, "test source")?,
                    head: "saved-head".to_owned(),
                    directory: DirectoryBinding {
                        device: u64::MAX,
                        inode: u64::MAX,
                    },
                },
                plan: plan_binding(&mission),
                routes: route_bindings(&manifest_options, &mission),
                provider: ProviderBinding {
                    executable: "unused-provider".to_owned(),
                    executable_id: String::new(),
                    version: "saved-version".to_owned(),
                    model_override: String::new(),
                    timeout_secs: 30,
                },
                verifier: VerifierBinding {
                    argv: vec!["unused-verifier".to_owned()],
                    executable_id: String::new(),
                    timeout_secs: 30,
                },
                output_root: utf8(&root, "test output root")?,
                output_directory: binding(directory_identity(&root)?),
                snapshot: SnapshotBinding {
                    baseline: utf8(&root.join("source-head"), "test baseline")?,
                    workspace: utf8(&root.join("workspace"), "test workspace")?,
                    baseline_directory: DirectoryBinding {
                        device: u64::MAX,
                        inode: u64::MAX,
                    },
                    workspace_directory: DirectoryBinding {
                        device: u64::MAX,
                        inode: u64::MAX,
                    },
                    baseline_digest: zero_digest.clone(),
                    workspace_digest: zero_digest,
                },
                execution_environment: None,
            };
            let manifest_path = root.join("manifest.json");
            let manifest_bytes = json_bytes(&manifest_path, &manifest)?;
            write_private(&manifest_path, &manifest_bytes)?;

            let mut started = record(
                &manifest.mission_id,
                1,
                RecordKind::MissionStarted,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            started.event_id = "durable-event-000001".to_owned();
            let first_record_timestamp = started.timestamp.clone();
            let (first_record, first_digest) = envelope_bytes(
                &root.join("journal/000001.json"),
                &sha256(&manifest_bytes),
                started,
            )?;
            write_private(&root.join("journal/000001.json"), &first_record)?;

            let mut replacement_started = record(
                &manifest.mission_id,
                1,
                RecordKind::MissionStarted,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            replacement_started.event_id = "durable-event-000001".to_owned();
            replacement_started.timestamp = "2099-01-01T00:00:00Z".to_owned();
            let (replacement_first_record, _) = envelope_bytes(
                &root.join("replacement-journal/000001.json"),
                &sha256(&manifest_bytes),
                replacement_started,
            )?;

            let first_phase = mission.execution_order[0];
            let mut phase_started = record(
                &manifest.mission_id,
                2,
                RecordKind::PhaseStarted,
                Some(mission.phases[first_phase].id.as_str()),
                Some(mission.roles[first_phase].as_str()),
                Some("started"),
                None,
                None,
                None,
            );
            phase_started.event_id = "durable-event-000002".to_owned();
            let (second_record, _) = envelope_bytes(
                &root.join("journal/000002.json"),
                &first_digest,
                phase_started,
            )?;

            let mut options = manifest_options;
            options.command = PilotCommand::Status;
            Ok(Self {
                base,
                root,
                options,
                first_record,
                first_record_timestamp,
                replacement_first_record,
                second_record,
            })
        }

        fn spawn_partial_writer(&self, finish: bool) -> TestResult<(Child, PartialAppendObserver)> {
            let complete = self.base.join("complete-record.json");
            let record = self.root.join("journal/000002.json");
            let start = self.base.join("start-append");
            let ready = self.base.join("partial-visible");
            let finish_barrier = self.base.join("finish-append");
            let completed = self.base.join("append-complete");
            fs::write(&complete, &self.second_record)?;
            let split = self.second_record.len() / 2;
            let script = if finish {
                "while [ ! -e \"$4\" ]; do sleep 0.01; done\n\
dd if=\"$1\" of=\"$2\" bs=1 count=\"$3\" 2>/dev/null\n\
chmod 600 \"$2\"\n\
: > \"$5\"\n\
while [ ! -e \"$6\" ]; do sleep 0.01; done\n\
dd if=\"$1\" bs=1 skip=\"$3\" 2>/dev/null >> \"$2\"\n\
: > \"$7\"\n"
            } else {
                "while [ ! -e \"$4\" ]; do sleep 0.01; done\n\
dd if=\"$1\" of=\"$2\" bs=1 count=\"$3\" 2>/dev/null\n\
chmod 600 \"$2\"\n\
: > \"$5\"\n"
            };
            let child = Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .arg("status-partial-writer")
                .arg(&complete)
                .arg(&record)
                .arg(split.to_string())
                .arg(&start)
                .arg(&ready)
                .arg(&finish_barrier)
                .arg(&completed)
                .spawn()?;
            Ok((
                child,
                PartialAppendObserver {
                    start,
                    ready,
                    finish: finish.then_some(finish_barrier),
                    completed: finish.then_some(completed),
                    calls: 0,
                },
            ))
        }
    }

    impl Drop for StatusFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn create_private_directory(path: &Path) -> TestResult {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn write_private(path: &Path, bytes: &[u8]) -> TestResult {
        fs::write(path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn envelope_bytes(
        path: &Path,
        previous: &str,
        record: JournalRecord,
    ) -> Result<(Vec<u8>, String), PilotError> {
        let record_bytes = serde_json::to_vec(&record)
            .map_err(|error| PilotError::Composition(error.to_string()))?;
        let digest = chained_digest(previous, &record_bytes);
        let envelope = JournalEnvelope {
            previous_digest: previous.to_owned(),
            digest: digest.clone(),
            record,
        };
        Ok((json_bytes(path, &envelope)?, digest))
    }

    fn observer_failure(action: &str, error: impl std::fmt::Display) -> PilotError {
        PilotError::Composition(format!("status test observer could not {action}: {error}"))
    }

    fn wait_for(path: &Path) -> Result<(), PilotError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() {
            if Instant::now() >= deadline {
                return Err(PilotError::Composition(format!(
                    "status test observer timed out waiting for {}",
                    path.display()
                )));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    struct PartialAppendObserver {
        start: PathBuf,
        ready: PathBuf,
        finish: Option<PathBuf>,
        completed: Option<PathBuf>,
        calls: usize,
    }

    impl PartialAppendObserver {
        fn release_writer(&self) -> TestResult {
            fs::write(&self.start, b"start\n")?;
            if let Some(finish) = &self.finish {
                fs::write(finish, b"finish\n")?;
            }
            Ok(())
        }
    }

    impl StatusReadObserver for PartialAppendObserver {
        fn before_enumeration(
            &mut self,
            _root: &InspectionRoot,
            _observation: usize,
        ) -> Result<(), PilotError> {
            match (
                self.calls,
                self.finish.as_ref().zip(self.completed.as_ref()),
            ) {
                (0, _) => {
                    fs::write(&self.start, b"start\n")
                        .map_err(|error| observer_failure("release partial writer", error))?;
                    wait_for(&self.ready)?;
                }
                (1, Some((finish, completed))) => {
                    fs::write(finish, b"finish\n")
                        .map_err(|error| observer_failure("release completing writer", error))?;
                    wait_for(completed)?;
                }
                _ => {}
            }
            self.calls += 1;
            Ok(())
        }
    }

    #[test]
    fn status_retries_a_visible_partial_final_name_then_reports_its_valid_completion() -> TestResult
    {
        let fixture = StatusFixture::new()?;
        let (mut writer, mut observer) = fixture.spawn_partial_writer(true)?;
        let report = status_with_observer(&fixture.options, &mut observer);
        observer.release_writer()?;
        assert!(writer.wait()?.success());
        let report = report?;
        assert!(observer.calls >= 3);
        assert_eq!(report["last_journal_sequence"], 2);
        assert_eq!(report["recorded_status"], "unresolved");
        assert_eq!(report["durability"], "not_attested");
        Ok(())
    }

    #[test]
    fn status_boundedly_refuses_a_visible_partial_final_name_that_stays_malformed() -> TestResult {
        let fixture = StatusFixture::new()?;
        let (mut writer, mut observer) = fixture.spawn_partial_writer(false)?;
        let started = Instant::now();
        let error = match status_with_observer(&fixture.options, &mut observer) {
            Ok(_) => return Err("persistent partial record was accepted".into()),
            Err(error) => error,
        };
        assert!(writer.wait()?.success());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(observer.calls >= STATUS_READ_ATTEMPTS);
        assert!(error.to_string().contains("did not stabilize"));
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum ReplacementKind {
        Root,
        Journal,
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum ReplacementTiming {
        Enumeration,
        RecordRead,
    }

    struct NamespaceReplacementObserver {
        kind: ReplacementKind,
        timing: ReplacementTiming,
        root: PathBuf,
        first_record: Vec<u8>,
        replacement_first_record: Vec<u8>,
        second_record: Vec<u8>,
        displaced: PathBuf,
        active: bool,
        injections: usize,
    }

    impl NamespaceReplacementObserver {
        fn new(fixture: &StatusFixture, kind: ReplacementKind, timing: ReplacementTiming) -> Self {
            let suffix = match kind {
                ReplacementKind::Root => "root",
                ReplacementKind::Journal => "journal",
            };
            Self {
                kind,
                timing,
                root: fixture.root.clone(),
                first_record: fixture.first_record.clone(),
                replacement_first_record: fixture.replacement_first_record.clone(),
                second_record: fixture.second_record.clone(),
                displaced: fixture.base.join(format!("displaced-{suffix}")),
                active: false,
                injections: 0,
            }
        }

        fn populate_replacement_journal(&self, journal: &Path) -> Result<(), PilotError> {
            create_private_directory(journal)
                .map_err(|error| observer_failure("create replacement journal", error))?;
            let first_record = match self.timing {
                ReplacementTiming::Enumeration => &self.first_record,
                ReplacementTiming::RecordRead => &self.replacement_first_record,
            };
            write_private(&journal.join("000001.json"), first_record)
                .map_err(|error| observer_failure("write first replacement record", error))?;
            if self.timing == ReplacementTiming::Enumeration {
                write_private(&journal.join("000002.json"), &self.second_record).map_err(
                    |error| observer_failure("write injected replacement record", error),
                )?;
            }
            Ok(())
        }

        fn replace(&mut self) -> Result<(), PilotError> {
            match self.kind {
                ReplacementKind::Root => {
                    fs::rename(&self.root, &self.displaced)
                        .map_err(|error| observer_failure("displace retained root name", error))?;
                    create_private_directory(&self.root)
                        .map_err(|error| observer_failure("create replacement root", error))?;
                    self.populate_replacement_journal(&self.root.join("journal"))?;
                }
                ReplacementKind::Journal => {
                    fs::rename(self.root.join("journal"), &self.displaced).map_err(|error| {
                        observer_failure("displace retained journal name", error)
                    })?;
                    self.populate_replacement_journal(&self.root.join("journal"))?;
                }
            }
            self.active = true;
            self.injections += 1;
            Ok(())
        }

        fn restore(&mut self) -> Result<(), PilotError> {
            if !self.active {
                return Ok(());
            }
            match self.kind {
                ReplacementKind::Root => {
                    fs::remove_dir_all(&self.root)
                        .map_err(|error| observer_failure("remove replacement root", error))?;
                    fs::rename(&self.displaced, &self.root)
                        .map_err(|error| observer_failure("restore retained root name", error))?;
                }
                ReplacementKind::Journal => {
                    fs::remove_dir_all(self.root.join("journal"))
                        .map_err(|error| observer_failure("remove replacement journal", error))?;
                    fs::rename(&self.displaced, self.root.join("journal")).map_err(|error| {
                        observer_failure("restore retained journal name", error)
                    })?;
                }
            }
            self.active = false;
            Ok(())
        }
    }

    impl StatusReadObserver for NamespaceReplacementObserver {
        fn before_enumeration(
            &mut self,
            _retained: &InspectionRoot,
            observation: usize,
        ) -> Result<(), PilotError> {
            if self.timing != ReplacementTiming::Enumeration || observation != 0 {
                return Ok(());
            }
            self.replace()
        }

        fn after_enumeration(
            &mut self,
            _retained: &InspectionRoot,
            observation: usize,
        ) -> Result<(), PilotError> {
            if self.timing != ReplacementTiming::Enumeration || observation != 0 {
                return Ok(());
            }
            self.restore()
        }

        fn before_record_read(
            &mut self,
            _retained: &InspectionRoot,
            _observation: usize,
            index: usize,
        ) -> Result<(), PilotError> {
            if self.timing != ReplacementTiming::RecordRead || index != 0 {
                return Ok(());
            }
            self.replace()
        }

        fn after_record_read(
            &mut self,
            _retained: &InspectionRoot,
            _observation: usize,
            index: usize,
        ) -> Result<(), PilotError> {
            if self.timing != ReplacementTiming::RecordRead || index != 0 {
                return Ok(());
            }
            self.restore()
        }
    }

    #[test]
    fn status_namespace_aba_cannot_inject_records_outside_retained_descriptors() -> TestResult {
        for kind in [ReplacementKind::Root, ReplacementKind::Journal] {
            let fixture = StatusFixture::new()?;
            let mut observer =
                NamespaceReplacementObserver::new(&fixture, kind, ReplacementTiming::Enumeration);
            let report = status_with_observer(&fixture.options, &mut observer)?;
            assert_eq!(observer.injections, 1);
            assert!(!observer.active);
            assert_eq!(report["last_journal_sequence"], 1);
            assert_eq!(report["recorded_status"], "in_progress");
            assert_eq!(report["durability"], "not_attested");
        }
        Ok(())
    }

    #[test]
    fn status_mid_record_namespace_aba_cannot_substitute_same_named_bytes() -> TestResult {
        for kind in [ReplacementKind::Root, ReplacementKind::Journal] {
            let fixture = StatusFixture::new()?;
            assert_ne!(fixture.first_record, fixture.replacement_first_record);
            let expected_timestamp = fixture.first_record_timestamp.clone();
            let mut observer =
                NamespaceReplacementObserver::new(&fixture, kind, ReplacementTiming::RecordRead);
            let report = status_with_observer(&fixture.options, &mut observer)?;
            assert_eq!(observer.injections, 2);
            assert!(!observer.active);
            assert_eq!(
                report["last_journal_timestamp"],
                expected_timestamp.as_str()
            );
            assert_ne!(report["last_journal_timestamp"], "2099-01-01T00:00:00Z");
            assert_eq!(report["last_journal_sequence"], 1);
            assert_eq!(report["recorded_status"], "in_progress");
            assert_eq!(report["durability"], "not_attested");
        }
        Ok(())
    }
}
