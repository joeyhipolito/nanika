//! End-to-end coverage for the local-executor protocol boundary.
//!
//! Dispatch reuses the hermetic fixture protocol: the attested fixture helper
//! runs only under exact fingerprint admission, and the protocol fails closed
//! *before any spawn* for a non-fixture executable, a populated environment, a
//! foreign working root, and a real network URL. Network egress is refused
//! structurally — no effect adapter services `EffectKind::NetworkRequest`, so a
//! URL-bearing effect is denied by the deny-all effect authority the runtime
//! injects into every dispatch.

use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessService,
    FixtureSequentialRun, FixtureSequentialRuntime, FixtureWorkspaceSeed, FreshFixtureAuthority,
    IsolatedFixtureRoot, SupervisorLimits, WorkspaceAuthority,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, MissionState,
    PhaseDefinition, PhaseId,
};
use orchestrator_exec::{
    AttemptOutcome, Clock, DispatchRequest, EffectBudget, EffectKind, EffectReceipt, EffectRequest,
    EffectService, EffectServiceError, EffectServiceErrorKind, Effort, EventReceipt, EventSink,
    EventSinkError, EventSinkErrorKind, ExecutionContext, ExecutionRequest, ExecutionRequestDraft,
    ExecutorRegistry, MechanicalTermination, PartialWork, PhaseExecutor, ProcessPurpose,
    ProcessReceipt, ProcessRequest, ProcessServiceErrorKind, RuntimeCaps, RuntimeDescriptor,
    RuntimeFamily, WatchdogDecision, WatchdogPolicy, WorkerEventDraft, WorkerIdentity,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "local-executor-mission";
const PHASE: &str = "phase-1";
const WORKER: &str = "worker-1";
const RUNTIME: &str = "fixture-runtime";
const EXECUTABLE: &str = "native-helper";

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

struct FixtureCase {
    parent: PathBuf,
    _authority: FreshFixtureAuthority,
    workspace: Option<WorkspaceAuthority>,
    process: Option<FixtureProcessService>,
    worker_root: PathBuf,
}

impl FixtureCase {
    fn take_workspace(&mut self) -> TestResult<WorkspaceAuthority> {
        self.workspace
            .take()
            .ok_or_else(|| "fixture workspace was already consumed".into())
    }

    fn take_process(&mut self) -> TestResult<FixtureProcessService> {
        self.process
            .take()
            .ok_or_else(|| "fixture process service was already consumed".into())
    }
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        self.process.take();
        self.workspace.take();
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn one_phase_state() -> TestResult<MissionState> {
    Ok(MissionState::new(
        MissionId::new(MISSION)?,
        vec![PhaseDefinition {
            id: PhaseId::new(PHASE)?,
            dependencies: Vec::new(),
        }],
    )?)
}

fn initial_checkpoint(state: &MissionState) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: MISSION.to_owned(),
        status: "pending".to_owned(),
        plan: Some(CheckpointPlan {
            id: "fixture-plan".to_owned(),
            phases: state
                .phases()
                .map(|phase| CheckpointPhase {
                    id: phase.id.to_string(),
                    status: "pending".to_owned(),
                    ..CheckpointPhase::default()
                })
                .collect(),
            ..CheckpointPlan::default()
        }),
        ..CheckpointProjection::default()
    }
}

fn fixture(label: &str, state: &MissionState) -> TestResult<FixtureCase> {
    let number = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temporary.join(format!(
        "orchestrator-rs-local-executor-{}-{number}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&parent);
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let helper = std::fs::read(env!("CARGO_BIN_EXE_orchestrator-owned-process-fixture"))?;
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
        .with_expected_fixture_helper(&helper);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let workspace = authority.create_workspace(
        MissionId::new(MISSION)?,
        FixtureWorkspaceSeed::new(
            b"fixture mission\n".to_vec(),
            &initial_checkpoint(state),
            b"{}".to_vec(),
        )?,
    )?;
    let executable = authority.install_fixture_executable(EXECUTABLE, &helper)?;
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
    let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
    let worker_root = std::fs::canonicalize(root.join("workspaces").join(MISSION))?;
    let process = FixtureProcessService::new(process_authority, CancellationToken::new())?;
    Ok(FixtureCase {
        parent,
        _authority: authority,
        workspace: Some(workspace),
        process: Some(process),
        worker_root,
    })
}

fn descriptor() -> Option<RuntimeDescriptor> {
    Some(RuntimeDescriptor::new(
        RuntimeFamily::parse(RUNTIME).ok()?,
        RuntimeCaps {
            tool_use: false,
            session_resume: false,
            streaming: false,
            cost_report: false,
            artifacts: false,
        },
    ))
}

fn request(worker_root: &Path) -> TestResult<ExecutionRequest> {
    Ok(ExecutionRequest::new(ExecutionRequestDraft {
        mission: MISSION.to_owned(),
        phase: PHASE.to_owned(),
        attempt: 1,
        revision: 1,
        objective: "exercise the local-executor protocol".to_owned(),
        persona: "fixture".to_owned(),
        role: "implementer".to_owned(),
        domain: "dev".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec!["fixture-only".to_owned()],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(RUNTIME)?,
        model: "fixture-model".to_owned(),
        effort: Effort::High,
        max_turns: 4,
        worker_dir: worker_root.to_path_buf(),
        target_dir: None,
        resume_from: None,
        hook_script: None,
    })?)
}

// --- Minimal inert services for building an ExecutionContext directly. ---

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct FixedWatchdog;

impl WatchdogPolicy for FixedWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

struct DeniedEffect(EffectServiceError);

impl DeniedEffect {
    fn new() -> TestResult<Self> {
        Ok(Self(EffectServiceError::new(
            EffectServiceErrorKind::Denied,
            "local executor does not admit effects",
        )?))
    }
}

impl EffectService for DeniedEffect {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.0.clone())
    }
}

struct UnusedSink(EventSinkError);

impl UnusedSink {
    fn new() -> TestResult<Self> {
        Ok(Self(EventSinkError::new(
            EventSinkErrorKind::Rejected,
            "local executor process check emits no worker events",
        )?))
    }
}

impl EventSink for UnusedSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        Err(self.0.clone())
    }
}

fn run_process(
    service: &FixtureProcessService,
    request: &ProcessRequest,
    timeout: Duration,
) -> TestResult<Result<ProcessReceipt, orchestrator_exec::ProcessServiceError>> {
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = UnusedSink::new()?;
    let mut context = ExecutionContext::new(
        service,
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        WorkerIdentity::new(MISSION, PHASE, WORKER)?,
        Instant::now() + timeout,
    );
    Ok(context.run_process(request))
}

fn exact_request(root: &Path, mode: &str) -> TestResult<ProcessRequest> {
    Ok(
        ProcessRequest::new(ProcessPurpose::ProviderWorker, EXECUTABLE, root)?
            .with_argument(mode)?,
    )
}

/// An executor that attempts to reach a real URL through the injected effect
/// authority, capturing the resulting error kind. It never returns completed —
/// the point is that the network effect is refused, not that work succeeds.
struct NetworkProbeExecutor {
    observed: Arc<Mutex<Option<EffectServiceErrorKind>>>,
    admitted: Arc<Mutex<bool>>,
}

impl PhaseExecutor for NetworkProbeExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        if let Ok(effect) = EffectRequest::new(
            EffectKind::NetworkRequest,
            "https://api.anthropic.com/v1/messages",
            "local-executor-network-probe",
        ) {
            match context.run_effect(&effect) {
                Ok(_) => {
                    if let Ok(mut admitted) = self.admitted.lock() {
                        *admitted = true;
                    }
                }
                Err(error) => {
                    if let Ok(mut observed) = self.observed.lock() {
                        *observed = Some(error.kind());
                    }
                }
            }
        }
        AttemptOutcome::incomplete(
            MechanicalTermination::ProviderStreamEnded,
            None,
            PartialWork::empty(),
            Duration::from_millis(1),
        )
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        descriptor()
    }
}

#[test]
fn attested_fixture_helper_runs_only_under_exact_admission() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("attested-helper", &state)?;
    let service = case.take_process()?;
    let receipt = run_process(
        &service,
        &exact_request(&case.worker_root, "zero")?,
        Duration::from_secs(3),
    )??;
    assert!(
        receipt.is_success(),
        "the attested helper should run to success"
    );
    assert!(!service.has_unresolved_processes());
    Ok(())
}

#[test]
fn non_fixture_executable_and_populated_environment_fail_closed_before_spawn() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("deny-boundaries", &state)?;
    let service = case.take_process()?;
    let other_root = case.parent.join("other-root");
    private_dir(&other_root)?;
    let cases = [
        (
            ProcessRequest::new(ProcessPurpose::Tool, EXECUTABLE, &case.worker_root)?,
            ProcessServiceErrorKind::Denied,
        ),
        (
            ProcessRequest::new(
                ProcessPurpose::ProviderWorker,
                "foreign-helper",
                &case.worker_root,
            )?,
            ProcessServiceErrorKind::NotEnrolled,
        ),
        (
            ProcessRequest::new(ProcessPurpose::ProviderWorker, EXECUTABLE, &other_root)?,
            ProcessServiceErrorKind::OutsideRoot,
        ),
        (
            ProcessRequest::new(
                ProcessPurpose::ProviderWorker,
                EXECUTABLE,
                &case.worker_root,
            )?
            .with_environment("POISON", "1")?,
            ProcessServiceErrorKind::Denied,
        ),
    ];
    for (request, expected) in cases {
        match run_process(&service, &request, Duration::from_secs(1))? {
            Ok(_) => return Err("a non-enrolled request was admitted before spawn".into()),
            Err(error) => assert_eq!(error.kind(), expected),
        }
        assert!(!service.has_unresolved_processes());
    }
    Ok(())
}

#[test]
fn real_network_url_is_denied_by_the_dispatch_effect_authority() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("network-denied", &state)?;
    let observed = Arc::new(Mutex::new(None));
    let admitted = Arc::new(Mutex::new(false));
    let executor = Arc::new(NetworkProbeExecutor {
        observed: Arc::clone(&observed),
        admitted: Arc::clone(&admitted),
    });
    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register(RUNTIME, executor)?;
    let runtime =
        FixtureSequentialRuntime::new(case.take_process()?, registry, Duration::from_secs(30))?;
    let mut runner = FixtureSequentialRun::new(
        case.take_workspace()?,
        state,
        runtime,
        RUNTIME,
        request(&case.worker_root)?,
        WORKER,
        Duration::from_secs(3),
        false,
    )?;
    let _attempt = runner.run_attempt()?;

    let network_admitted = *admitted.lock().map_err(|_| "admitted flag poisoned")?;
    assert!(
        !network_admitted,
        "a real network URL must never be admitted"
    );
    let observed_kind = *observed.lock().map_err(|_| "observed kind poisoned")?;
    assert!(
        matches!(observed_kind, Some(EffectServiceErrorKind::Denied)),
        "the network effect must be denied, got {observed_kind:?}"
    );
    Ok(())
}
