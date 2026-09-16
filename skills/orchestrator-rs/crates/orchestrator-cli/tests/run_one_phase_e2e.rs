//! End-to-end coverage for the CLI's one-phase dispatch seam.
//!
//! `orchestrator run --offline <task>` compiles a plan through the deterministic
//! keyword fallback (no authored `PHASE:` records, no live provider), replacing
//! the historical `phases = Vec::new()` single-worker degrade. The injectable
//! [`orchestrator_cli::run_one_phase`] seam then drives one released phase to a
//! terminal `AttemptOutcome::completed` through the enforced executor registry —
//! the sole producer of the dispatch request — emitting exactly one
//! `worker.spawned` and one terminal `worker.completed` event, instead of the
//! historical `ExecutionNotEnrolled` dead-end.
//!
//! The negatives fail closed: a non-offline plain task keeps the default
//! single-worker resolution (no fallback), the shipped binary keeps every
//! non-offline non-dry-run at the enrollment gate, and an empty registry (the
//! shipped binary enrolls no local executor in this slice) refuses to dispatch
//! rather than falling through.

use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use orchestrator_cli::{
    RunExecutionError, RunResolutionContext, resolve_with_context, run_one_phase,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchRequest, EffectBudget,
    EffectReceipt, EffectRequest, EffectService, EffectServiceError, EffectServiceErrorKind,
    EventReceipt, EventSink, EventSinkError, EventSinkErrorKind, ExecutionContext,
    ExecutorRegistry, MechanicalTermination, PartialWork, PhaseExecutor, ProcessBudget,
    ProcessPreflight, ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError,
    ProcessServiceErrorKind, RuntimeDescriptor, WatchdogDecision, WatchdogPolicy, WorkerEventDraft,
    WorkerEventKind, WorkerIdentity,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "run-one-phase-mission";
const WORKER: &str = "worker-1";
const CLAUDE: &str = "claude";

fn args(slice: &[&str]) -> Vec<String> {
    slice.iter().map(|value| (*value).to_owned()).collect()
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
            "offline local executor admits no effects",
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

/// A process service the offline echo executor never invokes. It exists only to
/// satisfy [`ExecutionContext::new`] and is a non-zero-sized type so the
/// registry's identity check accepts it.
struct NoProcess {
    error: ProcessServiceError,
}

impl NoProcess {
    fn new() -> TestResult<Self> {
        Ok(Self {
            error: ProcessServiceError::new(
                ProcessServiceErrorKind::Unavailable,
                "offline local executor spawns no process",
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

/// An event sink that accepts every emit (so the enforced lifecycle does not
/// fold into an `EventDeliveryFailure`) and records each event kind for
/// assertion.
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

    fn events(&self) -> Arc<Mutex<Vec<WorkerEventKind>>> {
        Arc::clone(&self.events)
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

/// A deterministic, provider-free local executor: it does no I/O and returns a
/// completed terminal outcome. This is the minimal embodiment of the local
/// executor protocol — a local executor registered in the registry and driven
/// through the enforced `ResolvedExecutor::execute` seam.
struct LocalEchoExecutor;

impl PhaseExecutor for LocalEchoExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        _context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        AttemptOutcome::completed(
            "offline local execution complete",
            AttemptEvidence::new(),
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
        None
    }
}

fn one_phase_offline(task: &str) -> TestResult<orchestrator_cli::ResolvedRun> {
    let resolved = resolve_with_context(
        &args(&["--offline", task]),
        &RunResolutionContext::default(),
    )?;
    Ok(resolved)
}

#[test]
fn offline_one_phase_dispatches_to_a_completed_terminal_outcome() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    assert_eq!(
        resolved.phases.len(),
        1,
        "the keyword fallback should compile one research phase"
    );
    let phase = &resolved.phases[0];

    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register(CLAUDE, Arc::new(LocalEchoExecutor))?;

    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let process = NoProcess::new()?;
    let mut sink = RecordingSink::new()?;
    let recorded = sink.events();
    let identity = WorkerIdentity::new(MISSION, phase.id.to_string(), WORKER)?;
    let mut context = ExecutionContext::new(
        &process,
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        identity,
        Instant::now() + Duration::from_secs(5),
    );

    let outcome = run_one_phase(MISSION, "dev", phase, &registry, &mut context, None)?;
    assert!(
        outcome.is_completed(),
        "dispatch should reach a completed terminal outcome"
    );

    let events = recorded.lock().map_err(|_| "recorded events poisoned")?;
    assert_eq!(
        events
            .iter()
            .filter(|kind| matches!(kind, WorkerEventKind::Spawned))
            .count(),
        1,
        "exactly one worker.spawned should be emitted, got {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|kind| matches!(kind, WorkerEventKind::Completed))
            .count(),
        1,
        "exactly one terminal worker.completed should be emitted, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|kind| matches!(kind, WorkerEventKind::Failed)),
        "a completed outcome must not emit worker.failed, got {events:?}"
    );
    Ok(())
}

#[test]
fn offline_dispatch_with_no_enrolled_executor_fails_closed() -> TestResult {
    let resolved = one_phase_offline("research the topic and write findings")?;
    assert_eq!(resolved.phases.len(), 1);
    let phase = &resolved.phases[0];

    // An empty registry models the shipped binary, which enrolls no local
    // executor in this slice.
    let registry = ExecutorRegistry::new();

    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let effect = DeniedEffect::new()?;
    let process = NoProcess::new()?;
    let mut sink = RecordingSink::new()?;
    let recorded = sink.events();
    let identity = WorkerIdentity::new(MISSION, phase.id.to_string(), WORKER)?;
    let mut context = ExecutionContext::new(
        &process,
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        identity,
        Instant::now() + Duration::from_secs(5),
    );

    let result = run_one_phase(MISSION, "dev", phase, &registry, &mut context, None);
    assert!(
        matches!(result, Err(RunExecutionError::Registry(_))),
        "an empty registry must fail closed at resolution, not dispatch"
    );
    // Nothing was dispatched, so no lifecycle event was emitted.
    let events = recorded.lock().map_err(|_| "recorded events poisoned")?;
    assert!(
        events.is_empty(),
        "a failed-closed resolution must not emit any worker event, got {events:?}"
    );
    Ok(())
}

#[test]
fn keyword_fallback_engages_only_offline() -> TestResult {
    // Offline: a plain-text task with "implement"/"test" compiles two phases.
    let offline = resolve_with_context(
        &args(&[
            "--offline",
            "implement the feature and add tests to verify it",
        ]),
        &RunResolutionContext::default(),
    )?;
    assert_eq!(
        offline.phases.len(),
        2,
        "the offline fallback should compile an implement + verify plan"
    );

    // Non-offline: the same plain task keeps the default single-worker
    // resolution (no fallback), preserving the existing render/parity contract.
    let plain = resolve_with_context(
        &args(&["implement the feature and add tests to verify it"]),
        &RunResolutionContext::default(),
    )?;
    assert!(
        plain.phases.is_empty(),
        "a non-offline plain task must not engage the fallback"
    );
    Ok(())
}

#[test]
fn shipped_binary_offline_plans_and_non_offline_stays_enrollment_gated() -> TestResult {
    let bin = env!("CARGO_BIN_EXE_orchestrator");

    // Offline: compiles + renders a fallback plan and exits 0 (no dispatch —
    // the shipped binary enrolls no local executor).
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

    // Non-offline, non-dry-run: still refused at the enrollment gate.
    let gated = Command::new(bin)
        .args(["run", "research the topic and write findings"])
        .output()?;
    assert!(
        !gated.status.success(),
        "a non-offline run must stay enrollment-gated"
    );
    let stderr = String::from_utf8_lossy(&gated.stderr);
    assert!(
        stderr.contains("not enrolled"),
        "the refusal must be the enrollment gate, got: {stderr}"
    );
    Ok(())
}
