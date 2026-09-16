//! Fixture-only composition for one recoverable sequential attempt.

use crate::{
    DurableAttemptOutcome, FixtureAttemptRun, FixtureProcessService, FixtureProjectionFault,
    LifecycleCoordinator, LifecycleError, OwnedChildState, WorkspaceAuthority,
    runtime_store::{PrivateProcessLedgerStore, RuntimeStoreError, TerminalDecisionRecord},
};
use orchestrator_core::{
    MissionState, MissionStatus, PhaseId, PhaseStatus, ReducerTransition, VerificationAction,
    VerificationDecision,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchError, EffectBudget,
    EffectReceipt, EffectRequest, EffectService, EffectServiceError, EffectServiceErrorKind,
    ExecutionContext, ExecutionRequest, ExecutorRegistry, MechanicalTermination, ResolvedExecutor,
    RuntimeCap, RuntimeRegistryError, ServiceContractError, WatchdogDecision, WatchdogPolicy,
    WorkerEventError, WorkerEventKind, WorkerIdentity,
};
use serde_json::{Value, json};
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    fmt,
    time::{Duration, Instant},
};
use thiserror::Error;

/// Fail-closed composition errors for one fixture attempt.
#[derive(Debug, Error)]
pub enum FixtureSequentialRunError {
    /// The supplied reducer state is not the one-phase plan supported by this slice.
    #[error("fixture sequential execution requires exactly one phase")]
    ExactlyOnePhaseRequired,
    /// Dependency scheduling is outside this one-phase fixture composition.
    #[error("fixture sequential execution does not support phase dependencies")]
    DependenciesUnsupported,
    /// The request, workspace, phase, runtime, or process root is not the same binding.
    #[error("fixture sequential execution bindings do not match")]
    BindingMismatch,
    /// The attempt deadline cannot be represented by the monotonic clock.
    #[error("fixture sequential execution deadline is out of range")]
    DeadlineOutOfRange,
    /// A new attempt cannot be admitted without any execution time.
    #[error("fixture sequential execution timeout must be non-zero")]
    ZeroTimeout,
    /// Cancellation observed before lifecycle admission must not start work.
    #[error("fixture sequential execution was cancelled before admission")]
    CancelledBeforeAdmission,
    /// Warn/continue cannot satisfy this fixture's required terminal gate.
    #[error("fixture terminal execution requires a blocking verification decision")]
    UnsupportedVerificationPolicy,
    /// A reopened worker terminal lacks durable typed terminal-decision authority.
    #[error("durable fixture attempt has no typed terminal-decision authority")]
    AmbiguousDurableTerminalDecision,
    /// Terminal lifecycle state without the exact worker terminal is corrupt.
    #[error("fixture terminal lifecycle state is missing its durable attempt")]
    DurableAttemptMissing,
    /// Durable lifecycle state conflicts with the injected structured decision.
    #[error("fixture terminal lifecycle state conflicts with the verification decision")]
    TerminalDecisionConflict,
    /// Constructing a fail-closed service failed validation.
    #[error(transparent)]
    ServiceContract(#[from] ServiceContractError),
    /// Runtime resolution failed before any worker event was emitted.
    #[error(transparent)]
    RuntimeRegistry(#[from] RuntimeRegistryError),
    /// Worker identity validation failed before durable attempt admission.
    #[error(transparent)]
    WorkerEvent(#[from] WorkerEventError),
    /// Durable lifecycle admission or recovery failed.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    /// The already-bound executor rejected dispatch.
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    /// Durable terminal-decision persistence or recovery failed (Cell 2F).
    #[error(transparent)]
    RuntimeStore(#[from] RuntimeStoreError),
}

/// How one terminal engine call obtained its durable result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureTerminalDisposition {
    /// This call admitted and invoked the executor.
    Executed,
    /// This call completed lifecycle transitions around an existing attempt.
    Recovered,
    /// The attempt and enclosing lifecycle were already terminal.
    Replayed,
}

enum FixtureTerminalAttempt {
    Executed(Box<AttemptOutcome>),
    Durable(Box<DurableAttemptOutcome>),
}

/// Safe terminal state plus the attempt evidence retained by the fixture engine.
///
/// Newly executed calls retain their complete typed [`AttemptOutcome`].
/// Recovery and replay calls retain the exact durable terminal worker record;
/// the current worker-event schema does not persist complete attempt evidence.
pub struct FixtureTerminalRun {
    disposition: FixtureTerminalDisposition,
    mission_status: MissionStatus,
    phase_status: PhaseStatus,
    attempt: FixtureTerminalAttempt,
}

impl FixtureTerminalRun {
    fn executed(
        mission_status: MissionStatus,
        phase_status: PhaseStatus,
        outcome: AttemptOutcome,
    ) -> Self {
        Self {
            disposition: FixtureTerminalDisposition::Executed,
            mission_status,
            phase_status,
            attempt: FixtureTerminalAttempt::Executed(Box::new(outcome)),
        }
    }

    fn durable(
        disposition: FixtureTerminalDisposition,
        mission_status: MissionStatus,
        phase_status: PhaseStatus,
        outcome: DurableAttemptOutcome,
    ) -> Self {
        Self {
            disposition,
            mission_status,
            phase_status,
            attempt: FixtureTerminalAttempt::Durable(Box::new(outcome)),
        }
    }

    /// Reports whether this call executed, recovered, or replayed work.
    #[must_use]
    pub const fn disposition(&self) -> FixtureTerminalDisposition {
        self.disposition
    }

    /// Returns the safe terminal mission status.
    #[must_use]
    pub const fn mission_status(&self) -> MissionStatus {
        self.mission_status
    }

    /// Returns the safe terminal phase status.
    #[must_use]
    pub const fn phase_status(&self) -> PhaseStatus {
        self.phase_status
    }

    /// Returns the newly executed typed outcome, when this call invoked the executor.
    #[must_use]
    pub const fn executed_outcome(&self) -> Option<&AttemptOutcome> {
        match &self.attempt {
            FixtureTerminalAttempt::Executed(outcome) => Some(outcome),
            FixtureTerminalAttempt::Durable(_) => None,
        }
    }

    /// Returns evidence retained by a newly executed outcome.
    #[must_use]
    pub fn evidence(&self) -> Option<&AttemptEvidence> {
        self.executed_outcome().map(AttemptOutcome::evidence)
    }

    /// Returns the exact durable terminal attempt for recovery or replay.
    #[must_use]
    pub const fn durable_attempt(&self) -> Option<&DurableAttemptOutcome> {
        match &self.attempt {
            FixtureTerminalAttempt::Executed(_) => None,
            FixtureTerminalAttempt::Durable(outcome) => Some(outcome),
        }
    }

    /// Consumes the result and returns a newly executed outcome.
    #[must_use]
    pub fn into_executed_outcome(self) -> Option<AttemptOutcome> {
        match self.attempt {
            FixtureTerminalAttempt::Executed(outcome) => Some(*outcome),
            FixtureTerminalAttempt::Durable(_) => None,
        }
    }
}

impl fmt::Debug for FixtureTerminalRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let attempt_kind = match &self.attempt {
            FixtureTerminalAttempt::Executed(_) => "executed",
            FixtureTerminalAttempt::Durable(_) => "durable",
        };
        formatter
            .debug_struct("FixtureTerminalRun")
            .field("disposition", &self.disposition)
            .field("mission_status", &self.mission_status)
            .field("phase_status", &self.phase_status)
            .field("attempt_kind", &attempt_kind)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureTerminalPlan {
    Completed,
    Failed { phase_error: &'static str },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedAttempt {
    termination: Option<MechanicalTermination>,
}

const VERIFICATION_FAILED: &str = "required verification gate did not pass";
const HARD_DEADLINE_FAILED: &str = "attempt hard deadline exceeded";
const WATCHDOG_FAILED: &str = "attempt watchdog stalled";
const PROCESS_FAILED: &str = "attempt process exited unsuccessfully";
const PROVIDER_STREAM_FAILED: &str = "attempt provider stream ended";
const SUPERVISOR_FAILED: &str = "attempt supervisor failed";
const EVENT_DELIVERY_FAILED: &str = "attempt event delivery failed";
const CONTRACT_FAILED: &str = "attempt contract was violated";
const CANCELLED_REASON: &str = "fixture terminal execution cancelled";

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct FixedWatchdog {
    stall_window: Duration,
}

impl WatchdogPolicy for FixedWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity
                .checked_add(self.stall_window)
                .unwrap_or(last_activity),
        }
    }

    fn stall_window(&self) -> Duration {
        self.stall_window
    }
}

struct DeniedEffects {
    denied: EffectServiceError,
}

impl DeniedEffects {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            denied: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "fixture sequential execution does not admit external effects",
            )?,
        })
    }
}

impl EffectService for DeniedEffects {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.denied.clone())
    }
}

/// Fixture-only services retained by the durable lifecycle coordinator.
///
/// The process service has no production constructor, while the built-in
/// effect service rejects every external effect. Concrete executors are still
/// injected through the v2 registry so this type can exercise the real
/// dispatch contract without enrolling a live provider.
pub struct FixtureSequentialRuntime {
    process: FixtureProcessService,
    executors: ExecutorRegistry,
    clock: SystemClock,
    watchdog: FixedWatchdog,
    effects: DeniedEffects,
    #[cfg(feature = "test-support")]
    force_unresolved_children: AtomicBool,
}

impl FixtureSequentialRuntime {
    /// Creates a fixture runtime around one already-enrolled helper service.
    pub fn new(
        process: FixtureProcessService,
        executors: ExecutorRegistry,
        stall_window: Duration,
    ) -> Result<Self, FixtureSequentialRunError> {
        Ok(Self {
            process,
            executors,
            clock: SystemClock,
            watchdog: FixedWatchdog { stall_window },
            effects: DeniedEffects::new()?,
            #[cfg(feature = "test-support")]
            force_unresolved_children: AtomicBool::new(false),
        })
    }

    /// Forces the fixture terminal boundary to observe unresolved ownership.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_forced_unresolved_children_for_test(mut self) -> Self {
        self.force_unresolved_children = AtomicBool::new(true);
        self
    }

    /// Clears a test-only unresolved-ownership observation.
    #[cfg(feature = "test-support")]
    pub fn clear_forced_unresolved_children_for_test(&self) {
        self.force_unresolved_children
            .store(false, Ordering::Release);
    }

    fn resolve(&self, requested_runtime: &str) -> Result<ResolvedExecutor, RuntimeRegistryError> {
        self.executors.resolve(requested_runtime)
    }

    fn deadline_after(&self, duration: Duration) -> Option<Instant> {
        self.clock.now().checked_add(duration)
    }
}

impl fmt::Debug for FixtureSequentialRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureSequentialRuntime")
            .field("kind", &"fixture-only")
            .field("has_unresolved_children", &self.has_unresolved_children())
            .finish()
    }
}

impl OwnedChildState for FixtureSequentialRuntime {
    fn has_unresolved_children(&self) -> bool {
        if self.process.has_unresolved_processes() {
            return true;
        }
        #[cfg(feature = "test-support")]
        {
            self.force_unresolved_children.load(Ordering::Acquire)
        }
        #[cfg(not(feature = "test-support"))]
        {
            false
        }
    }
}

/// One validated request bound to a durable, one-phase fixture workspace.
///
/// Calling [`Self::run_attempt`] again replays a durable terminal worker event
/// without invoking the executor. Reopening the workspace with a fresh runner
/// provides the same behavior after process restart. [`Self::run_to_terminal`]
/// adds the smallest required-gate lifecycle engine without changing the
/// attempt API.
pub struct FixtureSequentialRun {
    coordinator: LifecycleCoordinator<FixtureSequentialRuntime>,
    phase: PhaseId,
    identity: WorkerIdentity,
    request: ExecutionRequest,
    requested_runtime: String,
    timeout: Duration,
    verbose: bool,
    pending_terminal: Option<(FixtureTerminalPlan, VerificationDecision)>,
    observed_attempt: Option<ObservedAttempt>,
}

impl FixtureSequentialRun {
    /// Binds an exact request to an existing or newly-created fixture workspace.
    #[expect(
        clippy::too_many_arguments,
        reason = "all authority and identity bindings remain explicit"
    )]
    pub fn new(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        runtime: FixtureSequentialRuntime,
        requested_runtime: &str,
        request: ExecutionRequest,
        worker_id: impl Into<String>,
        timeout: Duration,
        verbose: bool,
    ) -> Result<Self, FixtureSequentialRunError> {
        let (phase, dependencies) = {
            let mut phases = initial_state.phases();
            let phase = phases
                .next()
                .ok_or(FixtureSequentialRunError::ExactlyOnePhaseRequired)?;
            let phase_id = phase.id.clone();
            let dependencies = phase
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if phases.next().is_some() {
                return Err(FixtureSequentialRunError::ExactlyOnePhaseRequired);
            }
            (phase_id, dependencies)
        };
        if !dependencies.is_empty() || !request.dependencies().is_empty() {
            return Err(FixtureSequentialRunError::DependenciesUnsupported);
        }

        let identity = WorkerIdentity::new(request.mission(), request.phase(), worker_id.into())?;
        if initial_state.mission_id().as_str() != request.mission()
            || workspace.mission_id() != initial_state.mission_id()
            || phase.as_str() != request.phase()
            || dependencies != request.dependencies()
            || runtime.process.working_root() != request.worker_dir()
        {
            return Err(FixtureSequentialRunError::BindingMismatch);
        }

        let coordinator = LifecycleCoordinator::new(workspace, runtime, initial_state)?;
        Ok(Self {
            coordinator,
            phase,
            identity,
            request,
            requested_runtime: requested_runtime.to_owned(),
            timeout,
            verbose,
            pending_terminal: None,
            observed_attempt: None,
        })
    }

    /// Executes once or returns the exact durable terminal replay.
    pub fn run_attempt(
        &mut self,
    ) -> Result<FixtureAttemptRun<AttemptOutcome>, FixtureSequentialRunError> {
        let _repaired = self.coordinator.repair_pending_admission_transition()?;
        if let Some(outcome) = self.coordinator.durable_attempt_replay(
            &self.phase,
            self.identity.worker_id(),
            self.request.attempt(),
        )? {
            return Ok(FixtureAttemptRun::Replayed(Box::new(outcome)));
        }
        let (executor, deadline) = {
            let runtime = self.coordinator.registry()?;
            let executor = runtime.resolve(&self.requested_runtime)?;
            if executor.effective_runtime() != self.request.runtime()
                || self.request.resume_from().is_some()
                    && executor
                        .descriptor()
                        .is_some_and(|descriptor| !descriptor.supports(RuntimeCap::SessionResume))
            {
                return Err(FixtureSequentialRunError::BindingMismatch);
            }
            if self.timeout.is_zero() {
                return Err(FixtureSequentialRunError::ZeroTimeout);
            }
            if runtime.process.is_cancelled() {
                return Err(FixtureSequentialRunError::CancelledBeforeAdmission);
            }
            let deadline = runtime
                .deadline_after(self.timeout)
                .ok_or(FixtureSequentialRunError::DeadlineOutOfRange)?;
            (executor, deadline)
        };
        self.ensure_phase_running()?;
        let phase = self.phase.clone();
        let worker_id = self.identity.worker_id().to_owned();
        let identity = self.identity.clone();
        let request = &self.request;
        let verbose = self.verbose;

        let run = self.coordinator.with_attempt(
            &phase,
            &worker_id,
            request.attempt(),
            |runtime, sink| {
                let mut context = ExecutionContext::new(
                    &runtime.process,
                    &runtime.clock,
                    &runtime.watchdog,
                    &runtime.effects,
                    sink,
                    identity,
                    deadline,
                )
                .with_verbose(verbose);
                executor.execute(request, &mut context)
            },
        )?;

        match run {
            FixtureAttemptRun::Executed(outcome) => {
                let outcome = outcome?;
                self.observed_attempt = Some(ObservedAttempt {
                    termination: outcome.termination(),
                });
                Ok(FixtureAttemptRun::Executed(outcome))
            }
            FixtureAttemptRun::Replayed(outcome) => Ok(FixtureAttemptRun::Replayed(outcome)),
        }
    }

    /// Drives one fixture attempt and its enclosing one-phase lifecycle terminal.
    ///
    /// Only a structured PASS/continue, non-PASS/block, or cancelled decision
    /// can admit fresh execution. Warn/continue is rejected before any fresh
    /// lifecycle mutation because this fixture models a required gate. Existing
    /// durable lifecycle state is repaired or replayed before that policy is
    /// evaluated. A newly completed attempt passes only with PASS; a completed
    /// cancelled decision cancels the mission.
    /// Incomplete attempts are classified solely from typed
    /// [`MechanicalTermination`]: cancellation emits one `mission.cancelled`
    /// transition, which atomically leaves the running phase skipped, while
    /// every other termination fails the phase and mission.
    ///
    /// A worker terminal observed by this runner can finish from its ephemeral
    /// typed outcome and the injected fixture-authoritative decision. After
    /// reopen, a still-running phase with any worker terminal is ambiguous:
    /// current worker bytes persist neither the selected verification decision
    /// nor typed mechanical termination. Durable phase and mission terminals
    /// remain recoverable, and human error text is never interpreted as
    /// authority. [`Self::run_to_terminal_recoverable`] (Cell 2F) durably
    /// persists both typed facts so that phase-running reopen gap can be
    /// recovered safely when a decision store is attached; this method alone
    /// keeps the original, store-free, ephemeral-only behavior byte for byte.
    pub fn run_to_terminal(
        &mut self,
        verification: VerificationDecision,
    ) -> Result<FixtureTerminalRun, FixtureSequentialRunError> {
        self.run_to_terminal_inner(verification, None)
    }

    /// Identical to [`Self::run_to_terminal`], but durably persists Cell 1's
    /// first-selected terminal/verification decision to `store` at the exact
    /// moment it is chosen (Cell 2F), and — when this exact
    /// mission/phase/worker/attempt binding already has a persisted decision
    /// — resumes terminalization from that decision instead of the
    /// fail-closed `AmbiguousDurableTerminalDecision` refusal a fresh runner
    /// would otherwise hit.
    ///
    /// The caller's `verification` argument is entirely ignored whenever a
    /// persisted decision for this exact binding already exists; a pre-crash
    /// BLOCK/CANCEL selection can never be silently promoted to PASS (or
    /// vice versa) by a later caller. It is used only to select a first
    /// decision, or to repair a pending non-worker lifecycle transition.
    ///
    /// This preserves the exact safe policy ordering unchanged: repair a
    /// pending lifecycle transition, honor a durable mission terminal, honor
    /// a durable phase terminal, then inspect a durable worker terminal
    /// (now persisted-decision aware) before any Warn refusal. Only that
    /// worker-terminal step's *content* changes; its position in the
    /// ordering does not move.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "staged recoverable entry point exercised only by tests until \
                      the hermetic CLI canary (Cell 4) wires a production caller \
                      for FixtureSequentialRun"
        )
    )]
    pub(crate) fn run_to_terminal_recoverable(
        &mut self,
        verification: VerificationDecision,
        store: &mut PrivateProcessLedgerStore,
    ) -> Result<FixtureTerminalRun, FixtureSequentialRunError> {
        self.run_to_terminal_inner(verification, Some(store))
    }

    fn run_to_terminal_inner(
        &mut self,
        verification: VerificationDecision,
        store: Option<&mut PrivateProcessLedgerStore>,
    ) -> Result<FixtureTerminalRun, FixtureSequentialRunError> {
        let repair_verification = self
            .pending_terminal
            .map_or(verification, |(_, selected)| selected);
        let repaired = self
            .coordinator
            .repair_pending_lifecycle_transition(Some(repair_verification))?;

        if self.coordinator.state().status().is_terminal() {
            let durable = self.require_durable_attempt()?;
            let disposition = if repaired {
                FixtureTerminalDisposition::Recovered
            } else {
                FixtureTerminalDisposition::Replayed
            };
            self.pending_terminal = None;
            self.observed_attempt = None;
            return self.durable_terminal_result(disposition, durable);
        }

        let phase_status = self.phase_status()?;
        if phase_status.is_terminal() {
            let durable = self.require_durable_attempt()?;
            let plan = terminal_plan_from_durable_phase(
                phase_status,
                durable.terminal().kind(),
                self.pending_terminal.map(|(plan, _)| plan),
            )?;
            self.finish_mission(plan)?;
            self.pending_terminal = None;
            self.observed_attempt = None;
            return self.durable_terminal_result(FixtureTerminalDisposition::Recovered, durable);
        }

        if let Some(durable) = self.coordinator.durable_attempt_replay(
            &self.phase,
            self.identity.worker_id(),
            self.request.attempt(),
        )? {
            return self.finish_durable_running_attempt(durable, verification, store);
        }

        validate_terminal_verification(verification)?;
        match self.run_attempt()? {
            FixtureAttemptRun::Executed(outcome) => {
                let plan = terminal_plan_for_outcome(&outcome, verification);
                self.persist_pending_terminal_decision(store, outcome.termination(), verification)?;
                self.pending_terminal = Some((plan, verification));
                self.finish_running_phase(plan, verification)?;
                let mission_status = self.coordinator.state().status();
                let phase_status = self.phase_status()?;
                self.pending_terminal = None;
                self.observed_attempt = None;
                Ok(FixtureTerminalRun::executed(
                    mission_status,
                    phase_status,
                    outcome,
                ))
            }
            FixtureAttemptRun::Replayed(durable) => {
                self.finish_durable_running_attempt(*durable, verification, store)
            }
        }
    }

    /// Reports whether any admitted fixture process group still needs cleanup.
    pub fn has_unresolved_children(&self) -> Result<bool, LifecycleError> {
        Ok(self.coordinator.registry()?.has_unresolved_children())
    }

    /// Stops the next fixture projection at one deterministic durable boundary.
    pub fn inject_fixture_projection_fault_once(&mut self, fault: FixtureProjectionFault) {
        self.coordinator.inject_fixture_projection_fault_once(fault);
    }

    /// Clears the test-only unresolved-child observation retained by the runtime.
    #[cfg(feature = "test-support")]
    pub fn clear_forced_unresolved_children_for_test(&self) -> Result<(), LifecycleError> {
        self.coordinator
            .registry()?
            .clear_forced_unresolved_children_for_test();
        Ok(())
    }

    fn ensure_phase_running(&mut self) -> Result<(), LifecycleError> {
        if self.coordinator.state().status() == MissionStatus::NotStarted {
            self.coordinator.transition_allocated(
                None,
                ReducerTransition::MissionStarted,
                Value::Null,
                None,
            )?;
        }
        let phase_status = self
            .coordinator
            .state()
            .phase(&self.phase)
            .map(|phase| phase.status);
        if self.coordinator.state().status() == MissionStatus::InProgress
            && phase_status == Some(PhaseStatus::Pending)
        {
            self.coordinator.transition_allocated(
                Some(self.phase.clone()),
                ReducerTransition::PhaseStarted,
                Value::Null,
                None,
            )?;
        }
        Ok(())
    }

    fn require_durable_attempt(&self) -> Result<DurableAttemptOutcome, FixtureSequentialRunError> {
        self.coordinator
            .durable_attempt_replay(
                &self.phase,
                self.identity.worker_id(),
                self.request.attempt(),
            )?
            .ok_or(FixtureSequentialRunError::DurableAttemptMissing)
    }

    fn phase_status(&self) -> Result<PhaseStatus, FixtureSequentialRunError> {
        self.coordinator
            .state()
            .phase(&self.phase)
            .map(|phase| phase.status)
            .ok_or(FixtureSequentialRunError::BindingMismatch)
    }

    fn finish_running_phase(
        &mut self,
        plan: FixtureTerminalPlan,
        verification: VerificationDecision,
    ) -> Result<(), FixtureSequentialRunError> {
        match plan {
            FixtureTerminalPlan::Completed => {
                self.coordinator.transition_allocated(
                    Some(self.phase.clone()),
                    ReducerTransition::PhaseCompleted,
                    Value::Null,
                    Some(verification),
                )?;
                self.finish_mission(plan)
            }
            FixtureTerminalPlan::Failed { phase_error } => {
                self.coordinator.transition_allocated(
                    Some(self.phase.clone()),
                    ReducerTransition::PhaseFailed {
                        error: phase_error.to_owned(),
                    },
                    json!({"error": phase_error}),
                    None,
                )?;
                self.finish_mission(plan)
            }
            FixtureTerminalPlan::Cancelled => self.finish_mission(plan),
        }
    }

    fn finish_durable_running_attempt(
        &mut self,
        durable: DurableAttemptOutcome,
        verification: VerificationDecision,
        store: Option<&mut PrivateProcessLedgerStore>,
    ) -> Result<FixtureTerminalRun, FixtureSequentialRunError> {
        let (plan, effective_verification) = match self.pending_terminal {
            Some(selected) => selected,
            None => match self
                .recovered_terminal_decision(store.as_deref(), durable.terminal().kind())?
            {
                Some(recovered) => recovered,
                None => {
                    let observed = self
                        .observed_attempt
                        .ok_or(FixtureSequentialRunError::AmbiguousDurableTerminalDecision)?;
                    if !worker_kind_matches_observation(durable.terminal().kind(), observed) {
                        return Err(FixtureSequentialRunError::TerminalDecisionConflict);
                    }
                    let selected = (
                        terminal_plan_for_observation(observed, verification)?,
                        verification,
                    );
                    self.persist_pending_terminal_decision(
                        store,
                        observed.termination,
                        verification,
                    )?;
                    selected
                }
            },
        };
        self.pending_terminal = Some((plan, effective_verification));
        self.finish_running_phase(plan, effective_verification)?;
        self.pending_terminal = None;
        self.observed_attempt = None;
        self.durable_terminal_result(FixtureTerminalDisposition::Recovered, durable)
    }

    /// Looks up a durably persisted terminal decision (Cell 2F) bound to
    /// this exact mission/phase/worker/attempt, when a decision store is
    /// attached. A persisted decision's typed observed termination is
    /// cross-checked against the worker-event kind that is actually durable
    /// on disk for this attempt; a mismatch is a typed conflict, never a
    /// silent substitution.
    fn recovered_terminal_decision(
        &self,
        store: Option<&PrivateProcessLedgerStore>,
        worker_kind: WorkerEventKind,
    ) -> Result<Option<(FixtureTerminalPlan, VerificationDecision)>, FixtureSequentialRunError>
    {
        let Some(store) = store else {
            return Ok(None);
        };
        let Some(record) = store.terminal_decision(
            self.coordinator.state().mission_id(),
            self.phase.as_str(),
            self.identity.worker_id(),
            self.request.attempt(),
        )?
        else {
            return Ok(None);
        };
        let observed = ObservedAttempt {
            termination: record.observed_termination(),
        };
        if !worker_kind_matches_observation(worker_kind, observed) {
            return Err(FixtureSequentialRunError::TerminalDecisionConflict);
        }
        let plan = terminal_plan_for_persisted_decision(
            record.observed_termination(),
            record.verification(),
        );
        Ok(Some((plan, record.verification())))
    }

    /// Durably persists Cell 1's first-selected terminal decision (Cell 2F)
    /// when a decision store is attached; a no-op otherwise, which keeps
    /// every existing store-free caller byte-for-byte unchanged.
    fn persist_pending_terminal_decision(
        &self,
        store: Option<&mut PrivateProcessLedgerStore>,
        observed_termination: Option<MechanicalTermination>,
        verification: VerificationDecision,
    ) -> Result<(), FixtureSequentialRunError> {
        let Some(store) = store else {
            return Ok(());
        };
        let record = TerminalDecisionRecord::new(
            self.coordinator.state().mission_id(),
            self.phase.as_str(),
            self.identity.worker_id(),
            self.request.attempt(),
            observed_termination,
            verification,
        )?;
        store.record_terminal_decision(&record)?;
        Ok(())
    }

    fn finish_mission(
        &mut self,
        plan: FixtureTerminalPlan,
    ) -> Result<(), FixtureSequentialRunError> {
        let (transition, data) = match plan {
            FixtureTerminalPlan::Completed => (ReducerTransition::MissionCompleted, Value::Null),
            FixtureTerminalPlan::Failed { .. } => (ReducerTransition::MissionFailed, Value::Null),
            FixtureTerminalPlan::Cancelled => (
                ReducerTransition::MissionCancelled {
                    reason: CANCELLED_REASON.to_owned(),
                },
                json!({"reason": CANCELLED_REASON}),
            ),
        };
        self.coordinator
            .transition_allocated(None, transition, data, None)?;
        Ok(())
    }

    fn durable_terminal_result(
        &self,
        disposition: FixtureTerminalDisposition,
        outcome: DurableAttemptOutcome,
    ) -> Result<FixtureTerminalRun, FixtureSequentialRunError> {
        Ok(FixtureTerminalRun::durable(
            disposition,
            self.coordinator.state().status(),
            self.phase_status()?,
            outcome,
        ))
    }
}

fn validate_terminal_verification(
    verification: VerificationDecision,
) -> Result<(), FixtureSequentialRunError> {
    if !verification.gate_passed() && verification.action() == VerificationAction::Continue {
        Err(FixtureSequentialRunError::UnsupportedVerificationPolicy)
    } else {
        Ok(())
    }
}

const fn completed_terminal_plan(verification: VerificationDecision) -> FixtureTerminalPlan {
    if verification.gate_passed() {
        FixtureTerminalPlan::Completed
    } else if matches!(verification.action(), VerificationAction::Cancelled) {
        FixtureTerminalPlan::Cancelled
    } else {
        FixtureTerminalPlan::Failed {
            phase_error: VERIFICATION_FAILED,
        }
    }
}

fn terminal_plan_for_outcome(
    outcome: &AttemptOutcome,
    verification: VerificationDecision,
) -> FixtureTerminalPlan {
    match outcome.termination() {
        None => completed_terminal_plan(verification),
        Some(MechanicalTermination::Cancelled) => FixtureTerminalPlan::Cancelled,
        Some(termination) => FixtureTerminalPlan::Failed {
            phase_error: mechanical_failure_reason(termination),
        },
    }
}

/// Rebuilds the exact terminal plan a persisted Cell 2F decision selected,
/// without re-validating the caller's verification argument: a persisted
/// decision was already validated at the moment it was first selected (see
/// [`FixtureSequentialRun::persist_pending_terminal_decision`]), so recovery
/// only needs to replay the same deterministic mapping
/// [`terminal_plan_for_outcome`] uses from a typed observed termination.
const fn terminal_plan_for_persisted_decision(
    observed_termination: Option<MechanicalTermination>,
    verification: VerificationDecision,
) -> FixtureTerminalPlan {
    match observed_termination {
        None => completed_terminal_plan(verification),
        Some(MechanicalTermination::Cancelled) => FixtureTerminalPlan::Cancelled,
        Some(termination) => FixtureTerminalPlan::Failed {
            phase_error: mechanical_failure_reason(termination),
        },
    }
}

fn terminal_plan_for_observation(
    observed: ObservedAttempt,
    verification: VerificationDecision,
) -> Result<FixtureTerminalPlan, FixtureSequentialRunError> {
    match observed.termination {
        None => {
            validate_terminal_verification(verification)?;
            Ok(completed_terminal_plan(verification))
        }
        Some(MechanicalTermination::Cancelled) => Ok(FixtureTerminalPlan::Cancelled),
        Some(termination) => Ok(FixtureTerminalPlan::Failed {
            phase_error: mechanical_failure_reason(termination),
        }),
    }
}

const fn worker_kind_matches_observation(kind: WorkerEventKind, observed: ObservedAttempt) -> bool {
    matches!(
        (kind, observed.termination),
        (WorkerEventKind::Completed, None) | (WorkerEventKind::Failed, Some(_))
    )
}

pub(crate) const fn mechanical_failure_reason(termination: MechanicalTermination) -> &'static str {
    match termination {
        MechanicalTermination::Cancelled => CANCELLED_REASON,
        MechanicalTermination::HardDeadlineExceeded => HARD_DEADLINE_FAILED,
        MechanicalTermination::WatchdogStalled => WATCHDOG_FAILED,
        MechanicalTermination::ProcessExited(_) => PROCESS_FAILED,
        MechanicalTermination::ProviderStreamEnded => PROVIDER_STREAM_FAILED,
        MechanicalTermination::SupervisorFailure => SUPERVISOR_FAILED,
        MechanicalTermination::EventDeliveryFailure => EVENT_DELIVERY_FAILED,
        MechanicalTermination::ContractViolation => CONTRACT_FAILED,
    }
}

const fn terminal_plan_matches_phase(plan: FixtureTerminalPlan, status: PhaseStatus) -> bool {
    matches!(
        (plan, status),
        (FixtureTerminalPlan::Completed, PhaseStatus::Completed)
            | (FixtureTerminalPlan::Failed { .. }, PhaseStatus::Failed)
    )
}

fn terminal_plan_from_durable_phase(
    status: PhaseStatus,
    worker_kind: WorkerEventKind,
    pending: Option<FixtureTerminalPlan>,
) -> Result<FixtureTerminalPlan, FixtureSequentialRunError> {
    let plan = match status {
        PhaseStatus::Completed if worker_kind == WorkerEventKind::Completed => {
            FixtureTerminalPlan::Completed
        }
        PhaseStatus::Failed => FixtureTerminalPlan::Failed {
            phase_error: CONTRACT_FAILED,
        },
        PhaseStatus::Pending | PhaseStatus::Running | PhaseStatus::Skipped => {
            return Err(FixtureSequentialRunError::TerminalDecisionConflict);
        }
        PhaseStatus::Completed => {
            return Err(FixtureSequentialRunError::TerminalDecisionConflict);
        }
    };
    if pending.is_some_and(|pending| !terminal_plan_matches_phase(pending, status)) {
        return Err(FixtureSequentialRunError::TerminalDecisionConflict);
    }
    Ok(plan)
}

impl fmt::Debug for FixtureSequentialRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureSequentialRun")
            .field("kind", &"recoverable-fixture-attempt")
            .field("phase", &self.phase)
            .field("attempt", &self.request.attempt())
            .finish_non_exhaustive()
    }
}

/// Cell 2F: engine-level tests proving `run_to_terminal_recoverable` durably
/// persists and later recovers Cell 1's first-selected terminal decision.
///
/// Everything here is hermetic and hand-built (no live home, DB, or
/// provider). Unlike `tests/fixture_sequential_run.rs`, this is a unit-test
/// module compiled into the library's own `--lib` test binary, so
/// `CARGO_BIN_EXE_<name>` is not available (Cargo only sets it for
/// integration tests) — the fixture "executable" is therefore a small
/// synthetic byte string carrying a valid Mach-O/ELF magic prefix (the only
/// thing `FixtureAdmissionPolicy`/`FixtureProcessAuthority` eagerly check)
/// rather than the real compiled `orchestrator-owned-process-fixture`
/// helper. It is never actually spawned: every executor below only emits
/// synthetic worker output and never calls `context.run_process(..)`.
#[cfg(test)]
mod cell2f_recoverable_tests {
    use super::*;
    use crate::runtime_store::{PrivateProcessLedgerBoundary, RuntimeStore};
    use crate::{
        CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureWorkspaceSeed,
        FreshFixtureAuthority, IsolatedFixtureRoot, ProductionBoundary, StorageActorAuthority,
        SupervisorLimits,
    };
    use orchestrator_core::{
        CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, PhaseDefinition,
        VerificationClass, VerificationMode, VerificationOutcome, decide_verification,
    };
    use orchestrator_exec::{
        DispatchRequest, Effort, ExecutionRequestDraft, PartialWork, PhaseExecutor, RuntimeCaps,
        RuntimeDescriptor, RuntimeFamily, WorkerEventPayload, WorkerOutput, WorkerOutputFields,
        WorkerOutputKind,
    };
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const MISSION: &str = "cell2f-recovery-mission";
    const PHASE: &str = "phase-1";
    const WORKER: &str = "worker-1";
    const RUNTIME: &str = "cell2f-fixture-runtime";
    const EXECUTABLE: &str = "cell2f-native-helper";
    /// Mach-O 64-bit magic (`0xfeedfacf`, little-endian byte order) plus
    /// padding: the only content `is_native_executable` inspects.
    const SYNTHETIC_EXECUTABLE: &[u8] =
        &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

    fn pass_decision() -> VerificationDecision {
        decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        )
    }

    #[cfg(feature = "test-support")]
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

    fn private_dir(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    #[cfg(feature = "test-support")]
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

    fn initial_checkpoint(state: &MissionState) -> CheckpointProjection {
        CheckpointProjection {
            workspace_id: MISSION.to_owned(),
            status: "pending".to_owned(),
            plan: Some(CheckpointPlan {
                id: "cell2f-fixture-plan".to_owned(),
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

    /// One hermetic fixture workspace plus its own separate `RuntimeStore`
    /// home. The two are deliberately independent directories: Cell 2F's
    /// decision journal is not the workspace's event/checkpoint projection.
    struct Cell2fCase {
        parent: PathBuf,
        root: PathBuf,
        authority: FreshFixtureAuthority,
        workspace: Option<WorkspaceAuthority>,
        process: Option<FixtureProcessService>,
        worker_root: PathBuf,
        store_home: PathBuf,
    }

    impl Cell2fCase {
        fn take_workspace(&mut self) -> TestResult<WorkspaceAuthority> {
            self.workspace
                .take()
                .ok_or_else(|| "cell2f fixture workspace was already consumed".into())
        }

        fn take_process(&mut self) -> TestResult<FixtureProcessService> {
            self.process
                .take()
                .ok_or_else(|| "cell2f fixture process service was already consumed".into())
        }

        fn workspace_path(&self) -> PathBuf {
            self.root.join("workspaces").join(MISSION)
        }

        /// Opens a fresh handle onto this case's own `RuntimeStore` home.
        /// Callers must `close()` it before opening another handle on the
        /// same home (single-writer lease), exactly like reopening after a
        /// restart.
        fn open_store(&self) -> TestResult<PrivateProcessLedgerStore> {
            Ok(RuntimeStore::open_private(
                PrivateProcessLedgerBoundary::new(Arc::new(
                    ProductionBoundary::from_canonical_root(&self.store_home)?,
                )),
                StorageActorAuthority::new(),
            )?)
        }
    }

    impl Drop for Cell2fCase {
        fn drop(&mut self) {
            self.process.take();
            self.workspace.take();
            let _ = std::fs::remove_dir_all(&self.parent);
        }
    }

    fn fixture(label: &str, state: &MissionState) -> TestResult<Cell2fCase> {
        let number = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-cell2f-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;
        let store_home = parent.join("runtime-store");
        private_dir(&store_home)?;
        let store_home = std::fs::canonicalize(store_home)?;
        let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
        let root = isolated.path().to_path_buf();
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
            .with_expected_fixture_helper(SYNTHETIC_EXECUTABLE);
        let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
        let workspace = authority.create_workspace(
            MissionId::new(MISSION)?,
            FixtureWorkspaceSeed::new(
                b"cell2f fixture mission\n".to_vec(),
                &initial_checkpoint(state),
                b"{}".to_vec(),
            )?,
        )?;
        let executable = authority.install_fixture_executable(EXECUTABLE, SYNTHETIC_EXECUTABLE)?;
        let limits =
            SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
        let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
        let worker_root = std::fs::canonicalize(root.join("workspaces").join(MISSION))?;
        let cancellation = CancellationToken::new();
        let process = FixtureProcessService::new(process_authority, cancellation)?;
        Ok(Cell2fCase {
            parent,
            root,
            authority,
            workspace: Some(workspace),
            process: Some(process),
            worker_root,
            store_home,
        })
    }

    fn request(worker_root: &Path) -> TestResult<ExecutionRequest> {
        Ok(ExecutionRequest::new(ExecutionRequestDraft {
            mission: MISSION.to_owned(),
            phase: PHASE.to_owned(),
            attempt: 1,
            revision: 1,
            objective: "exercise Cell 2F terminal-decision persistence".to_owned(),
            persona: "fixture".to_owned(),
            role: "implementer".to_owned(),
            domain: "dev".to_owned(),
            skills: vec!["rust-best-practices".to_owned()],
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

    fn descriptor() -> Option<RuntimeDescriptor> {
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

    fn registry(executor: Arc<dyn PhaseExecutor>) -> TestResult<ExecutorRegistry> {
        let mut registry = ExecutorRegistry::new();
        let _previous = registry.register(RUNTIME, executor)?;
        Ok(registry)
    }

    /// Emits synthetic worker output and completes; never spawns a process.
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
                chunk: Some("cell2f fixture output".to_owned()),
                event_kind: Some(WorkerOutputKind::Text),
                streaming: Some(true),
                tool_name: None,
                is_error: None,
                output_len: Some("cell2f fixture output".len()),
                duration: None,
            })
            .map(WorkerEventPayload::Output);
            if let Ok(output) = output {
                let _receipt = context.emit(&output);
            }
            AttemptOutcome::completed(
                "cell2f fixture completed",
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
            descriptor()
        }
    }

    fn runner(
        case: &mut Cell2fCase,
        state: MissionState,
        executor: Arc<dyn PhaseExecutor>,
        timeout: Duration,
    ) -> TestResult<FixtureSequentialRun> {
        let req = request(&case.worker_root)?;
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
            req,
            WORKER,
            timeout,
            false,
        )?)
    }

    /// Opens the same workspace as a genuinely fresh runner: no in-process
    /// `pending_terminal`/`observed_attempt` state survives, exactly like a
    /// process restart. The registry is deliberately empty and the
    /// requested runtime deliberately invalid, so any test relying on this
    /// helper proves recovery never needs to re-resolve or re-invoke an
    /// executor.
    fn reopened_runner(case: &Cell2fCase, state: MissionState) -> TestResult<FixtureSequentialRun> {
        let workspace = case.authority.open_workspace(MissionId::new(MISSION)?)?;
        let executable_label = format!(
            "reopen-helper-{}",
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        );
        let executable = case
            .authority
            .install_fixture_executable(&executable_label, SYNTHETIC_EXECUTABLE)?;
        let limits =
            SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
        let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
        let process = FixtureProcessService::new(process_authority, CancellationToken::new())?;
        let runtime = FixtureSequentialRuntime::new(
            process,
            ExecutorRegistry::new(),
            Duration::from_secs(30),
        )?;
        Ok(FixtureSequentialRun::new(
            workspace,
            state,
            runtime,
            "invalid runtime!",
            request(&case.worker_root)?,
            WORKER,
            Duration::ZERO,
            false,
        )?)
    }

    /// A runner whose terminal-effect boundary always observes unresolved
    /// owned children. Used only to reach, deterministically and without any
    /// filesystem fault injection, the exact durable state Cell 2F targets:
    /// the worker attempt fully durable and checkpointed (phase status
    /// cleanly "running", not merely "pending" a transition) with nothing
    /// else attempted, because `validate_effect_boundary`'s unresolved-child
    /// check runs *before* any event or checkpoint write for a terminal
    /// transition. A persisted decision therefore lands durably while the
    /// workspace-level phase/mission transition never touches disk at all —
    /// a strictly cleaner and more deterministic stand-in for "crashed
    /// between persisting the decision and finishing the phase" than a
    /// filesystem fault would be, since it needs no separate recovery pass
    /// over a half-written event.
    #[cfg(feature = "test-support")]
    fn runner_with_forced_unresolved_children(
        case: &mut Cell2fCase,
        state: MissionState,
        executor: Arc<dyn PhaseExecutor>,
        timeout: Duration,
    ) -> TestResult<FixtureSequentialRun> {
        let req = request(&case.worker_root)?;
        let runtime = FixtureSequentialRuntime::new(
            case.take_process()?,
            registry(executor)?,
            Duration::from_secs(30),
        )?
        .with_forced_unresolved_children_for_test();
        Ok(FixtureSequentialRun::new(
            case.take_workspace()?,
            state,
            runtime,
            RUNTIME,
            req,
            WORKER,
            timeout,
            false,
        )?)
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn persisted_decision_survives_close_and_reopen_and_reaches_same_terminal() -> TestResult {
        let state = one_phase_state()?;
        let mut case = fixture("persist-restart", &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner_with_forced_unresolved_children(
            &mut case,
            state.clone(),
            executor,
            Duration::from_secs(3),
        )?;
        let mut store = case.open_store()?;

        // The attempt executes and the decision is persisted before the
        // phase-completion transition is even attempted; the forced
        // unresolved-children boundary then blocks that transition cleanly,
        // touching no workspace bytes at all — this is the "crashed after
        // persisting the decision, before finishing the phase" state.
        let mission = MissionId::new(MISSION)?;
        let first_result = first.run_to_terminal_recoverable(pass_decision(), &mut store);
        assert!(matches!(
            first_result,
            Err(FixtureSequentialRunError::Lifecycle(
                LifecycleError::UnresolvedChildren
            ))
        ));
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        let persisted = store
            .terminal_decision(&mission, PHASE, WORKER, 1)?
            .ok_or("decision was not persisted before the blocked phase transition")?;
        assert_eq!(persisted.verification(), pass_decision());
        assert_eq!(persisted.observed_termination(), None);
        let event_path = case.workspace_path().join("events.jsonl");
        assert_eq!(
            event_types(&event_path)?,
            [
                "mission.started",
                "phase.started",
                "worker.spawned",
                "worker.output",
                "worker.completed"
            ]
        );
        store.close()?;
        drop(first);

        // Fresh runner, fresh store, no in-process state at all. A
        // DIFFERENT (Warn) decision is supplied here and must be ignored:
        // the persisted PASS decision from above is what actually resumes.
        let mut second = reopened_runner(&case, state)?;
        let mut reopened_store = case.open_store()?;
        let recovered = second.run_to_terminal_recoverable(warn_decision(), &mut reopened_store)?;
        assert_eq!(
            recovered.disposition(),
            FixtureTerminalDisposition::Recovered
        );
        assert_eq!(recovered.mission_status(), MissionStatus::Completed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Completed);
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
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
        reopened_store.close()?;
        Ok(())
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn persisted_block_decision_cannot_be_overridden_by_a_later_pass() -> TestResult {
        let state = one_phase_state()?;
        let mut case = fixture("persist-block-no-override", &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner_with_forced_unresolved_children(
            &mut case,
            state.clone(),
            executor,
            Duration::from_secs(3),
        )?;
        let mut store = case.open_store()?;

        let first_result = first.run_to_terminal_recoverable(block_decision(), &mut store);
        assert!(matches!(
            first_result,
            Err(FixtureSequentialRunError::Lifecycle(
                LifecycleError::UnresolvedChildren
            ))
        ));
        store.close()?;
        drop(first);

        let mut second = reopened_runner(&case, state)?;
        let mut reopened_store = case.open_store()?;
        // A later caller supplies PASS; the durable BLOCK selection must win.
        let recovered = second.run_to_terminal_recoverable(pass_decision(), &mut reopened_store)?;
        assert_eq!(
            recovered.disposition(),
            FixtureTerminalDisposition::Recovered
        );
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Failed);
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        reopened_store.close()?;
        Ok(())
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn persisted_pass_decision_cannot_be_overridden_by_a_later_block() -> TestResult {
        let state = one_phase_state()?;
        let mut case = fixture("persist-pass-no-override", &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner_with_forced_unresolved_children(
            &mut case,
            state.clone(),
            executor,
            Duration::from_secs(3),
        )?;
        let mut store = case.open_store()?;

        let first_result = first.run_to_terminal_recoverable(pass_decision(), &mut store);
        assert!(matches!(
            first_result,
            Err(FixtureSequentialRunError::Lifecycle(
                LifecycleError::UnresolvedChildren
            ))
        ));
        store.close()?;
        drop(first);

        let mut second = reopened_runner(&case, state)?;
        let mut reopened_store = case.open_store()?;
        // A later caller supplies BLOCK; the durable PASS selection must win.
        let recovered =
            second.run_to_terminal_recoverable(block_decision(), &mut reopened_store)?;
        assert_eq!(
            recovered.disposition(),
            FixtureTerminalDisposition::Recovered
        );
        assert_eq!(recovered.mission_status(), MissionStatus::Completed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Completed);
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        reopened_store.close()?;
        Ok(())
    }

    #[test]
    fn absent_persisted_decision_still_returns_ambiguous_durable_terminal_decision() -> TestResult {
        let state = one_phase_state()?;
        let mut case = fixture("no-persisted-decision", &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let mut first = runner(&mut case, state.clone(), executor, Duration::from_secs(3))?;
        // Only `run_attempt` — never `run_to_terminal`/`run_to_terminal_recoverable`
        // — so no decision is ever selected or persisted for this attempt,
        // exactly like genuinely pre-Cell-2F history.
        let outcome = first
            .run_attempt()?
            .into_executed()
            .ok_or("fresh attempt unexpectedly replayed")?;
        assert!(outcome.is_completed());
        drop(first);

        let mut second = reopened_runner(&case, state)?;
        let mut store = case.open_store()?;
        let result = second.run_to_terminal_recoverable(pass_decision(), &mut store);
        assert!(matches!(
            result,
            Err(FixtureSequentialRunError::AmbiguousDurableTerminalDecision)
        ));
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        store.close()?;
        Ok(())
    }

    #[test]
    fn fresh_warn_refuses_byte_nonmutatingly_before_any_persistence() -> TestResult {
        let state = one_phase_state()?;
        let mut case = fixture("warn-before-persist", &state)?;
        let invocations = Arc::new(AtomicU64::new(0));
        let executor = Arc::new(SuccessfulExecutor {
            invocations: Arc::clone(&invocations),
        });
        let workspace = case.workspace_path();
        let event_path = workspace.join("events.jsonl");
        let events_before = std::fs::read(&event_path).ok();
        let checkpoint_before = std::fs::read(workspace.join("checkpoint.json"))?;
        let mut first = runner(&mut case, state, executor, Duration::from_secs(3))?;
        let mut store = case.open_store()?;

        let result = first.run_to_terminal_recoverable(warn_decision(), &mut store);
        assert!(matches!(
            result,
            Err(FixtureSequentialRunError::UnsupportedVerificationPolicy)
        ));
        assert_eq!(invocations.load(Ordering::Relaxed), 0);
        assert_eq!(
            std::fs::read(&event_path).ok(),
            events_before,
            "Warn must refuse before any executor or lifecycle mutation"
        );
        assert_eq!(
            std::fs::read(workspace.join("checkpoint.json"))?,
            checkpoint_before
        );
        assert_eq!(
            store.terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?,
            None,
            "a refused fresh Warn must not have persisted anything either"
        );
        store.close()?;
        Ok(())
    }
}
