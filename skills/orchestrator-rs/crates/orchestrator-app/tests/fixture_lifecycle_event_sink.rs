use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureAttemptRun, FixtureProjectionFault, FixtureWorkspaceSeed,
    FreshFixtureAuthority, IsolatedFixtureRoot, LifecycleCoordinator, LifecycleError,
    OwnedChildState, project_event,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, EventRecord, MissionId, MissionState,
    MissionStatus, PhaseDefinition, PhaseId, PhaseStatus, ReducerTransition, VerificationClass,
    VerificationMode, VerificationOutcome, decide_verification, decode_checkpoint,
    decode_event_line,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchRequest, EffectBudget,
    EffectReceipt, EffectRequest, EffectService, EffectServiceError, EffectServiceErrorKind,
    Effort, ExecutionContext, ExecutionRequest, ExecutionRequestDraft, ExecutorRegistry,
    MechanicalTermination, PartialWork, PhaseExecutor, ProcessBudget, ProcessReceipt,
    ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind, RuntimeCaps,
    RuntimeDescriptor, RuntimeFamily, WatchdogDecision, WatchdogPolicy, WorkerEventKind,
    WorkerEventPayload, WorkerIdentity, WorkerOutput, WorkerOutputFields, WorkerOutputKind,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "lifecycle-worker-events";
const PHASE: &str = "implement";
const RUNTIME: &str = "fixture-runtime";

static CASE: AtomicU64 = AtomicU64::new(1);

struct FixtureCase {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

struct TestRuntime {
    executors: ExecutorRegistry,
}

impl OwnedChildState for TestRuntime {
    fn has_unresolved_children(&self) -> bool {
        false
    }
}

struct StreamingExecutor {
    outputs: Vec<WorkerEventPayload>,
    probe: Arc<RuntimeProbe>,
}

#[derive(Default)]
struct RuntimeProbe {
    checkpoint_sequences: Mutex<Vec<i64>>,
    invocations: AtomicU64,
}

impl PhaseExecutor for StreamingExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        self.probe.invocations.fetch_add(1, Ordering::Relaxed);
        for output in &self.outputs {
            if let Ok(receipt) = context.emit(output) {
                if let Ok(mut observed) = self.probe.checkpoint_sequences.lock() {
                    observed.push(receipt.sequence());
                }
            }
        }
        AttemptOutcome::completed(
            "fixture attempt completed",
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
        Some(RuntimeDescriptor::new(
            RuntimeFamily::parse(RUNTIME).ok()?,
            RuntimeCaps {
                tool_use: false,
                session_resume: false,
                streaming: true,
                cost_report: false,
                artifacts: false,
            },
        ))
    }
}

struct NoopServices {
    now: Instant,
    process_error: ProcessServiceError,
    effect_error: EffectServiceError,
}

impl NoopServices {
    fn new() -> TestResult<Self> {
        Ok(Self {
            now: Instant::now(),
            process_error: ProcessServiceError::new(
                ProcessServiceErrorKind::Denied,
                "processes are outside this event-sink fixture",
            )?,
            effect_error: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "effects are outside this event-sink fixture",
            )?,
        })
    }
}

impl Cancellation for NoopServices {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Clock for NoopServices {
    fn now(&self) -> Instant {
        self.now
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

fn fixture_case(label: &str) -> TestResult<FixtureCase> {
    let number = CASE.fetch_add(1, Ordering::Relaxed);
    let temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temp.join(format!(
        "orchestrator-rs-worker-event-sink-{}-{number}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let policy = FixtureAdmissionPolicy::new(
        parent.join("live-user"),
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        &temp,
    );
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn initial_state() -> TestResult<(MissionState, PhaseId)> {
    let mission = MissionId::new(MISSION)?;
    let phase = PhaseId::new(PHASE)?;
    let state = MissionState::new(
        mission,
        vec![PhaseDefinition {
            id: phase.clone(),
            dependencies: Vec::new(),
        }],
    )?;
    Ok((state, phase))
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

fn workspace_root(case: &FixtureCase) -> PathBuf {
    case.root.join("workspaces").join(MISSION)
}

fn runtime_with_chunks(
    _workspace_root: &Path,
    chunks: &[&str],
) -> TestResult<(TestRuntime, Arc<RuntimeProbe>)> {
    let probe = Arc::new(RuntimeProbe::default());
    let outputs = chunks
        .iter()
        .copied()
        .map(|chunk| {
            WorkerOutput::new(WorkerOutputFields {
                chunk: Some(chunk.to_owned()),
                event_kind: Some(WorkerOutputKind::Text),
                streaming: Some(true),
                tool_name: None,
                is_error: None,
                output_len: Some(chunk.len()),
                duration: None,
            })
            .map(WorkerEventPayload::Output)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let executor = StreamingExecutor {
        outputs,
        probe: Arc::clone(&probe),
    };
    let mut executors = ExecutorRegistry::new();
    let _previous = executors.register(RUNTIME, Arc::new(executor))?;
    Ok((TestRuntime { executors }, probe))
}

fn runtime(workspace_root: &Path) -> TestResult<(TestRuntime, Arc<RuntimeProbe>)> {
    runtime_with_chunks(workspace_root, &["first chunk", "second chunk"])
}

fn coordinator(
    case: &FixtureCase,
) -> TestResult<(
    LifecycleCoordinator<TestRuntime>,
    PhaseId,
    Arc<RuntimeProbe>,
)> {
    let (state, phase) = initial_state()?;
    let workspace = case.authority.create_workspace(
        MissionId::new(MISSION)?,
        FixtureWorkspaceSeed::new(
            b"fixture\n".to_vec(),
            &initial_checkpoint(&state),
            b"{}".to_vec(),
        )?,
    )?;
    let (runtime, observed) = runtime(&workspace_root(case))?;
    Ok((
        LifecycleCoordinator::new(workspace, runtime, state)?,
        phase,
        observed,
    ))
}

fn reopen_coordinator(
    case: &FixtureCase,
    chunks: &[&str],
) -> TestResult<(
    LifecycleCoordinator<TestRuntime>,
    PhaseId,
    Arc<RuntimeProbe>,
)> {
    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let (initial, phase) = initial_state()?;
    let (runtime, observed) = runtime_with_chunks(&workspace_root(case), chunks)?;
    Ok((
        LifecycleCoordinator::new(workspace, runtime, initial)?,
        phase,
        observed,
    ))
}

fn start_phase(coordinator: &mut LifecycleCoordinator<TestRuntime>, phase: &PhaseId) -> TestResult {
    coordinator.transition_allocated(None, ReducerTransition::MissionStarted, Value::Null, None)?;
    coordinator.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseStarted,
        Value::Null,
        None,
    )?;
    Ok(())
}

fn request(mission: &str, phase: &str, attempt: u32) -> TestResult<ExecutionRequest> {
    Ok(ExecutionRequest::new(ExecutionRequestDraft {
        mission: mission.to_owned(),
        phase: phase.to_owned(),
        attempt,
        revision: 1,
        objective: "exercise the lifecycle worker-event bridge".to_owned(),
        persona: "fixture".to_owned(),
        role: "implementer".to_owned(),
        domain: "code".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec!["fixture-only".to_owned()],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(RUNTIME)?,
        model: "fixture-model".to_owned(),
        effort: Effort::High,
        max_turns: 4,
        worker_dir: "/fixture/worker".into(),
        target_dir: None,
        resume_from: None,
        hook_script: None,
    })?)
}

fn dispatch_attempt(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    identity_mission: &str,
    identity_phase: &str,
    worker: &str,
    attempt: u32,
) -> TestResult<AttemptOutcome> {
    dispatch_attempt_with_binding(
        coordinator,
        bound_phase,
        worker,
        identity_mission,
        identity_phase,
        worker,
        attempt,
    )
}

fn dispatch_attempt_run(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    identity_mission: &str,
    identity_phase: &str,
    worker: &str,
    attempt: u32,
) -> TestResult<FixtureAttemptRun<AttemptOutcome>> {
    dispatch_attempt_run_with_binding(
        coordinator,
        bound_phase,
        worker,
        identity_mission,
        identity_phase,
        worker,
        attempt,
    )
}

fn dispatch_attempt_with_binding(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    bound_worker: &str,
    identity_mission: &str,
    identity_phase: &str,
    identity_worker: &str,
    attempt: u32,
) -> TestResult<AttemptOutcome> {
    dispatch_attempt_run_with_binding(
        coordinator,
        bound_phase,
        bound_worker,
        identity_mission,
        identity_phase,
        identity_worker,
        attempt,
    )?
    .into_executed()
    .ok_or_else(|| "attempt unexpectedly replayed a durable terminal".into())
}

fn dispatch_attempt_run_with_binding(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    bound_worker: &str,
    identity_mission: &str,
    identity_phase: &str,
    identity_worker: &str,
    attempt: u32,
) -> TestResult<FixtureAttemptRun<AttemptOutcome>> {
    dispatch_attempt_run_with_ordinals(
        coordinator,
        bound_phase,
        bound_worker,
        identity_mission,
        identity_phase,
        identity_worker,
        attempt,
        attempt,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_attempt_run_with_ordinals(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    bound_worker: &str,
    identity_mission: &str,
    identity_phase: &str,
    identity_worker: &str,
    bound_attempt: u32,
    request_attempt: u32,
) -> TestResult<FixtureAttemptRun<AttemptOutcome>> {
    Ok(dispatch_attempt_run_with_ordinals_result(
        coordinator,
        bound_phase,
        bound_worker,
        identity_mission,
        identity_phase,
        identity_worker,
        bound_attempt,
        request_attempt,
    )??)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_attempt_run_with_ordinals_result(
    coordinator: &mut LifecycleCoordinator<TestRuntime>,
    bound_phase: &PhaseId,
    bound_worker: &str,
    identity_mission: &str,
    identity_phase: &str,
    identity_worker: &str,
    bound_attempt: u32,
    request_attempt: u32,
) -> TestResult<Result<FixtureAttemptRun<AttemptOutcome>, LifecycleError>> {
    let request = request(identity_mission, identity_phase, request_attempt)?;
    let identity = WorkerIdentity::new(identity_mission, identity_phase, identity_worker)?;
    let services = NoopServices::new()?;
    let deadline = services.now + Duration::from_secs(60);
    let run =
        coordinator.with_attempt(bound_phase, bound_worker, bound_attempt, |runtime, sink| {
            let resolved = runtime.executors.resolve(RUNTIME)?;
            let mut context = ExecutionContext::new(
                &services, &services, &services, &services, sink, identity, deadline,
            );
            Ok::<_, Box<dyn std::error::Error>>(resolved.execute(&request, &mut context)?)
        });
    match run {
        Ok(FixtureAttemptRun::Executed(result)) => Ok(Ok(FixtureAttemptRun::Executed(result?))),
        Ok(FixtureAttemptRun::Replayed(outcome)) => Ok(Ok(FixtureAttemptRun::Replayed(outcome))),
        Err(error) => Ok(Err(error)),
    }
}

fn event_records(workspace_root: &Path) -> TestResult<Vec<EventRecord>> {
    let bytes = std::fs::read(workspace_root.join("events.jsonl"))?;
    if !bytes.ends_with(b"\n") {
        return Err("event log is not newline terminated".into());
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(decode_event_line(line)?.record))
        .collect()
}

fn append_fixture_worker_event(
    workspace_root: &Path,
    event_type: &str,
    sequence: i64,
    phase: &str,
    worker: &str,
    attempt: u32,
) -> TestResult {
    let data = match event_type {
        "worker.spawned" => serde_json::json!({
            "model": "fixture-model",
            "runtime": RUNTIME,
            "effort_level": "high",
            "persona": "fixture",
            "dir": "/fixture/worker",
            "attempt": attempt,
        }),
        "worker.output" => serde_json::json!({
            "chunk": "late output",
            "event_kind": "text",
            "streaming": true,
            "output_len": 11,
            "attempt": attempt,
        }),
        _ => return Err("unsupported test worker event type".into()),
    };
    let mut line = serde_json::to_vec(&serde_json::json!({
        "id": format!("evt_fixture_{sequence:019}"),
        "type": event_type,
        "timestamp": format!("2000-01-01T00:00:00.{sequence:019}Z"),
        "sequence": sequence,
        "mission_id": MISSION,
        "phase_id": phase,
        "worker_id": worker,
        "data": data,
    }))?;
    line.push(b'\n');
    use std::os::unix::fs::OpenOptionsExt;
    let mut events = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(workspace_root.join("events.jsonl"))?;
    events.write_all(&line)?;
    events.sync_all()?;
    Ok(())
}

fn checkpoint_sequence(path: &Path) -> TestResult<i64> {
    decode_checkpoint(&std::fs::read(path)?)?
        .projection
        .extra
        .get("fixture_event_sequence")
        .and_then(Value::as_i64)
        .ok_or_else(|| "checkpoint is missing fixture_event_sequence".into())
}

fn assert_strict_metadata(records: &[EventRecord]) -> TestResult {
    for pair in records.windows(2) {
        if pair[0].sequence >= pair[1].sequence
            || pair[0].id >= pair[1].id
            || pair[0].timestamp >= pair[1].timestamp
        {
            return Err(format!(
                "event metadata is not strictly increasing between sequences {} and {}",
                pair[0].sequence, pair[1].sequence
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn dispatch_bridge_persists_canonical_monotonic_worker_stream() -> TestResult {
    let case = fixture_case("monotonic")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, observed) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;

    let outcome = dispatch_attempt(&mut coordinator, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(
        outcome.is_completed(),
        "valid worker dispatch was not completed"
    );

    let records = event_records(&root)?;
    assert_eq!(records.len(), 6);
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.output",
            "worker.output",
            "worker.completed",
        ]
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    assert_strict_metadata(&records)?;
    for record in &records[2..] {
        assert_eq!(record.mission_id, MISSION);
        assert_eq!(record.phase_id.as_deref(), Some(PHASE));
        assert_eq!(record.worker_id.as_deref(), Some("worker-1"));
        assert_eq!(
            record
                .data
                .as_ref()
                .and_then(|data| data.get("attempt"))
                .and_then(Value::as_u64),
            Some(1),
            "worker history did not persist its attempt ordinal"
        );
    }
    assert_eq!(
        *observed
            .checkpoint_sequences
            .lock()
            .map_err(|_| "observation mutex poisoned")?,
        [4, 5],
        "checkpoint did not advance after each provider output"
    );
    assert_eq!(checkpoint_sequence(&root.join("checkpoint.json"))?, 6);
    assert_eq!(observed.invocations.load(Ordering::Relaxed), 1);
    let queried = coordinator
        .durable_attempt_replay(&phase, "worker-1", 1)?
        .ok_or("durable attempt query omitted the terminal record")?;
    assert_eq!(queried.terminal().kind(), WorkerEventKind::Completed);
    let committed_log = std::fs::read(root.join("events.jsonl"))?;
    let replay = dispatch_attempt_run(&mut coordinator, &phase, MISSION, PHASE, "worker-1", 1)?;
    let durable = replay
        .replayed()
        .ok_or("terminal attempt was executed instead of replayed")?;
    assert_eq!(durable.attempt(), 1);
    assert_eq!(durable.terminal().kind(), WorkerEventKind::Completed);
    assert_eq!(durable.terminal().receipt().sequence(), 6);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, committed_log);
    assert_eq!(
        *observed
            .checkpoint_sequences
            .lock()
            .map_err(|_| "observation mutex poisoned")?,
        [4, 5],
        "terminal replay invoked the provider"
    );
    assert_eq!(observed.invocations.load(Ordering::Relaxed), 1);
    drop(coordinator);
    let (mut reopened, phase, replay_probe) = reopen_coordinator(&case, &["different chunk"])?;
    let reopened_replay =
        dispatch_attempt_run(&mut reopened, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(reopened_replay.replayed().is_some());
    assert_eq!(replay_probe.invocations.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, committed_log);

    let verification = decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    );
    reopened.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseCompleted,
        Value::Null,
        Some(verification),
    )?;
    reopened.transition_allocated(None, ReducerTransition::MissionCompleted, Value::Null, None)?;
    let closed_query = reopened
        .durable_attempt_replay(&phase, "worker-1", 1)?
        .ok_or("closed mission omitted its durable attempt replay")?;
    assert_eq!(closed_query.terminal().kind(), WorkerEventKind::Completed);
    let terminal_log = std::fs::read(root.join("events.jsonl"))?;
    let terminal_replay =
        dispatch_attempt_run(&mut reopened, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(terminal_replay.replayed().is_some());
    assert_eq!(replay_probe.invocations.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, terminal_log);
    drop(reopened);

    let (mut completed, phase, completed_probe) = reopen_coordinator(&case, &["never execute"])?;
    let completed_replay =
        dispatch_attempt_run(&mut completed, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(completed_replay.replayed().is_some());
    assert_eq!(completed_probe.invocations.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, terminal_log);
    Ok(())
}

#[test]
fn pending_admission_transitions_repair_from_the_exact_retained_input() -> TestResult {
    let case = fixture_case("admission-repair")?;
    let (mut coordinator, phase, _) = coordinator(&case)?;

    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    let mission_start = coordinator.transition_allocated(
        None,
        ReducerTransition::MissionStarted,
        Value::Null,
        None,
    );
    assert!(mission_start.is_err());
    assert!(matches!(
        coordinator.registry(),
        Err(LifecycleError::RecoveryPending)
    ));
    assert!(coordinator.repair_pending_admission_transition()?);
    assert_eq!(coordinator.state().status(), MissionStatus::InProgress);
    assert!(!coordinator.repair_pending_admission_transition()?);

    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    let phase_start = coordinator.transition_allocated(
        Some(phase.clone()),
        ReducerTransition::PhaseStarted,
        Value::Null,
        None,
    );
    assert!(phase_start.is_err());
    assert!(coordinator.repair_pending_admission_transition()?);
    assert_eq!(
        coordinator.state().phase(&phase).map(|phase| phase.status),
        Some(PhaseStatus::Running)
    );
    assert!(coordinator.registry().is_ok());
    Ok(())
}

#[test]
fn pending_admission_recovery_is_exact_across_restart_boundaries() -> TestResult {
    for (transition, fault) in [
        (
            ReducerTransition::MissionStarted,
            FixtureProjectionFault::AfterEventSync,
        ),
        (
            ReducerTransition::MissionStarted,
            FixtureProjectionFault::AfterCheckpointRename,
        ),
        (
            ReducerTransition::PhaseStarted,
            FixtureProjectionFault::AfterEventSync,
        ),
        (
            ReducerTransition::PhaseStarted,
            FixtureProjectionFault::AfterCheckpointRename,
        ),
    ] {
        let label = format!("admission-restart-{transition:?}-{fault:?}");
        let case = fixture_case(&label)?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _) = coordinator(&case)?;
        if matches!(transition, ReducerTransition::PhaseStarted) {
            coordinator.transition_allocated(
                None,
                ReducerTransition::MissionStarted,
                Value::Null,
                None,
            )?;
        }
        coordinator.inject_fixture_projection_fault_once(fault);
        let result = coordinator.transition_allocated(
            matches!(transition, ReducerTransition::PhaseStarted).then(|| phase.clone()),
            transition.clone(),
            Value::Null,
            None,
        );
        assert!(result.is_err(), "{label}: injected transition did not fail");

        let faulted_log = std::fs::read(root.join("events.jsonl"))?;
        let faulted_records = event_records(&root)?;
        let pending = faulted_records
            .last()
            .ok_or("faulted admission event is missing")?;
        let pending_id = pending.id.clone();
        let pending_sequence = pending.sequence;
        drop(coordinator);

        let (mut reopened, recovered_phase, _) = reopen_coordinator(&case, &[])?;
        let repaired = reopened.repair_pending_admission_transition()?;
        assert_eq!(
            repaired,
            fault == FixtureProjectionFault::AfterEventSync,
            "{label}: repair classification did not match the durable boundary"
        );
        assert_eq!(
            std::fs::read(root.join("events.jsonl"))?,
            faulted_log,
            "{label}: recovery appended the retained admission event"
        );
        assert_eq!(
            reopened.state().status(),
            MissionStatus::InProgress,
            "{label}: mission start was not recovered"
        );

        if matches!(transition, ReducerTransition::MissionStarted) {
            reopened.transition_allocated(
                Some(recovered_phase),
                ReducerTransition::PhaseStarted,
                Value::Null,
                None,
            )?;
        } else {
            let outcome = dispatch_attempt(
                &mut reopened,
                &recovered_phase,
                MISSION,
                PHASE,
                "worker-after-admission-repair",
                1,
            )?;
            assert!(outcome.is_completed());
        }

        let final_records = event_records(&root)?;
        assert_eq!(
            final_records[faulted_records.len()].sequence,
            pending_sequence + 1,
            "{label}: the first post-recovery event was not contiguous"
        );
        assert_eq!(
            final_records
                .iter()
                .filter(|record| record.id == pending_id)
                .count(),
            1,
            "{label}: retained admission event was duplicated"
        );
        assert_strict_metadata(&final_records)?;
    }
    Ok(())
}

#[test]
fn with_attempt_admits_only_running_bound_identity() -> TestResult {
    let case = fixture_case("admission")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, observed) = coordinator(&case)?;
    let mut closure_ran = false;
    let pending = coordinator.with_attempt(&phase, "worker-pending", 1, |_, _| closure_ran = true);
    assert!(matches!(
        pending,
        Err(LifecycleError::AttemptPhaseNotRunning)
    ));
    assert!(!closure_ran, "pending-phase attempt closure was invoked");

    start_phase(&mut coordinator, &phase)?;
    let mut empty_worker_closure_ran = false;
    let empty_worker = coordinator.with_attempt(&phase, "", 1, |_, _| {
        empty_worker_closure_ran = true;
    });
    assert!(matches!(
        empty_worker,
        Err(LifecycleError::AttemptWorkerMismatch)
    ));
    assert!(!empty_worker_closure_ran);
    let mut zero_attempt_closure_ran = false;
    let zero_attempt = coordinator.with_attempt(&phase, "worker-zero", 0, |_, _| {
        zero_attempt_closure_ran = true;
    });
    assert!(matches!(zero_attempt, Err(LifecycleError::InvalidAttempt)));
    assert!(!zero_attempt_closure_ran);
    let no_terminal = coordinator.with_attempt(&phase, "worker-no-terminal", 1, |_, _| ());
    assert!(matches!(
        no_terminal,
        Err(LifecycleError::AttemptTerminalMissing)
    ));
    let admitted_log = std::fs::read(root.join("events.jsonl"))?;
    let wrong_mission = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-wrong-mission",
        "another-mission",
        PHASE,
        "worker-wrong-mission",
        1,
        1,
    )?;
    assert!(matches!(
        wrong_mission,
        Err(LifecycleError::AttemptTerminalMissing)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, admitted_log);

    let wrong_phase = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-wrong-phase",
        MISSION,
        "another-phase",
        "worker-wrong-phase",
        2,
        2,
    )?;
    assert!(matches!(
        wrong_phase,
        Err(LifecycleError::AttemptTerminalMissing)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, admitted_log);

    let wrong_worker = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-bound",
        MISSION,
        PHASE,
        "worker-unbound",
        3,
        3,
    )?;
    assert!(matches!(
        wrong_worker,
        Err(LifecycleError::AttemptTerminalMissing)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, admitted_log);

    let request_attempt_mismatch = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-attempt",
        MISSION,
        PHASE,
        "worker-attempt",
        4,
        5,
    )?;
    assert!(matches!(
        request_attempt_mismatch,
        Err(LifecycleError::AttemptTerminalMissing)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, admitted_log);
    assert_eq!(
        observed.invocations.load(Ordering::Relaxed),
        0,
        "attempt mismatch invoked the provider"
    );
    Ok(())
}

#[test]
fn reopen_reconstructs_exact_worker_stream_and_separates_next_attempt() -> TestResult {
    let case = fixture_case("reopen")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _observed) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    let first = dispatch_attempt(&mut coordinator, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(first.is_completed());
    let state_before = coordinator.state().clone();
    let log_before = std::fs::read(root.join("events.jsonl"))?;
    drop(coordinator);

    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let (initial, recovered_phase) = initial_state()?;
    let (runtime, _observed) = runtime(&root)?;
    let mut recovered = LifecycleCoordinator::new(workspace, runtime, initial)?;
    assert_eq!(recovered.state(), &state_before);
    assert_eq!(
        recovered
            .state()
            .phase(&recovered_phase)
            .ok_or("recovered phase is missing")?
            .status,
        PhaseStatus::Running
    );
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, log_before);

    let second = dispatch_attempt(
        &mut recovered,
        &recovered_phase,
        MISSION,
        PHASE,
        "worker-1",
        2,
    )?;
    assert!(second.is_completed());
    let records = event_records(&root)?;
    assert_eq!(records.len(), 10);
    assert_eq!(
        records[6..]
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        [7, 8, 9, 10]
    );
    assert!(
        records[6..]
            .iter()
            .all(|record| record.worker_id.as_deref() == Some("worker-1"))
    );
    assert!(
        records[6..].iter().all(|record| {
            record
                .data
                .as_ref()
                .and_then(|data| data.get("attempt"))
                .and_then(Value::as_u64)
                == Some(2)
        }),
        "attempt 2 reused attempt 1 history"
    );
    assert_strict_metadata(&records)?;
    assert_eq!(checkpoint_sequence(&root.join("checkpoint.json"))?, 10);
    let skipped_first = dispatch_attempt(
        &mut recovered,
        &recovered_phase,
        MISSION,
        PHASE,
        "worker-starting-at-two",
        2,
    )?;
    assert!(skipped_first.is_completed());
    assert!(matches!(
        recovered.durable_attempt_replay(&recovered_phase, "worker-starting-at-two", 1),
        Err(LifecycleError::AttemptOrdinalRegression)
    ));
    assert!(
        recovered
            .durable_attempt_replay(&recovered_phase, "worker-starting-at-two", 3)?
            .is_none(),
        "the next higher attempt was not reported as new work"
    );
    Ok(())
}

#[test]
fn durable_attempt_query_rejects_legacy_history_without_an_ordinal() -> TestResult {
    let case = fixture_case("legacy-attempt-query")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    let outcome = dispatch_attempt(&mut coordinator, &phase, MISSION, PHASE, "legacy-worker", 1)?;
    assert!(outcome.is_completed());
    drop(coordinator);

    let event_path = root.join("events.jsonl");
    let mut legacy_log = Vec::new();
    for line in std::fs::read(&event_path)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut event: Value = serde_json::from_slice(line)?;
        if event
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("worker."))
        {
            event
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .ok_or("worker event data is not an object")?
                .remove("attempt");
        }
        legacy_log.extend_from_slice(&serde_json::to_vec(&event)?);
        legacy_log.push(b'\n');
    }
    std::fs::write(&event_path, legacy_log)?;

    let (reopened, recovered_phase, _) = reopen_coordinator(&case, &[])?;
    assert!(matches!(
        reopened.durable_attempt_replay(&recovered_phase, "legacy-worker", 1),
        Err(LifecycleError::AmbiguousLegacyAttempt)
    ));
    Ok(())
}

#[test]
fn reopen_preserves_unknown_worker_fields_and_legacy_event_id_as_source_bytes() -> TestResult {
    let case = fixture_case("forward-worker-fields")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _observed) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    let first = dispatch_attempt(&mut coordinator, &phase, MISSION, PHASE, "worker-1", 1)?;
    assert!(first.is_completed());
    drop(coordinator);

    let event_path = root.join("events.jsonl");
    let existing = std::fs::read(&event_path)?;
    let mut lines = existing
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(Vec::from)
        .collect::<Vec<_>>();
    let terminal = lines.last_mut().ok_or("worker terminal event is missing")?;
    let mut value: Value = serde_json::from_slice(terminal)?;
    let envelope = value
        .as_object_mut()
        .ok_or("worker terminal envelope is not an object")?;
    envelope.insert(
        "future_envelope".to_owned(),
        serde_json::json!({"trace": "preserve"}),
    );
    envelope.insert("id".to_owned(), Value::String("legacy.event:1".to_owned()));
    value
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .ok_or("worker terminal data is not an object")?
        .insert("future_data".to_owned(), serde_json::json!([1, 2, 3]));
    let authoritative_terminal = serde_json::to_vec(&value)?;
    *terminal = authoritative_terminal.clone();
    let mut rewritten = Vec::new();
    for line in &lines {
        rewritten.extend_from_slice(line);
        rewritten.push(b'\n');
    }
    std::fs::write(&event_path, rewritten)?;

    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let (initial, recovered_phase) = initial_state()?;
    let (runtime, _observed) = runtime(&root)?;
    let mut recovered = LifecycleCoordinator::new(workspace, runtime, initial)?;
    let replay = dispatch_attempt_run(
        &mut recovered,
        &recovered_phase,
        MISSION,
        PHASE,
        "worker-1",
        1,
    )?;
    let terminal = replay
        .replayed()
        .ok_or("durable attempt was unexpectedly executed")?
        .terminal();
    assert_eq!(
        terminal.preserved_source_json(),
        Some(authoritative_terminal.as_slice())
    );
    assert_eq!(terminal.receipt().id(), "legacy.event:1");
    assert_eq!(
        terminal.to_json()?.as_bytes(),
        authoritative_terminal.as_slice()
    );
    Ok(())
}

#[test]
fn event_sync_fault_recovery_is_exact_for_first_and_later_worker_events() -> TestResult {
    for (label, with_prior_stream) in [("first-worker", false), ("later-worker", true)] {
        let case = fixture_case(label)?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _observed) = coordinator(&case)?;
        start_phase(&mut coordinator, &phase)?;
        if with_prior_stream {
            let prior = dispatch_attempt(&mut coordinator, &phase, MISSION, PHASE, "worker-1", 1)?;
            assert!(prior.is_completed());
        }
        drop(coordinator);

        let (mut faulting, phase, _observed) = reopen_coordinator(&case, &[])?;
        let records_before = event_records(&root)?;
        let checkpoint_before = std::fs::read(root.join("checkpoint.json"))?;
        faulting.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
        let faulted_attempt = if with_prior_stream { 2 } else { 1 };
        let faulted = dispatch_attempt_run_with_ordinals_result(
            &mut faulting,
            &phase,
            "worker-faulted",
            MISSION,
            PHASE,
            "worker-faulted",
            faulted_attempt,
            faulted_attempt,
        )?;
        assert!(matches!(faulted, Err(LifecycleError::RecoveryPending)));
        assert!(matches!(
            faulting.registry(),
            Err(LifecycleError::RecoveryPending)
        ));
        let pending_log = std::fs::read(root.join("events.jsonl"))?;
        let pending_records = event_records(&root)?;
        assert_eq!(pending_records.len(), records_before.len() + 1);
        let pending = pending_records
            .last()
            .ok_or("faulted worker event is missing")?;
        assert_eq!(pending.event_type, "worker.spawned");
        assert_eq!(pending.worker_id.as_deref(), Some("worker-faulted"));
        assert_eq!(
            std::fs::read(root.join("checkpoint.json"))?,
            checkpoint_before
        );
        let pending_sequence = pending.sequence;
        let pending_id = pending.id.clone();
        let pending_line = pending_log
            .split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty())
            .ok_or("faulted worker event line is missing")?;
        let pending_input = project_event(&decode_event_line(pending_line)?)?;
        assert!(matches!(
            faulting.transition(&pending_input, None),
            Err(LifecycleError::RecoveryPending)
        ));
        assert_eq!(std::fs::read(root.join("events.jsonl"))?, pending_log);
        assert_eq!(
            std::fs::read(root.join("checkpoint.json"))?,
            checkpoint_before
        );
        assert!(matches!(
            faulting.registry(),
            Err(LifecycleError::RecoveryPending)
        ));
        drop(faulting);

        let (repaired, _phase, _observed) =
            reopen_coordinator(&case, &["first chunk", "second chunk"])?;
        assert_eq!(std::fs::read(root.join("events.jsonl"))?, pending_log);
        assert_eq!(
            checkpoint_sequence(&root.join("checkpoint.json"))?,
            pending_sequence
        );
        assert_eq!(
            repaired.state().greatest_applied_sequence(),
            Some(pending_sequence)
        );
        let repaired_checkpoint = std::fs::read(root.join("checkpoint.json"))?;
        drop(repaired);

        let (mut reopened, phase, _observed) =
            reopen_coordinator(&case, &["first chunk", "second chunk"])?;
        assert_eq!(std::fs::read(root.join("events.jsonl"))?, pending_log);
        assert_eq!(
            std::fs::read(root.join("checkpoint.json"))?,
            repaired_checkpoint,
            "a fully repaired worker event changed on the next reopen"
        );
        let next_attempt = faulted_attempt + 1;
        let next = dispatch_attempt(
            &mut reopened,
            &phase,
            MISSION,
            PHASE,
            "worker-after-recovery",
            next_attempt,
        )?;
        assert!(next.is_completed());
        let final_records = event_records(&root)?;
        assert_eq!(
            final_records[records_before.len() + 1].sequence,
            pending_sequence + 1,
            "the first post-recovery allocation was not contiguous"
        );
        assert_eq!(
            final_records
                .iter()
                .filter(|record| record.id == pending_id)
                .count(),
            1,
            "pending worker event was appended twice"
        );
        let unique_ids = final_records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(unique_ids.len(), final_records.len());
        assert_strict_metadata(&final_records)?;
    }
    Ok(())
}

#[test]
fn post_publication_worker_faults_are_indeterminate_before_provider_invocation() -> TestResult {
    for (label, fault) in [
        (
            "worker-post-publish-conflict",
            FixtureProjectionFault::AfterCheckpointRenameConflict,
        ),
        (
            "worker-post-publish-identity",
            FixtureProjectionFault::AfterCheckpointRenameIdentityChange,
        ),
    ] {
        let case = fixture_case(label)?;
        let (mut coordinator, phase, probe) = coordinator(&case)?;
        start_phase(&mut coordinator, &phase)?;
        coordinator.inject_fixture_projection_fault_once(fault);

        let result = dispatch_attempt_run_with_ordinals_result(
            &mut coordinator,
            &phase,
            "worker-indeterminate",
            MISSION,
            PHASE,
            "worker-indeterminate",
            1,
            1,
        )?;
        assert!(matches!(result, Err(LifecycleError::RecoveryPending)));
        assert!(matches!(
            coordinator.registry(),
            Err(LifecycleError::RecoveryPending)
        ));
        assert_eq!(
            probe.invocations.load(Ordering::Relaxed),
            0,
            "{label} invoked the provider after an indeterminate spawn event"
        );
    }
    Ok(())
}

#[test]
fn reopened_active_attempt_requires_explicit_continuation_without_invoking_provider() -> TestResult
{
    let case = fixture_case("active-continuation")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _probe) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    let faulted = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-resume",
        MISSION,
        PHASE,
        "worker-resume",
        1,
        1,
    )?;
    assert!(matches!(faulted, Err(LifecycleError::RecoveryPending)));
    drop(coordinator);

    let (mut reopened, phase, probe) = reopen_coordinator(&case, &["first chunk", "second chunk"])?;
    let repaired_log = std::fs::read(root.join("events.jsonl"))?;
    let verification = decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    );
    assert!(matches!(
        reopened.transition_allocated(
            Some(phase.clone()),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            Some(verification),
        ),
        Err(LifecycleError::ActiveWorkerAttempt)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, repaired_log);
    assert!(matches!(
        reopened.durable_attempt_replay(&phase, "worker-resume", 1),
        Err(LifecycleError::AttemptRecoveryRequired)
    ));
    let mut closure_ran = false;
    let continued = reopened.with_attempt(&phase, "worker-resume", 1, |_, _| {
        closure_ran = true;
    });
    assert!(matches!(
        continued,
        Err(LifecycleError::AttemptRecoveryRequired)
    ));
    assert!(!closure_ran);
    assert_eq!(probe.invocations.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, repaired_log);
    Ok(())
}

#[test]
fn reopened_active_attempt_blocks_a_new_attempt_until_recovery() -> TestResult {
    let case = fixture_case("incomplete-prefix")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _probe) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    let faulted = dispatch_attempt_run_with_ordinals_result(
        &mut coordinator,
        &phase,
        "worker-incomplete",
        MISSION,
        PHASE,
        "worker-incomplete",
        1,
        1,
    )?;
    assert!(matches!(faulted, Err(LifecycleError::RecoveryPending)));
    drop(coordinator);

    let (mut reopened, phase, _probe) = reopen_coordinator(&case, &[])?;
    let repaired_log = std::fs::read(root.join("events.jsonl"))?;
    let mut closure_ran = false;
    let result = reopened.with_attempt(&phase, "worker-incomplete", 2, |_, _| {
        closure_ran = true;
    });
    assert!(matches!(
        result,
        Err(LifecycleError::AttemptRecoveryRequired)
    ));
    assert!(!closure_ran);
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, repaired_log);
    Ok(())
}

#[test]
fn constructor_rejects_a_gap_in_the_durable_event_sequence() -> TestResult {
    let case = fixture_case("sequence-gap")?;
    let root = workspace_root(&case);
    let (mut coordinator, phase, _probe) = coordinator(&case)?;
    start_phase(&mut coordinator, &phase)?;
    drop(coordinator);
    append_fixture_worker_event(&root, "worker.spawned", 4, PHASE, "worker-gap", 1)?;

    let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
    let (initial, _phase) = initial_state()?;
    let (runtime, _probe) = runtime(&root)?;
    assert!(matches!(
        LifecycleCoordinator::new(workspace, runtime, initial),
        Err(LifecycleError::RecoveryConflict)
    ));
    Ok(())
}

#[test]
fn constructor_rejects_workers_before_start_or_in_an_unknown_phase() -> TestResult {
    for (label, start_known_phase, worker_phase, sequence) in [
        ("worker-before-start", false, PHASE, 1),
        ("worker-unknown-phase", true, "unknown-phase", 3),
    ] {
        let case = fixture_case(label)?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _probe) = coordinator(&case)?;
        if start_known_phase {
            start_phase(&mut coordinator, &phase)?;
        }
        drop(coordinator);
        append_fixture_worker_event(
            &root,
            "worker.spawned",
            sequence,
            worker_phase,
            "worker-corrupt",
            1,
        )?;

        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let (initial, _phase) = initial_state()?;
        let (runtime, _probe) = runtime(&root)?;
        assert!(matches!(
            LifecycleCoordinator::new(workspace, runtime, initial),
            Err(LifecycleError::RecoveryConflict)
        ));
    }
    Ok(())
}

#[test]
fn constructor_rejects_workers_after_phase_or_attempt_terminal() -> TestResult {
    {
        let case = fixture_case("worker-after-phase-terminal")?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _probe) = coordinator(&case)?;
        start_phase(&mut coordinator, &phase)?;
        let verification = decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        );
        coordinator.transition_allocated(
            Some(phase.clone()),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            Some(verification),
        )?;
        drop(coordinator);
        append_fixture_worker_event(&root, "worker.spawned", 4, PHASE, "worker-too-late", 1)?;

        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let (initial, _phase) = initial_state()?;
        let (runtime, _probe) = runtime(&root)?;
        assert!(matches!(
            LifecycleCoordinator::new(workspace, runtime, initial),
            Err(LifecycleError::RecoveryConflict)
        ));
    }

    {
        let case = fixture_case("worker-after-attempt-terminal")?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _probe) = coordinator(&case)?;
        start_phase(&mut coordinator, &phase)?;
        let completed = dispatch_attempt(
            &mut coordinator,
            &phase,
            MISSION,
            PHASE,
            "worker-terminal",
            1,
        )?;
        assert!(completed.is_completed());
        drop(coordinator);
        append_fixture_worker_event(&root, "worker.output", 7, PHASE, "worker-terminal", 1)?;

        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let (initial, _phase) = initial_state()?;
        let (runtime, _probe) = runtime(&root)?;
        assert!(matches!(
            LifecycleCoordinator::new(workspace, runtime, initial),
            Err(LifecycleError::RecoveryConflict)
        ));
    }
    Ok(())
}

#[test]
fn constructor_rejects_every_corrupt_or_oversized_event_log_diagnostic() -> TestResult {
    let mut oversized = vec![b'x'; 1024 * 1024 + 1];
    oversized.push(b'\n');
    for (label, invalid_line) in [
        ("corrupt-line", b"{not-json}\n".to_vec()),
        ("oversized-line", oversized),
    ] {
        let case = fixture_case(label)?;
        let root = workspace_root(&case);
        let (mut coordinator, phase, _observed) = coordinator(&case)?;
        start_phase(&mut coordinator, &phase)?;
        drop(coordinator);

        let mut events = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("events.jsonl"))?;
        events.write_all(&invalid_line)?;
        events.sync_all()?;
        drop(events);

        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let (initial, _phase) = initial_state()?;
        let (runtime, _observed) = runtime(&root)?;
        assert!(matches!(
            LifecycleCoordinator::new(workspace, runtime, initial),
            Err(LifecycleError::RecoveryConflict)
        ));
    }
    Ok(())
}
