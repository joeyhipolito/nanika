use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES;
use orchestrator_exec::{
    ArtifactReceipt, AttemptEvidence, AttemptOutcome, Cancellation, Clock, ContractError,
    DispatchError, DispatchRequest, EffectBudget, EffectKind, EffectReceipt, EffectRequest,
    EffectService, EffectServiceError, EffectServiceErrorKind, EffectStatus, Effort, EventReceipt,
    EventSink, EventSinkError, EventSinkErrorKind, EvidenceVerification,
    EvidenceVerificationRequest, EvidenceVerifier, EvidenceVerifierError,
    EvidenceVerifierErrorKind, ExecutionContext, ExecutionRequest, ExecutionRequestDraft,
    ExecutorRegistry, Failure, FailureKind, MechanicalTermination, PartialWork, PhaseExecutor,
    ProcessBudget, ProcessExitStatus, ProcessPreflight, ProcessPreflightReason, ProcessPurpose,
    ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind,
    ProcessTerminationReceipt, ResolutionKind, RuntimeCap, RuntimeCaps, RuntimeDescriptor,
    RuntimeFamily, SessionHandle, ToolObservation, WatchdogDecision, WatchdogPolicy,
    WorkerCompleted, WorkerEventCodecError, WorkerEventDraft, WorkerEventEnvelope,
    WorkerEventError, WorkerEventKind, WorkerEventPayload, WorkerFailed, WorkerFailedFields,
    WorkerIdentity, WorkerOutput, WorkerOutputFields, WorkerOutputKind, WorkerSpawned,
    WorkerSpawnedFields,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Harness {
    now: Instant,
    cancelled: Arc<AtomicBool>,
    cancel_during_process: bool,
    cancel_after_effect_admission: bool,
    process_result: Result<ProcessReceipt, ProcessServiceError>,
    invalid_preflight: ProcessServiceError,
    effect_result: Result<EffectReceipt, EffectServiceError>,
    process_calls: AtomicUsize,
    process_preflight_calls: AtomicUsize,
    process_preflight_wrong_request_rejections: AtomicUsize,
    effect_calls: AtomicUsize,
    budgets: Mutex<Vec<ProcessBudget>>,
    preflight_reasons: Mutex<Vec<ProcessPreflightReason>>,
    alternate_process_request: ProcessRequest,
}

impl Harness {
    fn success(now: Instant) -> TestResult<Self> {
        Ok(Self {
            now,
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_during_process: false,
            cancel_after_effect_admission: false,
            process_result: Ok(ProcessReceipt::new(
                ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
                b"stdout".to_vec(),
                Vec::new(),
                0,
                0,
                true,
                Duration::from_millis(10),
            )?),
            invalid_preflight: ProcessServiceError::new(
                ProcessServiceErrorKind::InvalidRequest,
                "process preflight did not match its exact request",
            )?,
            effect_result: Ok(EffectReceipt::new(
                "operation-1",
                "effect-key",
                EffectStatus::Applied,
                None,
            )?),
            process_calls: AtomicUsize::new(0),
            process_preflight_calls: AtomicUsize::new(0),
            process_preflight_wrong_request_rejections: AtomicUsize::new(0),
            effect_calls: AtomicUsize::new(0),
            budgets: Mutex::new(Vec::new()),
            preflight_reasons: Mutex::new(Vec::new()),
            alternate_process_request: ProcessRequest::new(
                ProcessPurpose::ProviderWorker,
                "alternate-provider",
                "/fixture/worker",
            )?,
        })
    }

    fn with_process_error(now: Instant) -> TestResult<Self> {
        Self::with_process_error_kind(now, ProcessServiceErrorKind::Denied)
    }

    fn with_process_error_kind(now: Instant, kind: ProcessServiceErrorKind) -> TestResult<Self> {
        let mut harness = Self::success(now)?;
        harness.process_result = Err(ProcessServiceError::new(kind, "process service failure")?);
        Ok(harness)
    }

    fn with_process_termination(
        now: Instant,
        termination: ProcessTerminationReceipt,
    ) -> TestResult<Self> {
        Self::with_process_receipt(
            now,
            ProcessReceipt::new(
                termination,
                Vec::new(),
                Vec::new(),
                0,
                0,
                true,
                Duration::from_millis(10),
            )?,
        )
    }

    fn with_process_receipt(now: Instant, receipt: ProcessReceipt) -> TestResult<Self> {
        let mut harness = Self::success(now)?;
        harness.process_result = Ok(receipt);
        Ok(harness)
    }

    fn with_effect_error(now: Instant) -> TestResult<Self> {
        let mut harness = Self::success(now)?;
        harness.effect_result = Err(EffectServiceError::new(
            EffectServiceErrorKind::Execution,
            "effect failed",
        )?);
        Ok(harness)
    }

    fn with_mismatched_effect_receipt(now: Instant) -> TestResult<Self> {
        let mut harness = Self::success(now)?;
        harness.effect_result = Ok(EffectReceipt::new(
            "operation-1",
            "different-key",
            EffectStatus::Applied,
            None,
        )?);
        Ok(harness)
    }
}

impl Cancellation for Harness {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Clock for Harness {
    fn now(&self) -> Instant {
        self.now
    }
}

impl WatchdogPolicy for Harness {
    fn evaluate(&self, now: Instant, last_activity: Instant) -> WatchdogDecision {
        if now.saturating_duration_since(last_activity) >= Duration::from_secs(30) {
            WatchdogDecision::Stalled
        } else {
            WatchdogDecision::Continue {
                next_check: last_activity + Duration::from_secs(30),
            }
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

impl ProcessService for Harness {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        let Some(preflight) = preflight.bind(self, request) else {
            return Err(self.invalid_preflight.clone());
        };
        if !preflight.matches_request(&self.alternate_process_request) {
            self.process_preflight_wrong_request_rejections
                .fetch_add(1, Ordering::SeqCst);
        }
        self.process_preflight_calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut reasons) = self.preflight_reasons.lock() {
            reasons.push(preflight.reason());
        }
        Ok(())
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        self.process_calls.fetch_add(1, Ordering::SeqCst);
        if self.cancel_during_process {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        if let Ok(mut budgets) = self.budgets.lock() {
            budgets.push(budget);
        }
        self.process_result.clone()
    }
}

impl EffectService for Harness {
    fn execute(
        &self,
        _request: &EffectRequest,
        budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        self.effect_calls.fetch_add(1, Ordering::SeqCst);
        let _admission = budget.admit()?;
        if self.cancel_after_effect_admission {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        self.effect_result.clone()
    }
}

enum FixtureVerification {
    Pass,
    Fail(EvidenceVerifierError),
}

struct FixtureVerifier {
    behavior: FixtureVerification,
    construction_error: EvidenceVerifierError,
    calls: AtomicUsize,
}

impl FixtureVerifier {
    fn passing() -> TestResult<Self> {
        Ok(Self {
            behavior: FixtureVerification::Pass,
            construction_error: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Unavailable,
                "fixture verifier could not construct bounded evidence",
            )?,
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(kind: EvidenceVerifierErrorKind, detail: &str) -> TestResult<Self> {
        Ok(Self {
            behavior: FixtureVerification::Fail(EvidenceVerifierError::new(kind, detail)?),
            construction_error: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Unavailable,
                "fixture verifier could not construct bounded evidence",
            )?,
            calls: AtomicUsize::new(0),
        })
    }
}

impl EvidenceVerifier for FixtureVerifier {
    fn verify(
        &self,
        request: &EvidenceVerificationRequest<'_>,
    ) -> Result<EvidenceVerification, EvidenceVerifierError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.behavior {
            FixtureVerification::Pass => {
                EvidenceVerification::new(request.claimed_artifacts().to_vec())
                    .map_err(|_| self.construction_error.clone())
            }
            FixtureVerification::Fail(error) => Err(error.clone()),
        }
    }
}

struct RecordingSink {
    next_sequence: i64,
    receipt_error: EventSinkError,
    identities: Vec<(String, String, String)>,
    attempts: Vec<u32>,
    kinds: Vec<WorkerEventKind>,
    sequences: Vec<i64>,
}

impl RecordingSink {
    fn new() -> Result<Self, WorkerEventError> {
        Ok(Self {
            next_sequence: 1,
            receipt_error: EventSinkError::new(
                EventSinkErrorKind::Unavailable,
                "fixture event receipt allocator is exhausted",
            )?,
            identities: Vec::new(),
            attempts: Vec::new(),
            kinds: Vec::new(),
            sequences: Vec::new(),
        })
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        self.identities.push((
            event.identity().mission_id().to_owned(),
            event.identity().phase_id().to_owned(),
            event.identity().worker_id().to_owned(),
        ));
        self.attempts.push(event.attempt());
        self.kinds.push(event.kind());
        let sequence = self.next_sequence;
        let receipt = EventReceipt::new(
            format!("evt_fixture_{sequence:016x}"),
            format!("2026-07-15T00:00:00.{sequence:09}Z"),
            sequence,
        )
        .map_err(|_| self.receipt_error.clone())?;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| self.receipt_error.clone())?;
        self.sequences.push(sequence);
        Ok(receipt)
    }
}

struct FailingSink(EventSinkError);

impl EventSink for FailingSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        Err(self.0.clone())
    }
}

struct TriggeringSink {
    inner: RecordingSink,
    trigger_after_spawn: Arc<AtomicBool>,
}

impl TriggeringSink {
    fn new(trigger_after_spawn: Arc<AtomicBool>) -> TestResult<Self> {
        Ok(Self {
            inner: RecordingSink::new()?,
            trigger_after_spawn,
        })
    }
}

impl EventSink for TriggeringSink {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let receipt = self.inner.emit(event)?;
        if event.kind() == WorkerEventKind::Spawned {
            self.trigger_after_spawn.store(true, Ordering::SeqCst);
        }
        Ok(receipt)
    }
}

struct TriggeredClock {
    before: Instant,
    after: Instant,
    triggered: Arc<AtomicBool>,
}

impl Clock for TriggeredClock {
    fn now(&self) -> Instant {
        if self.triggered.load(Ordering::SeqCst) {
            self.after
        } else {
            self.before
        }
    }
}

struct FailOnceSink {
    inner: RecordingSink,
    first_failure: Option<EventSinkError>,
}

impl FailOnceSink {
    fn new(error: EventSinkError) -> TestResult<Self> {
        Ok(Self {
            inner: RecordingSink::new()?,
            first_failure: Some(error),
        })
    }
}

impl EventSink for FailOnceSink {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        if let Some(error) = self.first_failure.take() {
            return Err(error);
        }
        self.inner.emit(event)
    }
}

#[derive(Clone)]
enum Behavior {
    Complete,
    CompleteEvidence(AttemptEvidence),
    CompleteAllEvidence,
    Emit(WorkerEventPayload),
    RunProcess,
    RunProcessPurpose(ProcessPurpose, bool),
    RunProcessWithCap(ProcessPurpose, usize, bool),
    RunEffect,
    ManyEffects(usize),
    PanicWithProgress,
    ReturnIncompleteAfterProgress,
    RunProcessIncomplete,
}

struct MockExecutor {
    runtime: RuntimeFamily,
    caps: Option<RuntimeCaps>,
    behavior: Behavior,
    descriptor_calls: Option<Arc<AtomicUsize>>,
}

impl PhaseExecutor for MockExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        match &self.behavior {
            Behavior::Complete => completed(AttemptEvidence::new()),
            Behavior::CompleteEvidence(evidence) => completed(evidence.clone()),
            Behavior::CompleteAllEvidence => {
                let evidence = all_evidence(&self.runtime).unwrap_or_default();
                completed(evidence)
            }
            Behavior::Emit(payload) => {
                let _ = context.emit(payload);
                completed(AttemptEvidence::new())
            }
            Behavior::RunProcess => {
                if let Ok(request) = ProcessRequest::new(
                    ProcessPurpose::ProviderWorker,
                    "provider",
                    "/fixture/worker",
                ) {
                    let _ = context.run_process(&request);
                }
                completed(AttemptEvidence::new())
            }
            Behavior::RunProcessPurpose(purpose, acknowledge_truncation) => {
                if let Ok(request) =
                    ProcessRequest::new(*purpose, "fixture-process", "/fixture/worker")
                {
                    let request = if *acknowledge_truncation {
                        request.with_truncated_output_acknowledged()
                    } else {
                        request
                    };
                    let _ = context.run_process(&request);
                }
                completed(AttemptEvidence::new())
            }
            Behavior::RunProcessWithCap(purpose, cap, acknowledge_truncation) => {
                if let Ok(request) =
                    ProcessRequest::new(*purpose, "fixture-process", "/fixture/worker")
                        .and_then(|request| request.with_max_output_bytes(*cap))
                {
                    let request = if *acknowledge_truncation {
                        request.with_truncated_output_acknowledged()
                    } else {
                        request
                    };
                    let _ = context.run_process(&request);
                }
                completed(AttemptEvidence::new())
            }
            Behavior::RunEffect => {
                if let Ok(request) =
                    EffectRequest::new(EffectKind::ArtifactWrite, "output.md", "effect-key")
                {
                    let _ = context.run_effect(&request);
                }
                completed(AttemptEvidence::new())
            }
            Behavior::ManyEffects(count) => {
                for index in 0..*count {
                    if let Ok(request) = EffectRequest::new(
                        EffectKind::PluginAction,
                        "plugin",
                        format!("effect-{index}"),
                    ) {
                        let _ = context.run_effect(&request);
                    }
                }
                completed(AttemptEvidence::new())
            }
            Behavior::PanicWithProgress => {
                let _ = context.record_partial_output("accumulated before unwind");
                if let Ok(session) =
                    SessionHandle::new(self.runtime.clone(), "session-before-unwind")
                {
                    context.record_session(session);
                }
                if let Ok(tool) = ToolObservation::new("bash", Some(0), "observed before unwind") {
                    let _ = context.record_tool_observation(tool);
                }
                std::panic::resume_unwind(Box::new("provider panic payload"));
            }
            Behavior::ReturnIncompleteAfterProgress => {
                let _ = context.record_partial_output("context-owned partial output");
                if let Ok(session) =
                    SessionHandle::new(self.runtime.clone(), "context-owned-session")
                {
                    context.record_session(session);
                }
                AttemptOutcome::incomplete(
                    MechanicalTermination::ProviderStreamEnded,
                    None,
                    PartialWork::empty(),
                    Duration::from_secs(1),
                )
            }
            Behavior::RunProcessIncomplete => {
                if let Ok(request) = ProcessRequest::new(
                    ProcessPurpose::ProviderWorker,
                    "provider",
                    "/fixture/worker",
                ) {
                    let _ = context.run_process(&request);
                }
                let termination = ProcessExitStatus::code(1).map_or(
                    MechanicalTermination::ContractViolation,
                    MechanicalTermination::ProcessExited,
                );
                AttemptOutcome::incomplete(
                    termination,
                    None,
                    PartialWork::empty(),
                    Duration::from_secs(1),
                )
            }
        }
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        if let Some(calls) = self.descriptor_calls.as_ref() {
            calls.fetch_add(1, Ordering::SeqCst);
        }
        self.caps
            .map(|caps| RuntimeDescriptor::new(self.runtime.clone(), caps))
    }
}

struct CountingExecutor {
    runtime: RuntimeFamily,
    calls: Arc<AtomicUsize>,
}

impl PhaseExecutor for CountingExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        _context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        completed(AttemptEvidence::new())
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(self.runtime.clone(), all_caps()))
    }
}

fn completed(evidence: AttemptEvidence) -> AttemptOutcome {
    match AttemptOutcome::completed("finished", evidence, Duration::from_secs(1)) {
        Ok(outcome) => outcome,
        Err(_) => AttemptOutcome::incomplete(
            MechanicalTermination::ContractViolation,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        ),
    }
}

fn family(value: &str) -> Result<RuntimeFamily, ContractError> {
    RuntimeFamily::parse(value)
}

fn request(
    runtime: RuntimeFamily,
    resume_from: Option<SessionHandle>,
) -> Result<ExecutionRequest, ContractError> {
    request_with_expected(runtime, resume_from, Vec::new())
}

fn request_with_expected(
    runtime: RuntimeFamily,
    resume_from: Option<SessionHandle>,
    expected_evidence: Vec<String>,
) -> Result<ExecutionRequest, ContractError> {
    ExecutionRequest::new(request_draft(runtime, resume_from, expected_evidence))
}

fn request_draft(
    runtime: RuntimeFamily,
    resume_from: Option<SessionHandle>,
    expected_evidence: Vec<String>,
) -> ExecutionRequestDraft {
    ExecutionRequestDraft {
        mission: "mission-1".to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective: "implement the contract".to_owned(),
        persona: "rust-engineer".to_owned(),
        role: "implementer".to_owned(),
        domain: "code".to_owned(),
        skills: vec!["rust-best-practices".to_owned()],
        dependencies: vec!["phase-0".to_owned()],
        expected_evidence,
        constraints: vec!["fixture-only".to_owned()],
        prior_context: "prior evidence".to_owned(),
        runtime,
        model: "model-alias".to_owned(),
        effort: Effort::High,
        max_turns: 20,
        worker_dir: "/fixture/worker".into(),
        target_dir: Some("/fixture/target".into()),
        resume_from,
        hook_script: None,
    }
}

fn identity() -> Result<WorkerIdentity, WorkerEventError> {
    WorkerIdentity::new("mission-1", "phase-1", "worker-1")
}

fn all_caps() -> RuntimeCaps {
    RuntimeCaps {
        tool_use: true,
        session_resume: true,
        streaming: true,
        cost_report: true,
        artifacts: true,
    }
}

fn all_evidence(runtime: &RuntimeFamily) -> Result<AttemptEvidence, ContractError> {
    let session = SessionHandle::new(runtime.clone(), "continuation-secret")?;
    let cost = orchestrator_exec::CostInfo::new(10, 5, 0.125, 2, 3)?;
    let tool = ToolObservation::new("bash", Some(1), "tool secret")?;
    let artifact = ArtifactReceipt::new("outputs/output.md", "digest-secret", 42)?;
    AttemptEvidence::new()
        .with_session(session)
        .with_cost(cost)
        .with_tool_observation(tool)?
        .with_artifact_receipt(artifact)
}

fn registry(
    runtime: &RuntimeFamily,
    caps: Option<RuntimeCaps>,
    behavior: Behavior,
) -> TestResult<ExecutorRegistry> {
    let mut registry = ExecutorRegistry::new();
    let _ = registry.register(
        runtime.as_str(),
        Arc::new(MockExecutor {
            runtime: runtime.clone(),
            caps,
            behavior,
            descriptor_calls: None,
        }),
    )?;
    Ok(registry)
}

fn counting_registry(
    runtime: &RuntimeFamily,
    calls: Arc<AtomicUsize>,
) -> TestResult<ExecutorRegistry> {
    let mut registry = ExecutorRegistry::new();
    let _ = registry.register(
        runtime.as_str(),
        Arc::new(CountingExecutor {
            runtime: runtime.clone(),
            calls,
        }),
    )?;
    Ok(registry)
}

#[test]
fn operation_services_receive_exact_attempt_budget_and_return_receipts() -> TestResult {
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let deadline = now + Duration::from_secs(60);
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        deadline,
    );
    let process = ProcessRequest::new(ProcessPurpose::ProviderWorker, "codex", "/fixture/worker")?
        .with_argument("exec")?
        .with_environment("HOME", "/fixture/home")?;
    let process_receipt = context.run_process(&process)?;
    assert!(process_receipt.is_success());
    let effect = EffectRequest::new(EffectKind::ArtifactWrite, "output.md", "effect-key")?;
    let effect_receipt = context.run_effect(&effect)?;
    assert_eq!(effect_receipt.status(), EffectStatus::Applied);
    assert_eq!(harness.process_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.effect_calls.load(Ordering::SeqCst), 1);
    let budgets = harness
        .budgets
        .lock()
        .map_err(|_| "budget mutex poisoned")?;
    assert_eq!(budgets.len(), 1);
    assert_eq!(budgets[0].hard_deadline(), deadline);
    assert_eq!(budgets[0].remaining(), Duration::from_secs(60));
    assert_eq!(budgets[0].stall_window(), Duration::from_secs(30));
    Ok(())
}

#[test]
fn ignored_process_denial_is_terminal_and_preserves_provider_output() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_error(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    assert_eq!(outcome.output(), Some("finished"));
    assert_eq!(outcome.failures().len(), 1);
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Permission);
    Ok(())
}

#[test]
fn indeterminate_process_outcome_is_terminal_infrastructure_failure() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness =
        Harness::with_process_error_kind(now, ProcessServiceErrorKind::OutcomeIndeterminate)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );

    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;

    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Infrastructure);
    Ok(())
}

#[test]
fn ignored_process_cancellation_receipt_is_terminal() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_termination(now, ProcessTerminationReceipt::Cancelled)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(outcome.output(), Some("finished"));
    drop(context);
    assert_eq!(sink.kinds.last(), Some(&WorkerEventKind::Failed));
    assert!(!sink.kinds.contains(&WorkerEventKind::Completed));
    Ok(())
}

#[test]
fn ignored_process_stall_is_authoritative_and_preserves_provider_output() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_termination(now, ProcessTerminationReceipt::Stalled)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::WatchdogStalled)
    );
    assert_eq!(outcome.output(), Some("finished"));
    Ok(())
}

#[test]
fn cancellation_observed_during_a_stalled_process_keeps_precedence() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let mut harness = Harness::with_process_termination(now, ProcessTerminationReceipt::Stalled)?;
    harness.cancel_during_process = true;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(outcome.output(), Some("finished"));
    Ok(())
}

#[test]
fn cancelled_and_expired_contexts_do_not_start_processes_or_effects() -> TestResult {
    let now = Instant::now();
    let harness = Harness::success(now)?;
    harness.cancelled.store(true, Ordering::SeqCst);
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let process = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "provider",
        "/fixture/worker",
    )?;
    let effect = EffectRequest::new(EffectKind::ArtifactWrite, "output.md", "effect-key")?;
    assert_eq!(
        context.run_process(&process)?.termination(),
        ProcessTerminationReceipt::Cancelled
    );
    assert!(
        context
            .run_effect(&effect)
            .is_err_and(|error| error.kind() == EffectServiceErrorKind::Cancelled)
    );
    drop(context);
    assert_eq!(harness.process_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.process_preflight_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.effect_calls.load(Ordering::SeqCst), 0);

    harness.cancelled.store(false, Ordering::SeqCst);
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now,
    );
    assert_eq!(
        context.run_process(&process)?.termination(),
        ProcessTerminationReceipt::DeadlineExceeded
    );
    assert!(
        context
            .run_effect(&effect)
            .is_err_and(|error| error.kind() == EffectServiceErrorKind::DeadlineExceeded)
    );
    assert_eq!(harness.process_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.process_preflight_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        harness
            .process_preflight_wrong_request_rejections
            .load(Ordering::SeqCst),
        2
    );
    assert_eq!(harness.effect_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        *harness
            .preflight_reasons
            .lock()
            .map_err(|_| "preflight-reason mutex poisoned")?,
        vec![
            ProcessPreflightReason::Cancelled,
            ProcessPreflightReason::DeadlineExceeded,
        ]
    );
    Ok(())
}

#[test]
fn cancelled_preflight_closes_worker_stream_without_provider_invocation() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let request = request(runtime, None)?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    harness.cancelled.store(true, Ordering::SeqCst);
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );

    let outcome = resolved.execute(&request, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        resolved.execute(&request, &mut context),
        Err(DispatchError::ContextAlreadyUsed)
    ));
    drop(context);
    assert_eq!(
        sink.kinds,
        [WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn expired_preflight_closes_worker_stream_without_provider_invocation() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let request = request(runtime, None)?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now,
    );

    let outcome = resolved.execute(&request, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::HardDeadlineExceeded)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        resolved.execute(&request, &mut context),
        Err(DispatchError::ContextAlreadyUsed)
    ));
    drop(context);
    assert_eq!(
        sink.kinds,
        [WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn cancellation_after_spawn_closes_worker_stream_without_provider_invocation() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let request = request(runtime, None)?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = TriggeringSink::new(Arc::clone(&harness.cancelled))?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );

    let outcome = resolved.execute(&request, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    drop(context);
    assert_eq!(
        sink.inner.kinds,
        [WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn deadline_after_spawn_closes_worker_stream_without_provider_invocation() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let request = request(runtime, None)?;
    let before = Instant::now();
    let deadline = before + Duration::from_secs(30);
    let triggered = Arc::new(AtomicBool::new(false));
    let clock = TriggeredClock {
        before,
        after: deadline + Duration::from_nanos(1),
        triggered: Arc::clone(&triggered),
    };
    let harness = Harness::success(before)?;
    let mut sink = TriggeringSink::new(triggered)?;
    let mut context = ExecutionContext::new(
        &harness,
        &clock,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        deadline,
    );

    let outcome = resolved.execute(&request, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::HardDeadlineExceeded)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    drop(context);
    assert_eq!(
        sink.inner.kinds,
        [WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn post_service_cancellation_prevents_provider_success() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let mut harness = Harness::success(now)?;
    harness.cancel_during_process = true;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(outcome.output(), Some("finished"));
    Ok(())
}

#[test]
fn authoritative_termination_updates_provider_incomplete_state() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcessIncomplete)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_error(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Permission);
    Ok(())
}

#[test]
fn context_progress_reconciles_into_non_panicking_incomplete_outcome() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(
        &runtime,
        Some(all_caps()),
        Behavior::ReturnIncompleteAfterProgress,
    )?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(outcome.output(), Some("context-owned partial output"));
    assert!(outcome.evidence().session().is_some());
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ProviderStreamEnded)
    );
    Ok(())
}

#[test]
fn effect_requires_matching_execution_receipt() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunEffect)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_mismatched_effect_receipt(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Protocol);
    Ok(())
}

#[test]
fn context_owns_event_identity_and_sink_assigns_receipt() -> TestResult {
    let payload = WorkerEventPayload::Output(WorkerOutput::new(WorkerOutputFields {
        chunk: Some("hello".to_owned()),
        event_kind: Some(WorkerOutputKind::Text),
        streaming: Some(true),
        tool_name: None,
        is_error: None,
        output_len: None,
        duration: None,
    })?);
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::Emit(payload))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    assert!(
        resolved
            .execute(&request(runtime, None)?, &mut context)?
            .is_completed()
    );
    drop(context);
    assert_eq!(
        sink.kinds,
        vec![
            WorkerEventKind::Spawned,
            WorkerEventKind::Output,
            WorkerEventKind::Completed
        ]
    );
    assert_eq!(
        sink.identities,
        vec![
            (
                "mission-1".to_owned(),
                "phase-1".to_owned(),
                "worker-1".to_owned()
            );
            3
        ]
    );
    assert_eq!(sink.sequences, [1, 2, 3]);
    assert_eq!(sink.attempts, [1, 1, 1]);
    Ok(())
}

#[test]
fn mismatched_context_identity_is_rejected_before_provider_runs() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::Complete)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let wrong = WorkerIdentity::new("another-mission", "phase-1", "worker-1")?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        wrong,
        now + Duration::from_secs(60),
    );
    assert!(matches!(
        resolved.execute(&request(runtime, None)?, &mut context),
        Err(DispatchError::WorkerIdentityMismatch)
    ));
    Ok(())
}

#[test]
fn persistent_spawn_failure_is_authoritative_and_prevents_provider_work() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = FailingSink(EventSinkError::new(
        EventSinkErrorKind::Persistence,
        "database unavailable",
    )?);
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::EventDeliveryFailure)
    );
    assert_eq!(outcome.output(), None);
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Infrastructure);
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn transient_spawn_failure_is_closed_terminally_without_provider_work() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = FailOnceSink::new(EventSinkError::new(
        EventSinkErrorKind::Persistence,
        "first publication unavailable",
    )?)?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );

    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::EventDeliveryFailure)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    drop(context);
    assert_eq!(
        sink.inner.kinds,
        [WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn indeterminate_spawn_failure_is_not_retried_or_dispatched() -> TestResult {
    let runtime = family("claude")?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let registry = counting_registry(&runtime, Arc::clone(&provider_calls))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = FailOnceSink::new(EventSinkError::new(
        EventSinkErrorKind::Indeterminate,
        "spawn publication requires reconciliation",
    )?)?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );

    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::EventDeliveryFailure)
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    drop(context);
    assert!(sink.inner.kinds.is_empty());
    assert!(sink.first_failure.is_none());
    Ok(())
}

#[test]
fn provider_unwind_becomes_outcome_and_preserves_context_progress() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::PanicWithProgress)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    assert_eq!(outcome.output(), Some("accumulated before unwind"));
    assert!(outcome.evidence().session().is_some());
    assert_eq!(outcome.evidence().tool_observations().len(), 1);
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Infrastructure);
    assert!(
        !outcome.failures()[0]
            .expose_detail()
            .contains("provider panic payload")
    );
    Ok(())
}

#[test]
fn known_runtime_caps_are_enforced_but_evidence_is_preserved() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(
        &runtime,
        Some(RuntimeCaps::default()),
        Behavior::CompleteAllEvidence,
    )?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let verifier = FixtureVerifier::passing()?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert!(outcome.evidence().session().is_none());
    assert!(outcome.evidence().cost().is_some());
    assert_eq!(outcome.evidence().tool_observations().len(), 1);
    assert_eq!(outcome.evidence().artifact_receipts().len(), 1);
    assert_eq!(outcome.failures().len(), 4);
    Ok(())
}

#[test]
fn known_caps_reject_resume_before_attempt_and_unknown_caps_preserve_go_behavior() -> TestResult {
    let runtime = family("claude")?;
    let session = SessionHandle::new(runtime.clone(), "resume-secret")?;
    let known = registry(&runtime, Some(RuntimeCaps::default()), Behavior::Complete)?;
    let known_resolved = known.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    assert!(matches!(
        known_resolved.execute(
            &request(runtime.clone(), Some(session.clone()))?,
            &mut context
        ),
        Err(DispatchError::UnsupportedCapability(
            RuntimeCap::SessionResume
        ))
    ));

    let unknown = registry(&runtime, None, Behavior::CompleteAllEvidence)?;
    let unknown_resolved = unknown.resolve("claude")?;
    let verifier = FixtureVerifier::passing()?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let outcome = unknown_resolved.execute(&request(runtime, Some(session))?, &mut context)?;
    assert!(outcome.is_completed());
    assert!(outcome.evidence().session().is_some());
    Ok(())
}

#[test]
fn missing_expected_evidence_rejects_completion_and_preserves_unverified_claims() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::CompleteAllEvidence)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let verifier = FixtureVerifier::failing(
        EvidenceVerifierErrorKind::Missing,
        "expected artifact is absent",
    )?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let outcome = resolved.execute(
        &request_with_expected(runtime, None, vec!["outputs/output.md".to_owned()])?,
        &mut context,
    )?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Verification);
    assert_eq!(outcome.evidence().artifact_receipts().len(), 1);
    assert!(
        !outcome.evidence().artifact_receipts()[0].is_authority_verified(),
        "a rejected provider claim must remain explicitly unverified"
    );
    Ok(())
}

#[test]
fn mismatched_artifact_claim_cannot_qualify_completion() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::CompleteAllEvidence)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let verifier = FixtureVerifier::failing(
        EvidenceVerifierErrorKind::Mismatch,
        "artifact digest differs from authority observation",
    )?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert!(!outcome.is_completed());
    assert_eq!(outcome.failures()[0].kind(), FailureKind::Verification);
    assert!(!outcome.evidence().artifact_receipts()[0].is_authority_verified());
    Ok(())
}

#[test]
fn independent_verifier_qualifies_artifacts_before_completion() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::CompleteAllEvidence)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let verifier = FixtureVerifier::passing()?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert!(outcome.is_completed());
    assert!(outcome.evidence().artifact_receipts()[0].is_authority_verified());
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn provider_artifact_claim_without_authority_rejects_completion() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::CompleteAllEvidence)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert!(!outcome.evidence().artifact_receipts()[0].is_authority_verified());
    Ok(())
}

#[test]
fn previously_verified_receipt_is_demoted_when_replayed_by_provider() -> TestResult {
    let runtime = family("claude")?;
    let first_registry = registry(&runtime, Some(all_caps()), Behavior::CompleteAllEvidence)?;
    let first_resolved = first_registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let verifier = FixtureVerifier::passing()?;
    let mut first_sink = RecordingSink::new()?;
    let mut first_context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut first_sink,
        identity()?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&verifier);
    let first = first_resolved.execute(&request(runtime.clone(), None)?, &mut first_context)?;
    assert!(first.evidence().artifact_receipts()[0].is_authority_verified());

    let replayed = first.evidence().clone();
    let second_registry = registry(
        &runtime,
        Some(all_caps()),
        Behavior::CompleteEvidence(replayed),
    )?;
    let second_resolved = second_registry.resolve("claude")?;
    let mut second_sink = RecordingSink::new()?;
    let mut second_context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut second_sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let second = second_resolved.execute(&request(runtime, None)?, &mut second_context)?;
    assert!(!second.is_completed());
    assert!(!second.evidence().artifact_receipts()[0].is_authority_verified());
    Ok(())
}

#[test]
fn cancellation_after_effect_admission_preserves_committed_receipt() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunEffect)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let mut harness = Harness::success(now)?;
    harness.cancel_after_effect_admission = true;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::Cancelled)
    );
    assert_eq!(outcome.evidence().effect_receipts().len(), 1);
    assert_eq!(
        outcome.evidence().effect_receipts()[0].status(),
        EffectStatus::Applied
    );
    Ok(())
}

#[test]
fn nonzero_process_exit_is_authoritative_for_every_purpose() -> TestResult {
    for purpose in [
        ProcessPurpose::ProviderWorker,
        ProcessPurpose::Verification,
        ProcessPurpose::Plugin,
        ProcessPurpose::Git,
        ProcessPurpose::Tool,
    ] {
        let runtime = family("claude")?;
        let registry = registry(
            &runtime,
            Some(all_caps()),
            Behavior::RunProcessPurpose(purpose, false),
        )?;
        let resolved = registry.resolve("claude")?;
        let now = Instant::now();
        let harness = Harness::with_process_termination(
            now,
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(7)?),
        )?;
        let mut sink = RecordingSink::new()?;
        let mut context = ExecutionContext::new(
            &harness,
            &harness,
            &harness,
            &harness,
            &mut sink,
            identity()?,
            now + Duration::from_secs(60),
        );
        let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
        assert_eq!(
            outcome.termination(),
            Some(MechanicalTermination::ProcessExited(
                ProcessExitStatus::code(7)?
            )),
            "purpose {purpose:?} ignored a nonzero exit"
        );
    }
    Ok(())
}

#[test]
fn signaled_provider_process_cannot_be_ignored_into_completion() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::RunProcess)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let status = ProcessExitStatus::signal(9)?;
    let harness =
        Harness::with_process_termination(now, ProcessTerminationReceipt::Exited(status))?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ProcessExited(status))
    );
    Ok(())
}

#[test]
fn discarded_process_output_requires_explicit_acknowledgement() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(
        &runtime,
        Some(all_caps()),
        Behavior::RunProcessPurpose(ProcessPurpose::Tool, false),
    )?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_receipt(
        now,
        ProcessReceipt::new(
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
            b"bounded tail".to_vec(),
            Vec::new(),
            32,
            0,
            true,
            Duration::from_millis(10),
        )?
        .with_truncated_output_acknowledged(),
    )?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    Ok(())
}

#[test]
fn acknowledged_truncated_tool_output_can_complete_after_clean_exit() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(
        &runtime,
        Some(all_caps()),
        Behavior::RunProcessPurpose(ProcessPurpose::Tool, true),
    )?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_process_receipt(
        now,
        ProcessReceipt::new(
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
            b"bounded tail".to_vec(),
            Vec::new(),
            32,
            0,
            true,
            Duration::from_millis(10),
        )?
        .with_truncated_output_acknowledged(),
    )?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert!(outcome.is_completed());
    Ok(())
}

#[test]
fn retained_process_output_over_request_cap_is_authoritative_for_each_stream() -> TestResult {
    for (stream, stdout, stderr) in [
        ("stdout", vec![b'o'; 5], Vec::new()),
        ("stderr", Vec::new(), vec![b'e'; 5]),
    ] {
        let runtime = family("claude")?;
        let registry = registry(
            &runtime,
            Some(all_caps()),
            Behavior::RunProcessWithCap(ProcessPurpose::Tool, 4, true),
        )?;
        let resolved = registry.resolve("claude")?;
        let now = Instant::now();
        let harness = Harness::with_process_receipt(
            now,
            ProcessReceipt::new(
                ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
                stdout,
                stderr,
                0,
                0,
                true,
                Duration::from_millis(10),
            )?,
        )?;
        let mut sink = RecordingSink::new()?;
        let mut context = ExecutionContext::new(
            &harness,
            &harness,
            &harness,
            &harness,
            &mut sink,
            identity()?,
            now + Duration::from_secs(60),
        );

        let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
        assert_eq!(
            outcome.termination(),
            Some(MechanicalTermination::SupervisorFailure),
            "broken service exceeded the {stream} request cap"
        );
        assert_eq!(outcome.failures().len(), 1, "{stream}");
        assert_eq!(outcome.failures()[0].kind(), FailureKind::Infrastructure);
        assert_eq!(
            outcome.failures()[0].expose_detail(),
            "process receipt exceeded the request output bound"
        );
    }
    Ok(())
}

#[test]
fn provider_terminal_is_rejected_and_context_emits_one_failed_terminal() -> TestResult {
    let payload = WorkerEventPayload::Completed(WorkerCompleted::new(8, "1s")?);
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::Emit(payload))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    drop(context);
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert_eq!(
        sink.kinds,
        vec![WorkerEventKind::Spawned, WorkerEventKind::Failed]
    );
    Ok(())
}

#[test]
fn descriptor_is_snapshotted_once_at_registration() -> TestResult {
    let runtime = family("claude")?;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ExecutorRegistry::new();
    let _ = registry.register(
        "claude",
        Arc::new(MockExecutor {
            runtime,
            caps: Some(all_caps()),
            behavior: Behavior::Complete,
            descriptor_calls: Some(Arc::clone(&calls)),
        }),
    )?;
    let _ = registry.resolve("claude")?;
    let _ = registry.resolve("")?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn authoritative_failure_overflow_is_coalesced_not_silently_dropped() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::ManyEffects(40))?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::with_effect_error(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        identity()?,
        now + Duration::from_secs(60),
    );
    let outcome = resolved.execute(&request(runtime, None)?, &mut context)?;
    assert_eq!(outcome.failures().len(), 33);
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation),
        "coalesced authority loss must retain highest contract precedence"
    );
    assert!(
        outcome
            .failures()
            .last()
            .is_some_and(|failure| failure.expose_detail().contains('8'))
    );
    Ok(())
}

#[test]
fn request_bounds_turns_paths_and_identity_controls() -> TestResult {
    let runtime = family("claude")?;
    let mut draft = ExecutionRequestDraft {
        mission: "mission-1".to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective: "objective".to_owned(),
        persona: "persona".to_owned(),
        role: "role".to_owned(),
        domain: "domain".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime,
        model: String::new(),
        effort: Effort::High,
        max_turns: 1_000_001,
        worker_dir: "/fixture".into(),
        target_dir: None,
        resume_from: None,
        hook_script: None,
    };
    assert!(matches!(
        ExecutionRequest::new(draft),
        Err(ContractError::ValueTooLarge {
            field: "max_turns",
            ..
        })
    ));

    draft = ExecutionRequestDraft {
        mission: "mission-1".to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective: "objective".to_owned(),
        persona: "persona".to_owned(),
        role: "role".to_owned(),
        domain: "domain".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime: family("claude")?,
        model: String::new(),
        effort: Effort::High,
        max_turns: 0,
        worker_dir: String::from("/")
            .chars()
            .chain(std::iter::repeat_n('x', 16 * 1024))
            .collect::<String>()
            .into(),
        target_dir: None,
        resume_from: None,
        hook_script: None,
    };
    assert!(matches!(
        ExecutionRequest::new(draft),
        Err(ContractError::TooLong {
            field: "worker_dir",
            ..
        })
    ));
    assert!(matches!(
        WorkerIdentity::new("mission\nforged", "phase", "worker"),
        Err(WorkerEventError::ControlCharacter {
            field: "mission_id"
        })
    ));
    assert!(matches!(
        ArtifactReceipt::new("/absolute/output.md", "digest", 1),
        Err(ContractError::UnnormalizedArtifactPath)
    ));
    assert!(matches!(
        ArtifactReceipt::new("../escape.md", "digest", 1),
        Err(ContractError::UnnormalizedArtifactPath)
    ));
    Ok(())
}

#[test]
fn request_rejects_worker_spawn_metadata_before_dispatch_admission() -> TestResult {
    let mut persona = request_draft(family("claude")?, None, Vec::new());
    persona.persona = "p".repeat(257);
    assert!(matches!(
        ExecutionRequest::new(persona),
        Err(ContractError::TooLong {
            field: "persona",
            max: 256
        })
    ));

    let mut directory = request_draft(family("claude")?, None, Vec::new());
    directory.worker_dir = format!("/{}", "w".repeat(4 * 1024)).into();
    assert!(matches!(
        ExecutionRequest::new(directory),
        Err(ContractError::TooLong {
            field: "dir",
            max: 4096
        })
    ));

    let mut directory_control = request_draft(family("claude")?, None, Vec::new());
    directory_control.worker_dir = "/fixture/\u{7}".into();
    assert!(matches!(
        ExecutionRequest::new(directory_control),
        Err(ContractError::ControlCharacter { field: "dir" })
    ));
    Ok(())
}

#[test]
fn process_exit_status_cannot_represent_code_and_signal_together() -> TestResult {
    let code = MechanicalTermination::ProcessExited(ProcessExitStatus::code(1)?);
    let signal = MechanicalTermination::ProcessExited(ProcessExitStatus::signal(9)?);
    assert_ne!(code, signal);
    assert!(ProcessExitStatus::code(-1).is_err());
    assert!(ProcessExitStatus::signal(0).is_err());
    Ok(())
}

#[test]
fn fallback_preserves_requested_and_effective_runtime() -> TestResult {
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::Complete)?;
    let resolved = registry.resolve("future-runtime")?;
    assert_eq!(resolved.resolution(), ResolutionKind::Fallback);
    assert_eq!(resolved.requested_runtime().as_str(), "future-runtime");
    assert_eq!(resolved.effective_runtime().as_str(), "claude");
    Ok(())
}

#[test]
fn committed_go_worker_fixtures_round_trip_semantically() -> TestResult {
    for fixture in [
        include_str!("fixtures/go_worker_spawned.json"),
        include_str!("fixtures/go_worker_output.json"),
        include_str!("fixtures/go_worker_completed.json"),
        include_str!("fixtures/go_worker_failed.json"),
    ] {
        let decoded = WorkerEventEnvelope::from_json(fixture)?;
        let encoded = decoded.to_json()?;
        let original: serde_json::Value = serde_json::from_str(fixture)?;
        let round_trip: serde_json::Value = serde_json::from_str(&encoded)?;
        assert_eq!(round_trip, original);
    }
    Ok(())
}

#[test]
fn legacy_go_event_without_phase_or_worker_round_trips() -> TestResult {
    let legacy = r#"{
        "id":"evt_legacy000000001",
        "type":"worker.output",
        "timestamp":"2026-07-15T00:00:00.123+12:00",
        "sequence":1,
        "mission_id":"mission-1",
        "data":{"chunk":"legacy","event_kind":"text"}
    }"#;
    let decoded = WorkerEventEnvelope::from_json(legacy)?;
    assert!(decoded.identity().phase_id().is_empty());
    assert!(decoded.identity().worker_id().is_empty());
    let original: serde_json::Value = serde_json::from_str(legacy)?;
    let round_trip: serde_json::Value = serde_json::from_str(&decoded.to_json()?)?;
    assert_eq!(round_trip, original);
    Ok(())
}

#[test]
fn persisted_worker_codec_accepts_bounded_non_prefixed_event_id_but_live_receipt_rejects_it()
-> TestResult {
    let legacy = r#"{"id":"legacy-event-1","type":"worker.output","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","data":{"chunk":"legacy","event_kind":"text"}}"#;
    let decoded = WorkerEventEnvelope::from_json(legacy)?;
    assert_eq!(decoded.receipt().id(), "legacy-event-1");
    assert_eq!(decoded.preserved_source_json(), Some(legacy.as_bytes()));
    assert!(matches!(
        EventReceipt::new("legacy-event-1", "2026-07-15T00:00:00Z", 1),
        Err(WorkerEventError::InvalidEventId)
    ));
    Ok(())
}

#[test]
fn live_worker_identity_still_rejects_empty_phase_and_worker() {
    assert!(matches!(
        WorkerIdentity::new("mission-1", "", "worker-1"),
        Err(WorkerEventError::Empty { field: "phase_id" })
    ));
    assert!(matches!(
        WorkerIdentity::new("mission-1", "phase-1", ""),
        Err(WorkerEventError::Empty { field: "worker_id" })
    ));
}

#[test]
fn legacy_identity_cannot_be_reused_for_live_dispatch() -> TestResult {
    let legacy = r#"{
        "id":"evt_legacy000000002",
        "type":"worker.output",
        "timestamp":"2026-07-15T00:00:00Z",
        "sequence":1,
        "mission_id":"mission-1",
        "phase_id":"phase-1",
        "data":{"chunk":"legacy","event_kind":"text"}
    }"#;
    let decoded = WorkerEventEnvelope::from_json(legacy)?;
    let legacy_identity = decoded.identity().clone();
    let runtime = family("claude")?;
    let registry = registry(&runtime, Some(all_caps()), Behavior::Complete)?;
    let resolved = registry.resolve("claude")?;
    let now = Instant::now();
    let harness = Harness::success(now)?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &harness,
        &harness,
        &harness,
        &harness,
        &mut sink,
        legacy_identity,
        now + Duration::from_secs(60),
    );
    assert!(matches!(
        resolved.execute(&request(runtime, None)?, &mut context),
        Err(DispatchError::WorkerIdentityMismatch)
    ));
    Ok(())
}

#[test]
fn event_receipt_requires_rfc3339_timestamp() -> TestResult {
    assert!(matches!(
        EventReceipt::new("evt_0123456789abcdef", "2026-02-30T00:00:00Z", 1),
        Err(WorkerEventError::InvalidTimestamp)
    ));
    assert!(matches!(
        EventReceipt::new("evt_0123456789abcdef", "2026-07-15 00:00:00Z", 1),
        Err(WorkerEventError::InvalidTimestamp)
    ));
    assert!(
        EventReceipt::new(
            "evt_0123456789abcdef",
            "2026-07-15T00:00:00.123456789+12:00",
            1,
        )
        .is_ok()
    );
    Ok(())
}

#[test]
fn worker_codec_preserves_unknown_envelope_and_data_fields_exactly() -> TestResult {
    let forward = r#"{
        "id":"evt_0123456789abcdef",
        "type":"worker.output",
        "timestamp":"2026-07-15T00:00:00Z",
        "sequence":1,
        "mission_id":"mission-1",
        "phase_id":"phase-1",
        "worker_id":"worker-1",
        "future_envelope":{"trace":"preserve"},
        "data":{"chunk":"x","event_kind":"text","streaming":true,"future_data":[1,2,3]}
    }"#;
    let decoded = WorkerEventEnvelope::from_json(forward)?;
    assert_eq!(decoded.preserved_source_json(), Some(forward.as_bytes()));
    assert_eq!(decoded.to_json()?, forward);
    Ok(())
}

#[test]
fn worker_codec_preserves_unknown_only_historical_output_without_weakening_live_writes()
-> TestResult {
    let forward = r#"{"id":"evt_0123456789abcdef","type":"worker.output","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","phase_id":"phase-1","worker_id":"worker-1","data":{"future_output":{"kind":"delta"}}}"#;
    let decoded = WorkerEventEnvelope::from_json(forward)?;
    assert_eq!(decoded.kind(), WorkerEventKind::Output);
    assert_eq!(decoded.preserved_source_json(), Some(forward.as_bytes()));
    assert_eq!(decoded.to_json()?, forward);
    assert!(matches!(
        WorkerOutput::new(WorkerOutputFields {
            chunk: None,
            event_kind: None,
            streaming: None,
            tool_name: None,
            is_error: None,
            output_len: None,
            duration: None,
        }),
        Err(WorkerEventError::EmptyPayload)
    ));
    Ok(())
}

#[test]
fn worker_codec_rejects_wrong_type_for_known_field_with_unknown_fields_present() {
    let malformed = r#"{
        "id":"evt_0123456789abcdef",
        "type":"worker.output",
        "timestamp":"2026-07-15T00:00:00Z",
        "sequence":1,
        "mission_id":"mission-1",
        "phase_id":"phase-1",
        "worker_id":"worker-1",
        "future_envelope":true,
        "data":{"chunk":"x","streaming":"not-a-boolean","future_data":true}
    }"#;
    assert!(matches!(
        WorkerEventEnvelope::from_json(malformed),
        Err(WorkerEventCodecError::Json(_))
    ));
}

#[test]
fn worker_codec_rejects_oversized_unknown_payload_before_deserialization() {
    let prefix = r#"{"id":"evt_0123456789abcdef","type":"worker.output","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","data":{"chunk":"x"},"future":""#;
    let suffix = r#""}"#;
    let padding = GO_EVENT_JSON_CONTENT_MAX_BYTES + 1 - prefix.len() - suffix.len();
    let oversized = format!("{prefix}{}{suffix}", "x".repeat(padding));

    assert_eq!(oversized.len(), GO_EVENT_JSON_CONTENT_MAX_BYTES + 1);
    assert!(matches!(
        WorkerEventEnvelope::from_json(&oversized),
        Err(WorkerEventCodecError::EventTooLarge {
            max: GO_EVENT_JSON_CONTENT_MAX_BYTES
        })
    ));
}

#[test]
fn worker_codec_accepts_largest_go_scanner_record_with_preserved_line_terminator() -> TestResult {
    let prefix = r#"{"id":"evt_0123456789abcdef","type":"worker.output","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","data":{"chunk":"x"},"future":""#;
    let suffix = r#""}"#;
    let padding = GO_EVENT_JSON_CONTENT_MAX_BYTES - prefix.len() - suffix.len();
    let at_limit = format!("{prefix}{}{suffix}\n", "x".repeat(padding));

    assert_eq!(at_limit.len(), 1024 * 1024);
    let decoded = WorkerEventEnvelope::from_json(&at_limit)?;
    assert_eq!(decoded.preserved_source_json(), Some(at_limit.as_bytes()));
    Ok(())
}

#[test]
fn worker_codec_rejects_duplicate_known_envelope_fields_as_ambiguous() {
    let ambiguous = r#"{
        "id":"evt_0123456789abcdef",
        "type":"worker.completed",
        "timestamp":"2026-07-15T00:00:00Z",
        "sequence":1,
        "sequence":2,
        "mission_id":"mission-1",
        "phase_id":"phase-1",
        "worker_id":"worker-1",
        "data":{"output_len":1,"duration":"1ms","attempt":1}
    }"#;
    assert!(matches!(
        WorkerEventEnvelope::from_json(ambiguous),
        Err(WorkerEventCodecError::Json(_))
    ));
}

#[test]
fn worker_codec_rejects_duplicate_identity_and_terminal_evidence_inside_data() {
    for ambiguous in [
        r#"{"id":"evt_0123456789abcdef","type":"worker.output","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","phase_id":"phase-1","worker_id":"worker-1","data":{"chunk":"x","attempt":1,"attempt":2}}"#,
        r#"{"id":"evt_0123456789abcdef","type":"worker.completed","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","phase_id":"phase-1","worker_id":"worker-1","data":{"output_len":1,"output_len":2,"duration":"1ms","attempt":1}}"#,
    ] {
        assert!(matches!(
            WorkerEventEnvelope::from_json(ambiguous),
            Err(WorkerEventCodecError::Json(_))
        ));
    }
}

#[test]
fn worker_codec_reads_legacy_spawn_without_model_but_still_requires_directory() -> TestResult {
    let legacy = r#"{"id":"evt_0123456789abcdef","type":"worker.spawned","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","phase_id":"phase-1","worker_id":"worker-1","data":{"dir":"/fixture/worker"}}"#;
    let decoded = WorkerEventEnvelope::from_json(legacy)?;
    let WorkerEventPayload::Spawned(spawned) = decoded.payload() else {
        return Err("expected worker.spawned payload".into());
    };
    assert!(spawned.model().is_empty());
    assert_eq!(spawned.directory(), "/fixture/worker");

    let missing_directory = r#"{"id":"evt_0123456789abcdef","type":"worker.spawned","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-1","phase_id":"phase-1","worker_id":"worker-1","data":{}}"#;
    assert!(matches!(
        WorkerEventEnvelope::from_json(missing_directory),
        Err(WorkerEventCodecError::Json(_))
    ));
    Ok(())
}

#[test]
fn debug_redacts_service_requests_progress_events_and_receipts() -> TestResult {
    let process = ProcessRequest::new(ProcessPurpose::ProviderWorker, "provider", "/secret/root")?
        .with_argument("prompt-secret")?
        .with_environment("TOKEN", "token-secret")?
        .with_stdin(b"stdin-secret".to_vec())?;
    let effect = EffectRequest::new(EffectKind::PluginAction, "resource-secret", "key-secret")?
        .with_input(b"effect-secret".to_vec())?;
    let envelope = WorkerEventEnvelope::from_json(include_str!("fixtures/go_worker_failed.json"))?;
    let runtime = family("claude")?;
    let failure = Failure::new(FailureKind::Infrastructure, "failure-secret")?;
    let work = PartialWork::new(Some("partial-secret".to_owned()), all_evidence(&runtime)?)?;
    let outcome = AttemptOutcome::incomplete(
        MechanicalTermination::SupervisorFailure,
        Some(failure),
        work,
        Duration::ZERO,
    );
    let rendered = format!("{process:?} {effect:?} {envelope:?} {outcome:?}");
    for secret in [
        "/secret/root",
        "prompt-secret",
        "token-secret",
        "stdin-secret",
        "resource-secret",
        "key-secret",
        "effect-secret",
        "sanitized provider failure",
        "bounded sanitized stderr",
        "failure-secret",
        "partial-secret",
        "continuation-secret",
        "tool secret",
        "outputs/output.md",
        "digest-secret",
    ] {
        assert!(!rendered.contains(secret), "debug leaked a protected field");
    }
    Ok(())
}

#[test]
fn go_payload_constructors_preserve_committed_optional_fields() -> TestResult {
    let spawned = WorkerSpawned::new(WorkerSpawnedFields {
        model: "claude-opus".to_owned(),
        runtime: None,
        effort_level: Some(Effort::High),
        persona: Some("rust-engineer".to_owned()),
        directory: "/fixture/workers/p1".to_owned(),
    })?;
    assert!(spawned.runtime().is_none());
    assert_eq!(spawned.effort_level(), Some(Effort::High));

    let failed = WorkerFailed::new(WorkerFailedFields {
        error: "sanitized".to_owned(),
        duration: None,
        output_len: None,
        exit_code: None,
        stderr_tail: None,
    })?;
    assert!(failed.duration().is_none());
    assert!(failed.exit_code().is_none());
    Ok(())
}
