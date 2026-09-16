use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessService,
    FixtureProjectionFault, FixtureSequentialRun, FixtureSequentialRunError,
    FixtureSequentialRuntime, FixtureTerminalDisposition, FixtureWorkspaceSeed,
    FreshFixtureAuthority, IsolatedFixtureRoot, LifecycleCoordinator, LifecycleError,
    OwnedChildState, SupervisorLimits, WorkspaceAuthority,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, MissionState, MissionStatus,
    PhaseDefinition, PhaseId, PhaseStatus, ReducerTransition, VerificationClass,
    VerificationDecision, VerificationMode, VerificationOutcome, decide_verification,
    decode_checkpoint,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Clock, DispatchRequest, EffectBudget, EffectReceipt,
    EffectRequest, EffectService, EffectServiceError, EffectServiceErrorKind, Effort,
    ExecutionContext, ExecutionRequest, ExecutionRequestDraft, ExecutorRegistry,
    MechanicalTermination, PartialWork, PhaseExecutor, ProcessBudget, ProcessExitStatus,
    ProcessPurpose, ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError,
    ProcessServiceErrorKind, RuntimeCaps, RuntimeDescriptor, RuntimeFamily, RuntimeRegistryError,
    WatchdogDecision, WatchdogPolicy, WorkerEventKind, WorkerEventPayload, WorkerIdentity,
    WorkerOutput, WorkerOutputFields, WorkerOutputKind,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "fixture-sequential-mission";
const PHASE: &str = "phase-1";
const WORKER: &str = "worker-1";
const RUNTIME: &str = "fixture-runtime";
const EXECUTABLE: &str = "native-helper";

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

/// Serializes every fixture case that may own a real child process.
///
/// `has_unresolved_children` reads the *process-wide* owned-process registry —
/// a deliberate safety property, since a runner must never report a clean
/// attempt while an owned child is still live. That makes the registry shared
/// state across concurrently running tests in this binary, so cases take a turn
/// instead of the production check being weakened to a per-case view.
///
/// Deliberately duplicated from `lifecycle::owned_process_serialization`: that
/// module is `#[cfg(test)]`, so it exists only in the library test binary and
/// cannot be reached from an integration test. Sharing it would mean exporting
/// a test-only lock through the crate's public API.
mod owned_process_turn {
    use std::{
        cell::Cell,
        sync::{Mutex, MutexGuard, PoisonError},
    };

    static TURN: Mutex<()> = Mutex::new(());

    thread_local! {
        /// Nesting depth on this thread; [`Mutex`] is not reentrant.
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// Held for as long as a case may own a child process. Declared last in
    /// [`super::FixtureCase`] so it is released after every other field.
    pub(super) struct OwnedProcessTurn(
        #[expect(dead_code, reason = "held, not read")] Option<MutexGuard<'static, ()>>,
    );

    impl OwnedProcessTurn {
        pub(super) fn take() -> Self {
            let depth = DEPTH.with(|depth| {
                let current = depth.get();
                depth.set(current.saturating_add(1));
                current
            });
            Self((depth == 0).then(|| TURN.lock().unwrap_or_else(PoisonError::into_inner)))
        }
    }

    impl Drop for OwnedProcessTurn {
        fn drop(&mut self) {
            DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
}

use owned_process_turn::OwnedProcessTurn;

struct FixtureCase {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
    workspace: Option<WorkspaceAuthority>,
    process: Option<FixtureProcessService>,
    cancellation: CancellationToken,
    worker_root: PathBuf,
    /// Declared last so it is released after every other field.
    _serialization: OwnedProcessTurn,
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

    fn workspace_path(&self) -> PathBuf {
        self.root.join("workspaces").join(MISSION)
    }
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        self.process.take();
        self.workspace.take();
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn pass_decision() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    )
}

fn block_decision() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Fail),
        VerificationMode::Block,
    )
}

fn warn_decision() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Fail),
        VerificationMode::Warn,
    )
}

fn cancelled_decision() -> VerificationDecision {
    decide_verification(VerificationOutcome::Cancelled, VerificationMode::Block)
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

fn read_optional(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn event_types(path: &Path) -> TestResult<Vec<String>> {
    std::fs::read(path)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let event: Value = serde_json::from_slice(line)?;
            event
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "event type is missing".into())
        })
        .collect()
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

fn two_phase_state() -> TestResult<MissionState> {
    Ok(MissionState::new(
        MissionId::new(MISSION)?,
        vec![
            PhaseDefinition {
                id: PhaseId::new(PHASE)?,
                dependencies: Vec::new(),
            },
            PhaseDefinition {
                id: PhaseId::new("phase-2")?,
                dependencies: vec![PhaseId::new(PHASE)?],
            },
        ],
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
        "orchestrator-rs-fixture-sequential-{}-{number}-{label}",
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
    let cancellation = CancellationToken::new();
    let process = FixtureProcessService::new(process_authority, cancellation.clone())?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
        workspace: Some(workspace),
        process: Some(process),
        cancellation,
        worker_root,
        _serialization: OwnedProcessTurn::take(),
    })
}

fn request(worker_root: &Path) -> TestResult<ExecutionRequest> {
    request_with_dependencies(worker_root, Vec::new())
}

fn request_with_dependencies(
    worker_root: &Path,
    dependencies: Vec<String>,
) -> TestResult<ExecutionRequest> {
    Ok(ExecutionRequest::new(ExecutionRequestDraft {
        mission: MISSION.to_owned(),
        phase: PHASE.to_owned(),
        attempt: 1,
        revision: 1,
        objective: "exercise one recoverable fixture attempt".to_owned(),
        persona: "fixture".to_owned(),
        role: "implementer".to_owned(),
        domain: "dev".to_owned(),
        skills: vec!["rust-best-practices".to_owned()],
        dependencies,
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

fn registry(executor: Arc<dyn PhaseExecutor>) -> TestResult<ExecutorRegistry> {
    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register(RUNTIME, executor)?;
    Ok(registry)
}

fn reopened_runner(
    case: &FixtureCase,
    state: MissionState,
    executors: ExecutorRegistry,
    executable_label: &str,
    requested_runtime: &str,
    timeout: Duration,
    cancelled: bool,
) -> TestResult<FixtureSequentialRun> {
    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let helper = std::fs::read(env!("CARGO_BIN_EXE_orchestrator-owned-process-fixture"))?;
    let executable = case
        .authority
        .install_fixture_executable(executable_label, &helper)?;
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
    let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
    let cancellation = CancellationToken::new();
    if cancelled {
        let _cancelled_now = cancellation.cancel();
    }
    let process = FixtureProcessService::new(process_authority, cancellation)?;
    let runtime = FixtureSequentialRuntime::new(process, executors, Duration::from_secs(30))?;
    Ok(FixtureSequentialRun::new(
        workspace,
        state,
        runtime,
        requested_runtime,
        request(&case.worker_root)?,
        WORKER,
        timeout,
        false,
    )?)
}

fn descriptor(streaming: bool) -> Option<RuntimeDescriptor> {
    Some(RuntimeDescriptor::new(
        RuntimeFamily::parse(RUNTIME).ok()?,
        RuntimeCaps {
            tool_use: false,
            session_resume: false,
            streaming,
            cost_report: false,
            artifacts: false,
        },
    ))
}

struct SuccessfulExecutor {
    invocations: Arc<AtomicU64>,
}

impl PhaseExecutor for SuccessfulExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        self.invocations.fetch_add(1, Ordering::Relaxed);
        let output = WorkerOutput::new(WorkerOutputFields {
            chunk: Some("fixture output".to_owned()),
            event_kind: Some(WorkerOutputKind::Text),
            streaming: Some(true),
            tool_name: None,
            is_error: None,
            output_len: Some("fixture output".len()),
            duration: None,
        })
        .map(WorkerEventPayload::Output);
        if let Ok(output) = output {
            let _receipt = context.emit(&output);
        }
        AttemptOutcome::completed(
            "fixture completed",
            AttemptEvidence::new(),
            Duration::from_millis(1),
        )
        .unwrap_or_else(|_| {
            AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::ZERO,
            )
        })
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        descriptor(true)
    }
}

struct IncompleteExecutor {
    invocations: Arc<AtomicU64>,
    termination: MechanicalTermination,
}

impl PhaseExecutor for IncompleteExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        _context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        self.invocations.fetch_add(1, Ordering::Relaxed);
        AttemptOutcome::incomplete(
            self.termination,
            None,
            PartialWork::empty(),
            Duration::from_millis(1),
        )
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        descriptor(false)
    }
}

struct ProcessFailureExecutor {
    process: ProcessRequest,
}

impl PhaseExecutor for ProcessFailureExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        if context
            .record_partial_output("partial output survived")
            .is_err()
        {
            return AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::ZERO,
            );
        }
        let _receipt = context.run_process(&self.process);
        AttemptOutcome::incomplete(
            MechanicalTermination::ProviderStreamEnded,
            None,
            PartialWork::empty(),
            Duration::from_millis(1),
        )
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        descriptor(false)
    }
}

fn runner(
    case: &mut FixtureCase,
    state: MissionState,
    executor: Arc<dyn PhaseExecutor>,
    timeout: Duration,
) -> TestResult<FixtureSequentialRun> {
    let request = request(&case.worker_root)?;
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        registry(executor)?,
        Duration::from_secs(30),
    )?;
    Ok(FixtureSequentialRun::new(
        case.take_workspace()?,
        state,
        runtime,
        RUNTIME,
        request,
        WORKER,
        timeout,
        false,
    )?)
}

#[test]
fn successful_attempt_is_durable_and_terminal_replay_does_not_reinvoke_executor() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("success-replay", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

    let first = runner.run_attempt()?;
    let executed = first
        .into_executed()
        .ok_or("first fixture attempt unexpectedly replayed")?;
    assert!(executed.is_completed());
    assert_eq!(invocations.load(Ordering::Relaxed), 1);

    let workspace = case.workspace_path();
    let event_log = std::fs::read(workspace.join("events.jsonl"))?;
    let event_text = std::str::from_utf8(&event_log)?;
    assert!(event_text.contains("worker.spawned"));
    assert!(event_text.contains("worker.output"));
    assert!(event_text.contains("worker.completed"));
    let checkpoint = decode_checkpoint(&std::fs::read(workspace.join("checkpoint.json"))?)?;
    assert_eq!(checkpoint.projection.status, "in_progress");
    assert_eq!(
        checkpoint
            .projection
            .plan
            .as_ref()
            .and_then(|plan| plan.phases.first())
            .map(|phase| phase.status.as_str()),
        Some("running")
    );

    let replay = runner.run_attempt()?;
    assert!(replay.replayed().is_some());
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(workspace.join("events.jsonl"))?, event_log);
    Ok(())
}

#[test]
fn terminal_engine_passes_only_with_exact_success_order_and_replays_without_duplicates()
-> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-success-order", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;
    let event_path = case.workspace_path().join("events.jsonl");

    let first = runner.run_to_terminal(pass_decision())?;
    assert_eq!(
        (
            first.disposition(),
            first.mission_status(),
            first.phase_status(),
            first.executed_outcome().is_some(),
            first.evidence().is_some(),
        ),
        (
            FixtureTerminalDisposition::Executed,
            MissionStatus::Completed,
            PhaseStatus::Completed,
            true,
            true,
        )
    );
    assert_eq!(
        event_types(&event_path)?,
        [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.output",
            "worker.completed",
            "phase.completed",
            "mission.completed",
        ]
    );
    let terminal_bytes = std::fs::read(&event_path)?;

    let replay = runner.run_to_terminal(pass_decision())?;
    assert_eq!(
        (
            replay.disposition(),
            replay.mission_status(),
            replay.phase_status(),
            replay
                .durable_attempt()
                .map(|attempt| attempt.terminal().kind()),
            invocations.load(Ordering::Relaxed),
        ),
        (
            FixtureTerminalDisposition::Replayed,
            MissionStatus::Completed,
            PhaseStatus::Completed,
            Some(WorkerEventKind::Completed),
            1,
        )
    );
    assert_eq!(std::fs::read(event_path)?, terminal_bytes);
    Ok(())
}

#[test]
fn completed_attempt_terminal_decision_table_is_structured() -> TestResult {
    let rows = [
        (
            "terminal-completed-pass",
            pass_decision(),
            MissionStatus::Completed,
            PhaseStatus::Completed,
            vec!["phase.completed", "mission.completed"],
        ),
        (
            "terminal-completed-block",
            block_decision(),
            MissionStatus::Failed,
            PhaseStatus::Failed,
            vec!["phase.failed", "mission.failed"],
        ),
        (
            "terminal-completed-cancelled",
            cancelled_decision(),
            MissionStatus::Cancelled,
            PhaseStatus::Skipped,
            vec!["mission.cancelled"],
        ),
    ];

    for (label, decision, mission_status, phase_status, expected_tail) in rows {
        let state = one_phase_state()?;
        let mut case = fixture(label, &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

        let result = runner.run_to_terminal(decision)?;
        assert_eq!(
            (
                result.mission_status(),
                result.phase_status(),
                invocations.load(Ordering::Relaxed),
            ),
            (mission_status, phase_status, 1),
            "{label}"
        );
        let events = event_types(&case.workspace_path().join("events.jsonl"))?;
        assert_eq!(
            &events[events.len() - expected_tail.len()..],
            expected_tail.as_slice(),
            "{label}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("phase.") && event.ends_with("ed"))
                .count(),
            if phase_status == PhaseStatus::Skipped {
                1
            } else {
                2
            },
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn warn_continue_is_rejected_before_executor_or_lifecycle_mutation() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-warn-refusal", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let events_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

    let result = runner.run_to_terminal(warn_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::UnsupportedVerificationPolicy)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, events_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn durable_completed_worker_still_requires_supported_verification() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-durable-completed-warn", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;
    let _outcome = runner
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;

    let result = runner.run_to_terminal(warn_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::UnsupportedVerificationPolicy)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn pending_phase_completion_with_warn_stays_fail_closed() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-pending-phase-completed-warn", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let _outcome = first
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    first.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    assert!(first.run_to_terminal(pass_decision()).is_err());
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(first);

    let mut recovered = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-pending-phase-completed-warn-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = recovered.run_to_terminal(warn_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::Lifecycle(
            LifecycleError::VerificationNotPassed
        ))
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn every_incomplete_attempt_uses_only_typed_mechanical_termination() -> TestResult {
    let terminations = [
        MechanicalTermination::Cancelled,
        MechanicalTermination::HardDeadlineExceeded,
        MechanicalTermination::WatchdogStalled,
        MechanicalTermination::ProcessExited(ProcessExitStatus::code(9)?),
        MechanicalTermination::ProviderStreamEnded,
        MechanicalTermination::SupervisorFailure,
        MechanicalTermination::EventDeliveryFailure,
        MechanicalTermination::ContractViolation,
    ];

    for (index, termination) in terminations.into_iter().enumerate() {
        let label = format!("terminal-mechanical-{index}");
        let state = one_phase_state()?;
        let mut case = fixture(&label, &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(IncompleteExecutor {
            invocations: Arc::clone(&invocations),
            termination,
        });
        let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

        let result = runner.run_to_terminal(pass_decision())?;
        let expected = if termination == MechanicalTermination::Cancelled {
            (MissionStatus::Cancelled, PhaseStatus::Skipped)
        } else {
            (MissionStatus::Failed, PhaseStatus::Failed)
        };
        assert_eq!(
            (result.mission_status(), result.phase_status()),
            expected,
            "{termination:?}"
        );
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        let events = event_types(&case.workspace_path().join("events.jsonl"))?;
        if termination == MechanicalTermination::Cancelled {
            assert_eq!(
                &events[events.len() - 2..],
                ["worker.failed", "mission.cancelled"],
                "{termination:?}"
            );
        } else {
            assert_eq!(
                &events[events.len() - 3..],
                ["worker.failed", "phase.failed", "mission.failed"],
                "{termination:?}"
            );
        }
    }
    Ok(())
}

#[cfg(feature = "test-support")]
#[test]
fn unresolved_owned_child_blocks_the_first_terminal_transition() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-unresolved-child", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        }))?,
        Duration::from_secs(30),
    )?
    .with_forced_unresolved_children_for_test();
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

    let result = runner.run_to_terminal(pass_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::Lifecycle(
            LifecycleError::UnresolvedChildren
        ))
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(
        event_types(&case.workspace_path().join("events.jsonl"))?,
        [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.output",
            "worker.completed",
        ]
    );
    runner.clear_forced_unresolved_children_for_test()?;

    let recovered = runner.run_to_terminal(warn_decision())?;
    assert_eq!(
        (
            recovered.disposition(),
            recovered.mission_status(),
            recovered.phase_status(),
            invocations.load(Ordering::Relaxed),
        ),
        (
            FixtureTerminalDisposition::Recovered,
            MissionStatus::Completed,
            PhaseStatus::Completed,
            1,
        )
    );
    Ok(())
}

#[test]
fn reopened_completed_worker_is_ambiguous_without_terminal_decision_authority() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-reopen-completed-worker", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let outcome = first
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    assert!(outcome.is_completed());
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(first);

    let mut recovered = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-reopen-completed-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = recovered.run_to_terminal(pass_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::AmbiguousDurableTerminalDecision)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn reopened_failed_worker_is_ambiguous_without_parsing_human_error_text() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-reopen-failed-worker", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(IncompleteExecutor {
        invocations: Arc::clone(&invocations),
        termination: MechanicalTermination::Cancelled,
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let _outcome = first
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(first);

    let mut recovered = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-reopen-failed-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = recovered.run_to_terminal(warn_decision());
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::AmbiguousDurableTerminalDecision)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn phase_terminal_fault_reopens_and_finishes_without_duplicate_events() -> TestResult {
    for (index, fault) in [
        FixtureProjectionFault::AfterEventSync,
        FixtureProjectionFault::AfterCheckpointRename,
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("terminal-phase-fault-{index}");
        let state = one_phase_state()?;
        let mut case = fixture(&label, &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
        let _outcome = first
            .run_attempt()?
            .into_executed()
            .ok_or("fresh attempt unexpectedly replayed")?;
        first.inject_fixture_projection_fault_once(fault);
        assert!(first.run_to_terminal(pass_decision()).is_err(), "{fault:?}");
        drop(first);

        let executable = format!("terminal-phase-fault-helper-{index}");
        let mut recovered = reopened_runner(
            &case,
            state,
            ExecutorRegistry::new(),
            &executable,
            "invalid runtime!",
            Duration::ZERO,
            true,
        )?;
        let result = recovered.run_to_terminal(pass_decision())?;
        assert_eq!(
            (
                result.disposition(),
                result.mission_status(),
                result.phase_status(),
                invocations.load(Ordering::Relaxed),
            ),
            (
                FixtureTerminalDisposition::Recovered,
                MissionStatus::Completed,
                PhaseStatus::Completed,
                1,
            ),
            "{fault:?}"
        );
        let events = event_types(&case.workspace_path().join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "phase.completed")
                .count(),
            1,
            "{fault:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "mission.completed")
                .count(),
            1,
            "{fault:?}"
        );
    }
    Ok(())
}

#[test]
fn durable_phase_failure_finishes_missing_mission_failure_with_warn_after_reopen() -> TestResult {
    for (index, fault) in [
        FixtureProjectionFault::AfterEventSync,
        FixtureProjectionFault::AfterCheckpointRename,
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("terminal-phase-failed-fault-{index}");
        let state = one_phase_state()?;
        let mut case = fixture(&label, &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
        let _outcome = first
            .run_attempt()?
            .into_executed()
            .ok_or("fresh attempt unexpectedly replayed")?;
        first.inject_fixture_projection_fault_once(fault);
        assert!(
            first.run_to_terminal(block_decision()).is_err(),
            "{fault:?}"
        );
        drop(first);

        let executable = format!("terminal-phase-failed-helper-{index}");
        let mut recovered = reopened_runner(
            &case,
            state,
            ExecutorRegistry::new(),
            &executable,
            "invalid runtime!",
            Duration::ZERO,
            true,
        )?;
        let result = recovered.run_to_terminal(warn_decision())?;
        assert_eq!(
            (
                result.disposition(),
                result.mission_status(),
                result.phase_status(),
                invocations.load(Ordering::Relaxed),
            ),
            (
                FixtureTerminalDisposition::Recovered,
                MissionStatus::Failed,
                PhaseStatus::Failed,
                1,
            ),
            "{fault:?}"
        );
        let events = event_types(&case.workspace_path().join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "phase.failed")
                .count(),
            1,
            "{fault:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "mission.failed")
                .count(),
            1,
            "{fault:?}"
        );
    }
    Ok(())
}

#[test]
fn pending_mission_cancellation_repairs_before_warn_policy() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-pending-mission-cancelled", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let _outcome = first
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    first.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    assert!(first.run_to_terminal(cancelled_decision()).is_err());
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(first);

    let mut recovered = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-pending-mission-cancelled-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = recovered.run_to_terminal(warn_decision())?;
    assert_eq!(
        (
            result.disposition(),
            result.mission_status(),
            result.phase_status(),
            invocations.load(Ordering::Relaxed),
        ),
        (
            FixtureTerminalDisposition::Recovered,
            MissionStatus::Cancelled,
            PhaseStatus::Skipped,
            1,
        )
    );
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn pending_mission_failure_repairs_before_warn_policy() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-pending-mission-failed", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let _outcome = first
        .run_attempt()?
        .into_executed()
        .ok_or("fresh attempt unexpectedly replayed")?;
    first.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterCheckpointRename);
    assert!(first.run_to_terminal(block_decision()).is_err());
    drop(first);

    let mut mission_fault = reopened_runner(
        &case,
        state.clone(),
        ExecutorRegistry::new(),
        "terminal-pending-mission-failed-stage-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    mission_fault.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    assert!(mission_fault.run_to_terminal(warn_decision()).is_err());
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(mission_fault);

    let mut recovered = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-pending-mission-failed-recover-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = recovered.run_to_terminal(warn_decision())?;
    assert_eq!(
        (
            result.disposition(),
            result.mission_status(),
            result.phase_status(),
            invocations.load(Ordering::Relaxed),
        ),
        (
            FixtureTerminalDisposition::Recovered,
            MissionStatus::Failed,
            PhaseStatus::Failed,
            1,
        )
    );
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn fully_terminal_replay_ignores_warn_bad_runtime_timeout_and_current_cancellation() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("terminal-closed-replay", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
    let _executed = first.run_to_terminal(pass_decision())?;
    let event_path = case.workspace_path().join("events.jsonl");
    let events_before = std::fs::read(&event_path)?;
    drop(first);

    let mut replay = reopened_runner(
        &case,
        state,
        ExecutorRegistry::new(),
        "terminal-closed-replay-helper",
        "invalid runtime!",
        Duration::ZERO,
        true,
    )?;
    let result = replay.run_to_terminal(warn_decision())?;
    assert_eq!(
        (
            result.disposition(),
            result.mission_status(),
            result.phase_status(),
            invocations.load(Ordering::Relaxed),
        ),
        (
            FixtureTerminalDisposition::Replayed,
            MissionStatus::Completed,
            PhaseStatus::Completed,
            1,
        )
    );
    assert_eq!(std::fs::read(event_path)?, events_before);
    Ok(())
}

#[test]
fn unrepresentable_deadline_does_not_start_or_mutate_the_attempt() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("deadline-preflight", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let event_log_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let mut runner = runner(&mut case, state, executor, Duration::MAX)?;

    let result = runner.run_attempt();
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::DeadlineOutOfRange)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, event_log_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn missing_runtime_for_fresh_work_fails_before_lifecycle_mutation() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("missing-runtime-preflight", &state)?;
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let event_log_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        ExecutorRegistry::new(),
        Duration::from_secs(30),
    )?;
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

    let result = runner.run_attempt();
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::RuntimeRegistry(
            RuntimeRegistryError::MissingDefault
        ))
    ));
    assert_eq!(read_optional(&event_path)?, event_log_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn zero_timeout_does_not_start_or_mutate_the_attempt() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("zero-timeout-preflight", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let event_log_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let mut runner = runner(&mut case, state, executor, Duration::ZERO)?;

    let result = runner.run_attempt();
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::ZeroTimeout)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, event_log_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn cancellation_before_admission_does_not_start_or_mutate_the_attempt() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("cancelled-preflight", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(SuccessfulExecutor {
        invocations: Arc::clone(&invocations),
    });
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let event_log_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    assert!(case.cancellation.cancel());
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

    let result = runner.run_attempt();
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::CancelledBeforeAdmission)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, event_log_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn process_failure_preserves_partial_output_and_releases_ownership() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("partial-failure", &state)?;
    let process = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        EXECUTABLE,
        &case.worker_root,
    )?
    .with_argument("nonzero")?;
    let executor = Arc::new(ProcessFailureExecutor { process });
    let mut runner = runner(&mut case, state, executor, Duration::from_secs(3))?;

    let outcome = runner
        .run_attempt()?
        .into_executed()
        .ok_or("failed fixture attempt unexpectedly replayed")?;
    assert_eq!(outcome.output(), Some("partial output survived"));
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ProcessExited(
            ProcessExitStatus::code(7)?
        ))
    );
    assert!(!outcome.failures().is_empty());
    assert!(!runner.has_unresolved_children()?);
    Ok(())
}

struct RawRuntime {
    executors: ExecutorRegistry,
}

impl OwnedChildState for RawRuntime {
    fn has_unresolved_children(&self) -> bool {
        false
    }
}

struct NoopServices {
    now: Instant,
    clock_reads: AtomicU64,
    expire_after_first_read: bool,
    process_error: ProcessServiceError,
    effect_error: EffectServiceError,
}

impl NoopServices {
    fn new() -> TestResult<Self> {
        Ok(Self {
            now: Instant::now(),
            clock_reads: AtomicU64::new(0),
            expire_after_first_read: false,
            process_error: ProcessServiceError::new(
                ProcessServiceErrorKind::Denied,
                "active recovery fixture does not admit processes",
            )?,
            effect_error: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "active recovery fixture does not admit effects",
            )?,
        })
    }

    fn expiring_after_context_creation() -> TestResult<Self> {
        Ok(Self {
            expire_after_first_read: true,
            ..Self::new()?
        })
    }
}

impl orchestrator_exec::Cancellation for NoopServices {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Clock for NoopServices {
    fn now(&self) -> Instant {
        if self.expire_after_first_read && self.clock_reads.fetch_add(1, Ordering::SeqCst) != 0 {
            self.now + Duration::from_secs(60)
        } else {
            self.now
        }
    }
}

impl WatchdogPolicy for NoopServices {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

impl ProcessService for NoopServices {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: orchestrator_exec::ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        if preflight.bind(self, request).is_some() {
            Ok(())
        } else {
            Err(self.process_error.clone())
        }
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        Err(self.process_error.clone())
    }
}

impl EffectService for NoopServices {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.effect_error.clone())
    }
}

#[test]
fn retained_mission_and_phase_admission_transitions_repair_before_dispatch() -> TestResult {
    for (label, fail_phase_start) in [
        ("repair-mission-start", false),
        ("repair-phase-start", true),
    ] {
        let state = one_phase_state()?;
        let mut case = fixture(label, &state)?;
        let raw_runtime = RawRuntime {
            executors: ExecutorRegistry::new(),
        };
        let mut coordinator =
            LifecycleCoordinator::new(case.take_workspace()?, raw_runtime, state.clone())?;
        if fail_phase_start {
            coordinator.transition_allocated(
                None,
                ReducerTransition::MissionStarted,
                Value::Null,
                None,
            )?;
        }
        coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
        let phase = fail_phase_start.then(|| PhaseId::new(PHASE)).transpose()?;
        let transition = if fail_phase_start {
            ReducerTransition::PhaseStarted
        } else {
            ReducerTransition::MissionStarted
        };
        let pending = coordinator.transition_allocated(phase, transition, Value::Null, None);
        assert!(pending.is_err());
        drop(coordinator);

        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let runtime = FixtureSequentialRuntime::new(
            case.take_process()?,
            registry(Arc::new(SuccessfulExecutor {
                invocations: Arc::clone(&invocations),
            }))?,
            Duration::from_secs(30),
        )?;
        let mut runner = FixtureSequentialRun::new(
            workspace,
            state,
            runtime,
            RUNTIME,
            request(&case.worker_root)?,
            WORKER,
            Duration::from_secs(3),
            false,
        )?;

        let outcome = runner
            .run_attempt()?
            .into_executed()
            .ok_or("repaired fixture admission unexpectedly replayed")?;
        assert!(outcome.is_completed());
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
    }
    Ok(())
}

#[test]
fn closed_mission_replays_without_a_valid_or_enrolled_current_runtime() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("closed-replay", &state)?;
    let raw_invocations = Arc::new(AtomicU64::new(0));
    let raw_runtime = RawRuntime {
        executors: registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&raw_invocations),
        }))?,
    };
    let mut coordinator =
        LifecycleCoordinator::new(case.take_workspace()?, raw_runtime, state.clone())?;
    coordinator.transition_allocated(None, ReducerTransition::MissionStarted, Value::Null, None)?;
    let phase = PhaseId::new(PHASE)?;
    coordinator.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseStarted,
        Value::Null,
        None,
    )?;
    let raw_request = request(&case.worker_root)?;
    let services = NoopServices::new()?;
    let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
    let attempt =
        coordinator.with_attempt(&phase, WORKER, 1, |runtime, sink| -> TestResult<_> {
            let executor = runtime.executors.resolve(RUNTIME)?;
            let mut context = ExecutionContext::new(
                &services,
                &services,
                &services,
                &services,
                sink,
                identity,
                Instant::now() + Duration::from_secs(3),
            );
            Ok(executor.execute(&raw_request, &mut context)?)
        })?;
    let outcome = attempt
        .into_executed()
        .ok_or("fresh raw attempt unexpectedly replayed")??;
    assert!(outcome.is_completed());
    assert_eq!(raw_invocations.load(Ordering::Relaxed), 1);

    let verification = decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    );
    coordinator.transition_allocated(
        Some(phase),
        ReducerTransition::PhaseCompleted,
        Value::Null,
        Some(verification),
    )?;
    coordinator.transition_allocated(
        None,
        ReducerTransition::MissionCompleted,
        Value::Null,
        None,
    )?;
    let event_path = case.workspace_path().join("events.jsonl");
    let terminal_log = std::fs::read(&event_path)?;
    let persisted_terminal = terminal_log
        .split(|byte| *byte == b'\n')
        .find(|line| {
            serde_json::from_slice::<Value>(line).is_ok_and(|event| {
                event.get("type").and_then(Value::as_str) == Some("worker.completed")
            })
        })
        .ok_or("durable worker terminal event is missing")?
        .to_vec();
    let checkpoint_path = case.workspace_path().join("checkpoint.json");
    let checkpoint_before = std::fs::read(&checkpoint_path)?;
    drop(coordinator);

    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        ExecutorRegistry::new(),
        Duration::from_secs(30),
    )?;
    let mut runner = FixtureSequentialRun::new(
        workspace,
        state,
        runtime,
        "invalid runtime!",
        request(&case.worker_root)?,
        WORKER,
        Duration::MAX,
        false,
    )?;

    let replay = runner.run_attempt()?;
    let replayed = replay
        .replayed()
        .ok_or("closed durable attempt unexpectedly executed")?;
    assert_eq!(
        replayed
            .terminal()
            .preserved_source_json()
            .ok_or("replayed terminal event lost its authoritative source JSON")?,
        persisted_terminal
    );
    assert_eq!(
        replayed.terminal().to_json()?.as_bytes(),
        persisted_terminal
    );
    assert_eq!(std::fs::read(event_path)?, terminal_log);
    assert_eq!(std::fs::read(checkpoint_path)?, checkpoint_before);
    Ok(())
}

#[test]
fn deadline_expiring_after_lifecycle_admission_is_terminal_and_replayable() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("post-admission-deadline", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let raw_runtime = RawRuntime {
        executors: registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        }))?,
    };
    let mut coordinator = LifecycleCoordinator::new(case.take_workspace()?, raw_runtime, state)?;
    coordinator.transition_allocated(None, ReducerTransition::MissionStarted, Value::Null, None)?;
    let phase = PhaseId::new(PHASE)?;
    coordinator.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseStarted,
        Value::Null,
        None,
    )?;
    let raw_request = request(&case.worker_root)?;
    let services = NoopServices::expiring_after_context_creation()?;
    let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
    let deadline = services.now + Duration::from_secs(3);

    let attempt =
        coordinator.with_attempt(&phase, WORKER, 1, |runtime, sink| -> TestResult<_> {
            let executor = runtime.executors.resolve(RUNTIME)?;
            let mut context = ExecutionContext::new(
                &services, &services, &services, &services, sink, identity, deadline,
            );
            Ok(executor.execute(&raw_request, &mut context)?)
        })?;
    let outcome = attempt
        .into_executed()
        .ok_or("post-admission deadline unexpectedly replayed")??;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::HardDeadlineExceeded)
    );
    assert_eq!(invocations.load(Ordering::SeqCst), 0);

    let replay = coordinator.with_attempt(&phase, WORKER, 1, |_, _| {
        invocations.fetch_add(1, Ordering::SeqCst);
    })?;
    assert!(replay.replayed().is_some());
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    let events = std::fs::read_to_string(case.workspace_path().join("events.jsonl"))?;
    assert!(events.contains("worker.spawned"));
    assert!(events.contains("worker.failed"));
    Ok(())
}

#[test]
fn durable_active_attempt_requires_explicit_recovery_instead_of_reexecution() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("active-recovery", &state)?;
    let workspace = case.take_workspace()?;
    let raw_invocations = Arc::new(AtomicU64::new(0));
    let raw_runtime = RawRuntime {
        executors: registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&raw_invocations),
        }))?,
    };
    let mut coordinator = LifecycleCoordinator::new(workspace, raw_runtime, state.clone())?;
    coordinator.transition_allocated(None, ReducerTransition::MissionStarted, Value::Null, None)?;
    let phase = PhaseId::new(PHASE)?;
    coordinator.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseStarted,
        Value::Null,
        None,
    )?;
    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    let raw_request = request(&case.worker_root)?;
    let services = NoopServices::new()?;
    let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
    let active = coordinator.with_attempt(&phase, WORKER, 1, |runtime, sink| -> TestResult<_> {
        let executor = runtime.executors.resolve(RUNTIME)?;
        let mut context = ExecutionContext::new(
            &services,
            &services,
            &services,
            &services,
            sink,
            identity,
            Instant::now() + Duration::from_secs(3),
        );
        Ok(executor.execute(&raw_request, &mut context)?)
    });
    assert!(matches!(active, Err(LifecycleError::RecoveryPending)));
    assert_eq!(raw_invocations.load(Ordering::Relaxed), 0);
    drop(coordinator);

    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        }))?,
        Duration::from_secs(30),
    )?;
    let mut runner = FixtureSequentialRun::new(
        workspace,
        state,
        runtime,
        RUNTIME,
        request(&case.worker_root)?,
        WORKER,
        Duration::from_secs(3),
        false,
    )?;

    let error = runner.run_attempt();
    assert!(
        matches!(
            &error,
            Err(FixtureSequentialRunError::Lifecycle(
                LifecycleError::AttemptRecoveryRequired
            ))
        ),
        "unexpected recovery result: {error:?}"
    );
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn composition_rejects_more_than_one_phase_before_dispatch() -> TestResult {
    let state = two_phase_state()?;
    let mut case = fixture("two-phase", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let events_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        }))?,
        Duration::from_secs(30),
    )?;
    let result = FixtureSequentialRun::new(
        case.take_workspace()?,
        state,
        runtime,
        RUNTIME,
        request(&case.worker_root)?,
        WORKER,
        Duration::from_secs(3),
        false,
    );
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::ExactlyOnePhaseRequired)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, events_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn composition_rejects_request_dependency_before_byte_mutation() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("one-phase-request-dependency", &state)?;
    let invocations = Arc::new(AtomicU64::new(0));
    let workspace = case.workspace_path();
    let event_path = workspace.join("events.jsonl");
    let events_before = read_optional(&event_path)?;
    let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
    let runtime = FixtureSequentialRuntime::new(
        case.take_process()?,
        registry(Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        }))?,
        Duration::from_secs(30),
    )?;
    let result = FixtureSequentialRun::new(
        case.take_workspace()?,
        state,
        runtime,
        RUNTIME,
        request_with_dependencies(&case.worker_root, vec!["phase-0".to_owned()])?,
        WORKER,
        Duration::from_secs(3),
        false,
    );
    assert!(matches!(
        result,
        Err(FixtureSequentialRunError::DependenciesUnsupported)
    ));
    assert_eq!(invocations.load(Ordering::Relaxed), 0);
    assert_eq!(read_optional(&event_path)?, events_before);
    assert_eq!(
        std::fs::read(workspace.join("checkpoint.json"))?,
        checkpoint_before
    );
    Ok(())
}

#[test]
fn timed_out_descendant_tree_is_reaped_before_attempt_returns() -> TestResult {
    let state = one_phase_state()?;
    let mut case = fixture("descendant-cleanup", &state)?;
    let process = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        EXECUTABLE,
        &case.worker_root,
    )?
    .with_argument("hang-tree")?;
    let executor = Arc::new(ProcessFailureExecutor { process });
    let mut runner = runner(&mut case, state, executor, Duration::from_millis(150))?;

    let outcome = runner
        .run_attempt()?
        .into_executed()
        .ok_or("timed-out fixture attempt unexpectedly replayed")?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::HardDeadlineExceeded)
    );
    assert_eq!(outcome.output(), Some("partial output survived"));
    assert!(!runner.has_unresolved_children()?);
    Ok(())
}
