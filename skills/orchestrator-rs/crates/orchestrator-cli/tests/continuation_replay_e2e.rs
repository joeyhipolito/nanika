//! End-to-end coverage for B2 continuation/replay at the CLI dispatch seam.
//!
//! The store that persists `continuation.handle_recorded` /
//! `continuation.decision_selected` is sealed out-of-crate
//! (`orchestrator_app::runtime_home::ProductionBoundary::from_canonical_root`
//! is `#[cfg(test)] pub(crate)`, proven by
//! `orchestrator-app/tests/authority_compile_fail.rs`), so this suite — like
//! `run_one_phase_e2e.rs` — uses only public exec-layer types: `SessionHandle`,
//! `ExecutionRequest`, `ExecutorRegistry`, and the CLI's injectable
//! [`orchestrator_cli::run_one_phase`] seam, now threading a
//! `resume_from: Option<SessionHandle>` through to the executor. The
//! store-backed halves (byte-identical persisted replay, the capsule
//! seeded-secret scan, and the durable-terminal short-circuit) are proven
//! in-crate in `orchestrator-app/src/runtime_store.rs` and
//! `orchestrator-app/src/mission_service.rs`.

use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use orchestrator_cli::{
    RunExecutionError, RunResolutionContext, resolve_with_context, run_one_phase,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, ContractError, DispatchError,
    DispatchRequest, EffectBudget, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, Effort, EventReceipt, EventSink, EventSinkError, EventSinkErrorKind,
    ExecutionContext, ExecutionRequest, ExecutionRequestDraft, ExecutorRegistry,
    MechanicalTermination, PartialWork, PhaseExecutor, ProcessBudget, ProcessPreflight,
    ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind,
    RuntimeCap, RuntimeCaps, RuntimeDescriptor, RuntimeFamily, SessionHandle, WatchdogDecision,
    WatchdogPolicy, WorkerEventDraft, WorkerEventKind, WorkerIdentity,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "continuation-replay-mission";
const WORKER: &str = "worker-1";
const CLAUDE: &str = "claude";
const CODEX: &str = "codex";

fn args(slice: &[&str]) -> Vec<String> {
    slice.iter().map(|value| (*value).to_owned()).collect()
}

// --- Minimal inert services, mirroring run_one_phase_e2e.rs. ---

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
            "continuation replay test admits no effects",
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

struct NoProcess {
    error: ProcessServiceError,
}

impl NoProcess {
    fn new() -> TestResult<Self> {
        Ok(Self {
            error: ProcessServiceError::new(
                ProcessServiceErrorKind::Unavailable,
                "continuation replay test spawns no process",
            )?,
        })
    }
}

impl Cancellation for NoProcess {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ProcessService for NoProcess {
    fn finish_preflight(
        &self,
        _request: &ProcessRequest,
        _preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        Ok(())
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        Err(self.error.clone())
    }
}

struct RecordingSink {
    events: Arc<Mutex<Vec<WorkerEventKind>>>,
    sequence: AtomicU64,
    error: EventSinkError,
}

impl RecordingSink {
    fn new() -> TestResult<Self> {
        Ok(Self {
            events: Arc::new(Mutex::new(Vec::new())),
            sequence: AtomicU64::new(0),
            error: EventSinkError::new(
                EventSinkErrorKind::Rejected,
                "recording sink could not mint a receipt",
            )?,
        })
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut events) = self.events.lock() {
            events.push(event.kind());
        }
        let sequence = i64::try_from(sequence).unwrap_or(i64::MAX);
        match EventReceipt::new(format!("evt_{sequence}"), "2026-07-24T00:00:00Z", sequence) {
            Ok(receipt) => Ok(receipt),
            Err(_) => Err(self.error.clone()),
        }
    }
}

/// A deterministic, provider-free executor that records whether the dispatched
/// request carried a resume handle, optionally attaches a session to its
/// evidence, and — when `resume_from` is absent — counts the dispatch as a
/// fresh accepted effect. A resumed dispatch (`resume_from.is_some()`) never
/// increments the effect counter: this is the CLI-level analog of "resume
/// repeats no accepted effect" (N2), whose store-backed half (durable-terminal
/// short-circuit on a crash-injected fault) is proven in-crate.
struct RecordingExecutor {
    saw_resume: Arc<Mutex<Option<bool>>>,
    session_to_attach: Option<SessionHandle>,
    descriptor: Option<RuntimeDescriptor>,
    fresh_effect_count: Arc<AtomicU64>,
}

impl PhaseExecutor for RecordingExecutor {
    fn execute(
        &self,
        request: DispatchRequest<'_>,
        _context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let resumed = request.request().resume_from().is_some();
        if let Ok(mut saw) = self.saw_resume.lock() {
            *saw = Some(resumed);
        }
        if !resumed {
            self.fresh_effect_count.fetch_add(1, Ordering::Relaxed);
        }
        let mut evidence = AttemptEvidence::new();
        if let Some(session) = self.session_to_attach.clone() {
            evidence = evidence.with_session(session);
        }
        AttemptOutcome::completed(
            "continuation replay complete",
            evidence,
            Duration::from_millis(1),
        )
        .unwrap_or_else(|_| {
            AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::from_millis(1),
            )
        })
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        self.descriptor.clone()
    }
}

fn one_phase_offline(task: &str) -> TestResult<orchestrator_cli::ResolvedRun> {
    let resolved = resolve_with_context(
        &args(&["--offline", task]),
        &RunResolutionContext::default(),
    )?;
    Ok(resolved)
}

fn build_context<'a>(
    process: &'a NoProcess,
    clock: &'a SystemClock,
    watchdog: &'a FixedWatchdog,
    effect: &'a DeniedEffect,
    sink: &'a mut RecordingSink,
    phase_id: impl ToString,
) -> TestResult<ExecutionContext<'a>> {
    let identity = WorkerIdentity::new(MISSION, phase_id.to_string(), WORKER)?;
    Ok(ExecutionContext::new(
        process,
        clock,
        watchdog,
        effect,
        sink,
        identity,
        Instant::now() + Duration::from_secs(5),
    ))
}

/// P1 — a prior `claude` handle exists and the next attempt's runtime is
/// `claude` too: the request threads `resume_from = Some(handle)` and the
/// dispatched executor observes it.
#[test]
fn same_provider_resume_is_threaded_into_the_request() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    let phase = &resolved.phases[0];

    let handle = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, "prior-session-id")?;
    let saw_resume = Arc::new(Mutex::new(None));

    let mut registry = ExecutorRegistry::new();
    registry.register(
        CLAUDE,
        Arc::new(RecordingExecutor {
            saw_resume: Arc::clone(&saw_resume),
            session_to_attach: None,
            descriptor: None,
            fresh_effect_count: Arc::new(AtomicU64::new(0)),
        }),
    )?;

    let process = NoProcess::new()?;
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = RecordingSink::new()?;
    let mut context = build_context(&process, &clock, &watchdog, &effect, &mut sink, &phase.id)?;

    let outcome = run_one_phase(MISSION, "dev", phase, &registry, &mut context, Some(handle))?;
    assert!(outcome.is_completed());
    assert_eq!(
        *saw_resume.lock().map_err(|_| "poisoned")?,
        Some(true),
        "the executor must observe a threaded resume handle"
    );
    Ok(())
}

/// P2 — the prior handle is `claude` but the next attempt's runtime is
/// `codex` (cross-provider): the continuation layer selects `resume_from =
/// None`, so a fresh `codex` session is requested and dispatched — the
/// `codex` executor runs and never observes a resume handle.
#[test]
fn cross_provider_fresh_session_carries_no_resume_handle() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    let mut phase = resolved.phases[0].clone();
    phase.effective_runtime = CODEX.to_owned();

    let saw_resume = Arc::new(Mutex::new(None));
    let mut registry = ExecutorRegistry::new();
    registry.register(
        CODEX,
        Arc::new(RecordingExecutor {
            saw_resume: Arc::clone(&saw_resume),
            session_to_attach: None,
            descriptor: None,
            fresh_effect_count: Arc::new(AtomicU64::new(0)),
        }),
    )?;

    let process = NoProcess::new()?;
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = RecordingSink::new()?;
    let mut context = build_context(&process, &clock, &watchdog, &effect, &mut sink, &phase.id)?;

    // The continuation decision for a cross-family switch is `FreshFromCapsule`
    // (`select_resume_or_fresh`, orchestrator-app): no handle is threaded.
    let outcome = run_one_phase(MISSION, "dev", &phase, &registry, &mut context, None)?;
    assert!(outcome.is_completed());
    assert_eq!(
        *saw_resume.lock().map_err(|_| "poisoned")?,
        Some(false),
        "a cross-provider fresh session must never carry a resume handle"
    );
    Ok(())
}

/// N1 (layer 1) — `ExecutionRequest::new` rejects a resume handle whose family
/// differs from the request's own runtime, fail-closed, before any dispatch.
#[test]
fn cross_family_handle_is_rejected_at_construction() -> TestResult {
    let claude_handle = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, "claude-session")?;
    let draft = ExecutionRequestDraft {
        mission: MISSION.to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective: "objective".to_owned(),
        persona: "persona".to_owned(),
        role: "role".to_owned(),
        domain: "dev".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(CODEX)?,
        model: "model".to_owned(),
        effort: Effort::High,
        max_turns: 0,
        worker_dir: std::path::PathBuf::from("."),
        target_dir: None,
        resume_from: Some(claude_handle),
        hook_script: None,
    };
    let result = ExecutionRequest::new(draft);
    assert!(
        matches!(result, Err(ContractError::SessionRuntimeMismatch)),
        "a claude handle must never construct into a codex request, got {result:?}"
    );
    Ok(())
}

/// N1 (layer 2) — even a request built for the *matching* family is rejected
/// at dispatch if the resolved executor's descriptor does not advertise
/// `SessionResume`. This is the capability gate `ResolvedExecutor::execute`
/// enforces independently of the construction-time family check.
#[test]
fn resume_without_session_resume_capability_is_rejected_at_dispatch() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    let phase = &resolved.phases[0];

    let handle = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, "prior-session-id")?;
    let no_resume_descriptor = RuntimeDescriptor::new(
        RuntimeFamily::parse(CLAUDE)?,
        RuntimeCaps {
            tool_use: true,
            session_resume: false,
            streaming: false,
            cost_report: false,
            artifacts: false,
        },
    );

    let mut registry = ExecutorRegistry::new();
    registry.register(
        CLAUDE,
        Arc::new(RecordingExecutor {
            saw_resume: Arc::new(Mutex::new(None)),
            session_to_attach: None,
            descriptor: Some(no_resume_descriptor),
            fresh_effect_count: Arc::new(AtomicU64::new(0)),
        }),
    )?;

    let process = NoProcess::new()?;
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = RecordingSink::new()?;
    let mut context = build_context(&process, &clock, &watchdog, &effect, &mut sink, &phase.id)?;

    let result = run_one_phase(MISSION, "dev", phase, &registry, &mut context, Some(handle));
    assert!(
        matches!(
            result,
            Err(RunExecutionError::Dispatch(
                DispatchError::UnsupportedCapability(RuntimeCap::SessionResume)
            ))
        ),
        "a runtime lacking SessionResume must reject a resume dispatch, got {result:?}"
    );
    Ok(())
}

/// N2 — an interrupted-then-resumed attempt repeats no accepted effect. The
/// first (fresh) dispatch counts as one accepted effect; the second dispatch
/// carries the handle the first attempt's evidence returned and is observed
/// as a resume, so it is never counted as a second fresh effect. The durable
/// short-circuit itself (`DurableAttemptOutcome`, no second event) is proven
/// against the real store in-crate; this is the exec-layer analog available
/// through the CLI's public dispatch seam.
#[test]
fn interrupted_resume_never_repeats_a_fresh_effect() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    let phase = &resolved.phases[0];

    let fresh_effect_count = Arc::new(AtomicU64::new(0));
    let saw_resume = Arc::new(Mutex::new(None));
    let session = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, "attempt-1-session")?;

    let mut registry = ExecutorRegistry::new();
    registry.register(
        CLAUDE,
        Arc::new(RecordingExecutor {
            saw_resume: Arc::clone(&saw_resume),
            session_to_attach: Some(session.clone()),
            descriptor: None,
            fresh_effect_count: Arc::clone(&fresh_effect_count),
        }),
    )?;

    // Attempt 1: fresh dispatch, no resume handle yet.
    let process = NoProcess::new()?;
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = RecordingSink::new()?;
    let mut context = build_context(&process, &clock, &watchdog, &effect, &mut sink, &phase.id)?;
    let first = run_one_phase(MISSION, "dev", phase, &registry, &mut context, None)?;
    assert!(first.is_completed());
    let recovered_session = first
        .evidence()
        .session()
        .cloned()
        .ok_or("attempt 1 must return a session handle to resume into")?;
    assert_eq!(fresh_effect_count.load(Ordering::Relaxed), 1);

    // Attempt 2 ("resume" after an injected crash between the effect commit and
    // the phase terminating): the recovered handle is threaded back in.
    let mut sink2 = RecordingSink::new()?;
    let mut context2 = build_context(&process, &clock, &watchdog, &effect, &mut sink2, &phase.id)?;
    let second = run_one_phase(
        MISSION,
        "dev",
        phase,
        &registry,
        &mut context2,
        Some(recovered_session),
    )?;
    assert!(second.is_completed());
    assert_eq!(
        *saw_resume.lock().map_err(|_| "poisoned")?,
        Some(true),
        "the second dispatch must be observed as a resume"
    );
    assert_eq!(
        fresh_effect_count.load(Ordering::Relaxed),
        1,
        "a resumed dispatch must never be counted as a second fresh effect"
    );
    Ok(())
}

/// N3 — a `SessionHandle`'s id never appears in ordinary `Debug` output,
/// including the request that carries it: the exec-layer analog of "no seeded
/// secret leaks into a provider-neutral record" (the capsule-level scan over
/// typed reasoning rows is proven in-crate in `orchestrator-app`).
#[test]
fn resume_session_id_never_leaks_through_debug_output() -> TestResult {
    const SENTINEL: &str = "SECRET-2af9c1e4-session-id";
    let handle = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, SENTINEL)?;
    let handle_debug = format!("{handle:?}");
    assert!(
        !handle_debug.contains(SENTINEL),
        "SessionHandle::Debug must redact the session id, got: {handle_debug}"
    );

    let draft = ExecutionRequestDraft {
        mission: MISSION.to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective: "objective".to_owned(),
        persona: "persona".to_owned(),
        role: "role".to_owned(),
        domain: "dev".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(CLAUDE)?,
        model: "model".to_owned(),
        effort: Effort::High,
        max_turns: 0,
        worker_dir: std::path::PathBuf::from("."),
        target_dir: None,
        resume_from: Some(handle),
        hook_script: None,
    };
    let request = ExecutionRequest::new(draft)?;
    let request_debug = format!("{request:?}");
    assert!(
        !request_debug.contains(SENTINEL),
        "ExecutionRequest::Debug must never surface a resumed session id, got: {request_debug}"
    );
    Ok(())
}

/// A minimal, local, provider-neutral replay record — the CLI-visible fields
/// of `orchestrator_app`'s (store-sealed) `continuation.decision_selected`
/// payload — computed purely from durable inputs (a persisted handle and the
/// next resolved runtime), with no clock and no live probe.
#[derive(Debug, PartialEq, Eq)]
struct ReplayRecord {
    attempt: u32,
    chosen_runtime: String,
    resumed_same_provider: bool,
}

fn decide(
    persisted: Option<&SessionHandle>,
    next_runtime: &RuntimeFamily,
    attempt: u32,
) -> ReplayRecord {
    let resumed_same_provider =
        persisted.is_some_and(|handle| handle.can_resume_into(next_runtime));
    ReplayRecord {
        attempt,
        chosen_runtime: next_runtime.as_str().to_owned(),
        resumed_same_provider,
    }
}

/// P3 / N4 — replaying the same decision twice over identical durable inputs
/// (no clock, no live probe) yields byte-identical output.
#[test]
fn decision_replay_is_byte_identical_across_two_runs() -> TestResult {
    let handle = SessionHandle::new(RuntimeFamily::parse(CLAUDE)?, "replay-session")?;
    let next_runtime = RuntimeFamily::parse(CLAUDE)?;

    let first = decide(Some(&handle), &next_runtime, 2);
    let second = decide(Some(&handle), &next_runtime, 2);
    assert_eq!(first, second);
    assert_eq!(
        format!("{first:?}").into_bytes(),
        format!("{second:?}").into_bytes(),
        "replaying an identical decision twice must produce byte-identical output"
    );
    assert!(first.resumed_same_provider);

    // A cross-family replay of the *same* inputs is equally deterministic and
    // always selects fresh, never resume.
    let codex_runtime = RuntimeFamily::parse(CODEX)?;
    let cross_family_first = decide(Some(&handle), &codex_runtime, 2);
    let cross_family_second = decide(Some(&handle), &codex_runtime, 2);
    assert_eq!(cross_family_first, cross_family_second);
    assert!(!cross_family_first.resumed_same_provider);
    Ok(())
}

/// Confirms the shipped binary's `--offline` one-phase seam still runs end to
/// end with the new `resume_from` parameter in place (no regression to the
/// slice-1 gate's observable behavior).
#[test]
fn shipped_binary_offline_dispatch_is_unaffected() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_orchestrator");
    let offline = Command::new(bin)
        .args(["run", "--offline", "research the topic and write findings"])
        .output()?;
    assert!(
        offline.status.success(),
        "offline run should succeed: {}",
        String::from_utf8_lossy(&offline.stderr)
    );
    let stdout = String::from_utf8_lossy(&offline.stdout);
    assert!(
        stdout.contains("phases: 1"),
        "offline run should render one fallback phase, got:\n{stdout}"
    );
    Ok(())
}
