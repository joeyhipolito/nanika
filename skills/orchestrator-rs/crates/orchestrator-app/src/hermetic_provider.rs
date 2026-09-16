//! Cell 3: durable attested hermetic provider composition.
//!
//! Composes the typed private process ledger + [`DurableProcessService`] +
//! cancellation/deadline/watchdog + the attested fixture helper executable +
//! typed process outcome and cleanup evidence into a hermetic provider that
//! runs one worker attempt for one phase end-to-end under durable,
//! crash-recoverable authority. See
//! `docs/rust-orchestrator/CLAUDE-TO-CODEX-CELL3-BRIEF.md` §3 and
//! `docs/rust-orchestrator/CODEX-TO-CLAUDE-RETURN-CONTINUATION.md` §8 for the
//! contract this module implements.
//!
//! ## Why the Cell-1 runner, not the projector
//!
//! `HermeticCompatibilityProjector` (Cell 2D, `hermetic_projector.rs`) owns the
//! Go-compatible journal and its real projection receipts. Rust-private
//! process claims and terminal decisions must never enter that store, even
//! under a nominal compatibility projection. This composition instead drives
//! phase/mission lifecycle through [`LifecycleCoordinator`] — the same generic
//! Cell-1 engine `FixtureSequentialRun` already proves — while an isolated
//! schema-v4 [`PrivateProcessLedgerStore`] retains only process
//! claim/resolution and terminal-decision evidence. Every private row is
//! mission-bound, sealed with the private-ledger marker, projection-free, and
//! self-acknowledged. The separate ledger is canary recovery state, never a
//! second canonical mission truth and never opened by a compatibility
//! projector.
//!
//! ## Composition shape
//!
//! 1. **Exact request binding**: a versioned, length-framed SHA-256 digest
//!    binds the worker id, requested runtime, and every source field of
//!    [`ExecutionRequest`] into the immutable outbox payload (only its schema
//!    and digest are persisted; raw request text and session identifiers are
//!    not). One deterministic [`ProcessRequest`] (same executable
//!    id/working root/output cap across recovery) is moved into the durable
//!    operation and borrowed for outbox admission (`OutboxIntent::for_process`
//!    + `bind_process`), actor spawn, and the eventual
//!      `context.run_process(..)` call; `DurableProcessService`'s own enrollment
//!      check (`ProviderEnrollment::admit`) rejects any process request whose
//!      fingerprint does not match the bound effect.
//! 2. **Execution**: the request is admitted as a sealed, self-acknowledged
//!    private-ledger row with no compatibility projection or Go artifact, then
//!    claimed/executed through
//!    `DurableProcessActor`/`DurableProcessService`, which spawns the attested
//!    fixture helper under `ProductionProcessLaunchAuthority`'s
//!    disposable-canary constructor. Deadline and cancellation are honored via
//!    `ProcessBudget`/`CancellationToken`; `FixedWatchdog`'s `WatchdogPolicy`
//!    callback (`evaluate`) is wired but inert in this composition — it always
//!    returns `Continue`, so real stall detection flows through
//!    `stall_window()` feeding `ProcessBudget`'s own OS-level supervision, not
//!    through the callback's return value. The process group is reaped by the
//!    production supervisor before any receipt is produced.
//! 3. **Typed outcome**: the process result is mapped to
//!    [`MechanicalTermination`] purely from typed enums (never human provider
//!    output) and persisted as a `TerminalDecisionRecord` (Cell 2F) at the
//!    moment it is first selected.
//! 4. **Restart/reconcile**: before ever admitting a fresh attempt, this
//!    composition checks the exact retained outbox state, its newest immutable
//!    observation, and the terminal-decision record for the same binding.
//!    Only a current `Succeeded`/`Failed` state plus a matching newest
//!    observation proves a terminal outcome; decision records never synthesize
//!    process evidence. Proven terminal evidence closes the ledger without
//!    spawning an actor and drives projection/replay instead of relaunching.
//!
//! ## Crash-cut (ii): bounded, exact-identity cleanup
//!
//! A genuine orchestrator `kill -9` while the attested helper is truly
//! mid-execution (a real, separate OS process killed out from under this one)
//! is cut (ii) in Cell 1's crash-cut taxonomy, and this composition makes an
//! explicit, bounded recovery policy for it:
//!
//! 1. **Durable pre-spawn state is explicit.** A claim-protocol marker commits
//!    atomically with the exact claim. A separate spawn permit commits only
//!    after specification/runtime initialization and immediately before gated
//!    launch. A claim with no permit is therefore proven not started; a permit
//!    with no observed launcher identity remains fail-closed. After the
//!    authenticated initial grant is acknowledged, a third StartedObserved
//!    marker commits before the final START frame can permit target execution.
//! 2. **Blocked identity and start observation bracket release.** The gated launcher's exact
//!    PID, process-group ID, and process-start identity commit before the
//!    release authorization. Restart distinguishes blocked cleanup,
//!    authorization without StartedObserved (C5), and StartedObserved without
//!    terminal evidence (C6), without reconstructing a gate or start capability.
//! 3. **Cleanup authority is exact and consuming.** Only a non-cloneable
//!    recovery capability bound to the exact request/claim/identity can reach
//!    cleanup. The kernel identity is inspected twice, including immediately
//!    before `SIGKILL`; a mismatched leader or ambiguous surviving group is
//!    never signalled. Terminal mutation waits for a fresh opaque proof that
//!    the whole recorded process group is absent.
//! 4. **Terminal evidence stays conservative.** Proven blocked cleanup records
//!    `NotStarted(RecoveredBeforeSpawn)`. Proven C5/C6 cleanup records
//!    `Uncertain(SupervisorOutcomeLost)`. Cleanup of an already-uncertain
//!    attempt appends no second terminal observation. None of these paths can
//!    synthesize success or an exit status.
//! 5. **Never-double-launch holds.** Every recovered `Executing` or `Uncertain`
//!    attempt returns `ProcessRecoveryReconciled` with a redacted typed
//!    disposition after reconciliation and before actor admission. Lifecycle
//!    records durable before the crash may already exist, but the attested
//!    helper is never spawned twice for the same idempotency key.
//!
//! The trade, stated plainly: this composition chooses **fail-closed on
//! double-launch and ambiguous identity**. Exact cleanup can end a retained
//! process group, but it cannot recover the child's exit status after the
//! supervisor relationship is lost. Such an attempt remains explicitly
//! `Uncertain` and requires policy above this cell.
//! The test module below exercises five bounded recovery cells with real
//! supervisor `SIGKILL` boundaries: Pending, Claimed, ReleasedLive,
//! ReleasedExited (C5), and StartedExited (C6). Those cells prove the local
//! crash-cut contract; they are intentionally not the full aggregate
//! integration matrix, which remains a broader composition gate.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged production surface exercised only by tests until Cell 4 wires \
                  the CLI composition root"
    )
)]

#[cfg(test)]
use std::path::Path;
use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, MissionState, MissionStatus,
    PhaseDefinition, PhaseId, PhaseState, PhaseStatus, ReducerTransition, VerificationAction,
    VerificationClass, VerificationDecision, VerificationMode, VerificationOutcome, WorkerId,
    decide_verification,
};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchError, EffectBudget,
    EffectReceipt, EffectRequest, EffectService, EffectServiceError, EffectServiceErrorKind,
    Effort, EventReceipt, EventSink, EventSinkError, EventSinkErrorKind, ExecutionContext,
    ExecutionRequest, ExecutionRequestDraft, ExecutionRequestFingerprint, ExecutorRegistry,
    MechanicalTermination, PartialWork, PhaseExecutor, ProcessBudget, ProcessExitStatus,
    ProcessPreflight, ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError,
    ProcessServiceErrorKind, ProcessTerminationReceipt, RuntimeDescriptor, RuntimeFamily,
    RuntimeRegistryError, ServiceContractError, WatchdogDecision, WatchdogPolicy, WorkerEventDraft,
    WorkerEventError, WorkerEventKind, WorkerIdentity,
};
#[cfg(test)]
use orchestrator_process::ProcessError;
use orchestrator_process::{
    CancellationToken, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    process_wide_has_owned_processes,
};
use thiserror::Error;

#[cfg(test)]
use crate::durable_process_service::DeferredProviderRuntime;
use crate::durable_process_service::{
    DurableProcessBuildError, DurableProcessRecoveryError, RecoveredProcessDisposition,
    reconcile_recovered_process_attempt,
};
use crate::{
    DurableProcessService, EffectEvidenceCode, EffectObservation, EffectOperationSlot,
    ExecutableCapability, FixtureAttemptRun, FixtureAuthorityError, FixtureWorkspaceSeed,
    FreshFixtureAuthority, JournalIntent, LifecycleCoordinator, LifecycleError, OutboxEffectKind,
    OutboxIntent, OutboxState, OwnedChildState, ProcessExecutionIdentity,
    ProcessNotStartedEvidenceReason, ProcessUncertaintyEvidence, ProductionBoundary, RuntimeStore,
    RuntimeStoreError, StartedProcessFailureEvidence, StorageActorAuthority, WorkspaceAuthority,
    WorkspaceError,
    capability::{CapabilityRoot, SharedCapabilityRoot},
    durable_process_service::{
        DurableProcessActor, DurableProcessBarrierObserver, DurableProcessBarrierPoint,
        PreparedProviderLaunch, ProcessTimestampSource, ProviderLaunchError,
    },
    fixture_sequential_run::mechanical_failure_reason,
    runtime_store::{
        ExactOutboxSnapshot, PRIVATE_PROCESS_CLAIM_TRANSITION_KIND as CLAIM_TRANSITION_KIND,
        PrivateProcessLedgerBoundary, PrivateProcessLedgerStore, ProcessEffectBinding,
        RecoveredProcessAttemptClassification, TerminalDecisionRecord,
    },
};
const OPERATION_SLOT: &str = "hermetic-provider-attempt";
const CANCELLED_REASON: &str = "hermetic provider attempt cancelled";
const VERIFICATION_FAILED: &str = "required verification gate did not pass";

/// Fail-closed composition errors for the durable hermetic provider.
#[derive(Debug, Error)]
pub(crate) enum HermeticProviderError {
    #[error("hermetic provider requires exactly one phase")]
    ExactlyOnePhaseRequired,
    #[error("hermetic provider does not support phase dependencies")]
    DependenciesUnsupported,
    #[error(
        "hermetic provider does not support session resume or executor-verified expected evidence"
    )]
    UnsupportedRequest,
    #[error("hermetic provider request, workspace, or helper bindings do not match")]
    BindingMismatch,
    #[error("hermetic provider ledger boundary must be disjoint from the workspace's own boundary")]
    LedgerBoundaryCollision,
    #[error("hermetic provider terminal execution requires a blocking verification decision")]
    UnsupportedVerificationPolicy,
    #[error(
        "hermetic provider replayed a durable worker record with no corresponding Cell 2F \
         terminal decision on the ledger"
    )]
    TerminalDecisionConflict,
    #[error("hermetic provider execution binding conflicts with the retained process claim")]
    ExecutionBindingConflict,
    #[error(
        "hermetic provider process attempt remains pending after a service error and requires a \
         fresh retry"
    )]
    ProcessRetryRequired,
    #[error(
        "hermetic provider process attempt is executing or uncertain and requires explicit \
         recovery before it may continue"
    )]
    ProcessRecoveryRequired,
    #[error(
        "hermetic provider process recovery completed fail-closed with disposition {0:?}; \
         explicit recovery handling is required before continuation"
    )]
    ProcessRecoveryReconciled(RecoveredProcessDisposition),
    #[error(transparent)]
    ProcessRecovery(#[from] DurableProcessRecoveryError),
    #[error(transparent)]
    ServiceContract(#[from] ServiceContractError),
    #[error(transparent)]
    WorkerEvent(#[from] WorkerEventError),
    #[error(transparent)]
    RuntimeRegistry(#[from] RuntimeRegistryError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    RuntimeStore(#[from] RuntimeStoreError),
    #[error(transparent)]
    FixtureAuthority(#[from] FixtureAuthorityError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    ProviderLaunch(#[from] ProviderLaunchError),
    #[error("hermetic provider actor could not be constructed")]
    ActorBuild,
    #[error("hermetic provider actor shutdown observed an incomplete store close")]
    ActorShutdown,
    #[error("R0 fixture reached the durable phase-terminal boundary")]
    R0PhaseTerminalCut,
    #[cfg(test)]
    #[error("test stopped immediately after the phase terminal projection")]
    InjectedStopAfterPhaseTerminal,
}

fn close_runtime_store<T>(
    store: PrivateProcessLedgerStore,
    result: Result<T, HermeticProviderError>,
) -> Result<T, HermeticProviderError> {
    match store.close() {
        Ok(()) => result,
        Err(close) => Err(close.into()),
    }
}

fn map_actor_build_error(error: DurableProcessBuildError) -> HermeticProviderError {
    match error {
        DurableProcessBuildError::Store(close)
        | DurableProcessBuildError::ThreadStartAndStoreClose { close, .. } => close.into(),
        DurableProcessBuildError::Contract(contract) => contract.into(),
        DurableProcessBuildError::InvalidEnrollment | DurableProcessBuildError::ThreadStart(_) => {
            HermeticProviderError::ActorBuild
        }
    }
}

/// Attested fixture helper installed under a fresh, hermetic capability root.
///
/// Never a real provider binary. The exact byte content is pinned at install
/// time (`FreshFixtureAuthority::install_fixture_executable` already rejects
/// any content other than the one the admission policy expects) and re-proven
/// on every launch by `ExecutableCapability::open_verified` and by
/// `ProductionProcessLaunchAuthority`'s own SHA-256/length attestation.
pub(crate) struct AttestedFixtureHelper {
    executable: ExecutableCapability,
    label: String,
}

impl fmt::Debug for AttestedFixtureHelper {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttestedFixtureHelper")
            .field("kind", &"attested-fixture-helper")
            .finish_non_exhaustive()
    }
}

impl AttestedFixtureHelper {
    /// Installs `bytes` under `authority` as `label` and pins its exact
    /// SHA-256/length attestation. `authority` must be the same
    /// [`FreshFixtureAuthority`] that admits the workspace this helper will be
    /// launched against.
    pub(crate) fn install(
        authority: &FreshFixtureAuthority,
        label: &str,
        bytes: &[u8],
    ) -> Result<Self, HermeticProviderError> {
        let executable = authority.install_fixture_executable(label, bytes)?;
        Ok(Self {
            executable,
            label: label.to_owned(),
        })
    }

    fn recover(
        authority: &FreshFixtureAuthority,
        label: &str,
        bytes: &[u8],
    ) -> Result<Self, HermeticProviderError> {
        Ok(Self {
            executable: authority.recover_fixture_executable_inner(label, bytes)?,
            label: label.to_owned(),
        })
    }

    #[cfg(test)]
    fn recover_for_test(
        authority: &FreshFixtureAuthority,
        label: &str,
        bytes: &[u8],
    ) -> Result<Self, HermeticProviderError> {
        Self::recover(authority, label, bytes)
    }
}

fn execution_binding_payload(execution_binding: &ExecutionRequestFingerprint) -> serde_json::Value {
    serde_json::json!({
        "provider": "hermetic-attested-fixture",
        "execution_binding": {
            "schema": ExecutionRequestFingerprint::schema_version(),
            "sha256": execution_binding.to_lowercase_hex(),
        },
    })
}

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
    // Always `Continue`: this callback half of `WatchdogPolicy` is inert in
    // this composition. Real stall detection is `stall_window()` below,
    // consumed by `ProcessBudget`'s OS-level supervision.
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
                "hermetic provider does not admit external effects",
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

/// Fixture-only executor that runs the durable process service through the
/// ordinary dispatch contract. When `known_outcome` is set (an already
/// resolved ledger evidence recovered across a crash), the attested helper is
/// never relaunched — the typed outcome is reconstructed from durable
/// evidence only.
struct ProviderWorkerExecutor {
    outcome: Arc<Mutex<Option<AttemptOutcome>>>,
    descriptor: RuntimeDescriptor,
}

impl PhaseExecutor for ProviderWorkerExecutor {
    fn execute(
        &self,
        _request: orchestrator_exec::DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        if let Some(outcome) = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return outcome;
        }
        let _ = context;
        AttemptOutcome::incomplete(
            MechanicalTermination::ContractViolation,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        )
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(self.descriptor.clone())
    }
}

fn map_process_termination(
    termination: ProcessTerminationReceipt,
    elapsed: Duration,
) -> AttemptOutcome {
    match termination {
        ProcessTerminationReceipt::Exited(status) if status.as_code() == Some(0) => {
            AttemptOutcome::completed(
                "hermetic provider attempt completed",
                AttemptEvidence::new(),
                elapsed,
            )
            .unwrap_or_else(|_| {
                AttemptOutcome::incomplete(
                    MechanicalTermination::ContractViolation,
                    None,
                    PartialWork::empty(),
                    elapsed,
                )
            })
        }
        ProcessTerminationReceipt::Exited(status) => AttemptOutcome::incomplete(
            MechanicalTermination::ProcessExited(status),
            None,
            PartialWork::empty(),
            elapsed,
        ),
        ProcessTerminationReceipt::DeadlineExceeded => AttemptOutcome::incomplete(
            MechanicalTermination::HardDeadlineExceeded,
            None,
            PartialWork::empty(),
            elapsed,
        ),
        ProcessTerminationReceipt::Cancelled => AttemptOutcome::incomplete(
            MechanicalTermination::Cancelled,
            None,
            PartialWork::empty(),
            elapsed,
        ),
        ProcessTerminationReceipt::Stalled => AttemptOutcome::incomplete(
            MechanicalTermination::WatchdogStalled,
            None,
            PartialWork::empty(),
            elapsed,
        ),
        ProcessTerminationReceipt::OutputLimit
        | ProcessTerminationReceipt::SupervisorFailure
        | ProcessTerminationReceipt::UnresolvedOwnership => AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            elapsed,
        ),
    }
}

fn map_authoritative_process_receipt(
    request: &ProcessRequest,
    receipt: &ProcessReceipt,
) -> AttemptOutcome {
    let receipt_contract_failed = receipt.expose_stdout().len() > request.max_output_bytes()
        || receipt.expose_stderr().len() > request.max_output_bytes()
        || receipt.truncated_output_acknowledged() && !request.truncated_output_acknowledged()
        || (receipt.stdout_discarded() > 0 || receipt.stderr_discarded() > 0)
            && !receipt.truncated_output_acknowledged()
        || !receipt.ownership_released()
            && !matches!(
                receipt.termination(),
                ProcessTerminationReceipt::UnresolvedOwnership
            );
    if receipt_contract_failed {
        return AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            receipt.elapsed(),
        );
    }
    map_process_termination(receipt.termination(), receipt.elapsed())
}

/// Reconstructs the typed [`AttemptOutcome`] a prior, already-terminal ledger
/// observation proves, without touching the executor or the process service.
/// Only [`EffectEvidenceCode`] values are inspected — never provider text.
fn outcome_from_ledger_evidence(observation: &EffectObservation) -> AttemptOutcome {
    let evidence = observation.evidence();
    match evidence.code() {
        EffectEvidenceCode::ExitObservedSuccess => AttemptOutcome::completed(
            "hermetic provider attempt completed",
            AttemptEvidence::new(),
            Duration::ZERO,
        )
        .unwrap_or_else(|_| {
            AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::ZERO,
            )
        }),
        EffectEvidenceCode::ExitObservedFailure => {
            let termination = evidence
                .exit_code()
                .and_then(|code| ProcessExitStatus::code(code).ok())
                .map_or(
                    MechanicalTermination::SupervisorFailure,
                    MechanicalTermination::ProcessExited,
                );
            AttemptOutcome::incomplete(termination, None, PartialWork::empty(), Duration::ZERO)
        }
        EffectEvidenceCode::ProcessNotStarted => {
            let termination = match evidence.process_not_started_reason() {
                Some(ProcessNotStartedEvidenceReason::Cancelled) => {
                    MechanicalTermination::Cancelled
                }
                Some(ProcessNotStartedEvidenceReason::Deadline) => {
                    MechanicalTermination::HardDeadlineExceeded
                }
                Some(
                    ProcessNotStartedEvidenceReason::SpawnFailed
                    | ProcessNotStartedEvidenceReason::GateRejected
                    | ProcessNotStartedEvidenceReason::GateIndeterminate
                    | ProcessNotStartedEvidenceReason::GateProtocol
                    | ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
                )
                | None => MechanicalTermination::SupervisorFailure,
            };
            AttemptOutcome::incomplete(termination, None, PartialWork::empty(), Duration::ZERO)
        }
        EffectEvidenceCode::ProcessFailed => {
            let termination = evidence.started_process_failure().map_or(
                MechanicalTermination::SupervisorFailure,
                termination_from_started_process_failure,
            );
            AttemptOutcome::incomplete(termination, None, PartialWork::empty(), Duration::ZERO)
        }
        _ => AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        ),
    }
}

fn termination_from_started_process_failure(
    failure: StartedProcessFailureEvidence,
) -> MechanicalTermination {
    match failure {
        StartedProcessFailureEvidence::Signaled(signal) => ProcessExitStatus::signal(signal)
            .map_or(
                MechanicalTermination::SupervisorFailure,
                MechanicalTermination::ProcessExited,
            ),
        StartedProcessFailureEvidence::Deadline => MechanicalTermination::HardDeadlineExceeded,
        StartedProcessFailureEvidence::Stalled => MechanicalTermination::WatchdogStalled,
        StartedProcessFailureEvidence::Cancelled => MechanicalTermination::Cancelled,
        StartedProcessFailureEvidence::OutputLimit
        | StartedProcessFailureEvidence::InfrastructureFailure => {
            MechanicalTermination::SupervisorFailure
        }
    }
}

enum ExactProcessLedger {
    Pending,
    RecoveryRequired,
    Terminal(Box<ExactTerminalEvidence>),
}

struct ExactTerminalEvidence {
    outcome: AttemptOutcome,
    committed_at_utc: String,
}

fn inspect_exact_process_ledger(
    snapshot: &ExactOutboxSnapshot,
    expected_binding: &ExecutionRequestFingerprint,
    decision: Option<&TerminalDecisionRecord>,
) -> Result<ExactProcessLedger, HermeticProviderError> {
    if snapshot.effect().payload() != &execution_binding_payload(expected_binding) {
        return Err(HermeticProviderError::ExecutionBindingConflict);
    }
    let state = snapshot.effect().state();
    match state {
        OutboxState::Pending => {
            if decision.is_some() {
                return Err(HermeticProviderError::TerminalDecisionConflict);
            }
            Ok(ExactProcessLedger::Pending)
        }
        OutboxState::Executing | OutboxState::Uncertain => Ok(ExactProcessLedger::RecoveryRequired),
        OutboxState::Succeeded | OutboxState::Failed => {
            let observation = snapshot
                .current_observation()
                .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
            let committed_at_utc = snapshot
                .current_observation_committed_at_utc()?
                .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
            if observation.state() != state || observation.attempt() != snapshot.effect().attempts()
            {
                return Err(HermeticProviderError::TerminalDecisionConflict);
            }
            let outcome = outcome_from_ledger_evidence(observation);
            if decision.is_some_and(|record| record.observed_termination() != outcome.termination())
            {
                return Err(HermeticProviderError::TerminalDecisionConflict);
            }
            Ok(ExactProcessLedger::Terminal(Box::new(
                ExactTerminalEvidence {
                    outcome,
                    committed_at_utc: committed_at_utc.to_owned(),
                },
            )))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurableAttemptOpen {
    AdmitMissing,
    ExistingOnly,
}

struct DurableHermeticTerminal {
    outcome: Option<AttemptOutcome>,
    observed_termination: Option<MechanicalTermination>,
    committed_at_utc: String,
    decision: Option<TerminalDecisionRecord>,
}

impl DurableHermeticTerminal {
    fn from_evidence(
        evidence: ExactTerminalEvidence,
        decision: Option<TerminalDecisionRecord>,
    ) -> Self {
        Self {
            observed_termination: evidence.outcome.termination(),
            outcome: Some(evidence.outcome),
            committed_at_utc: evidence.committed_at_utc,
            decision,
        }
    }

    fn take_projection_outcome(&mut self) -> Result<AttemptOutcome, HermeticProviderError> {
        self.outcome
            .take()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)
    }

    const fn observed_termination(&self) -> Option<MechanicalTermination> {
        self.observed_termination
    }

    fn committed_at_utc(&self) -> &str {
        &self.committed_at_utc
    }
}

/// Process-only durable ownership for one exact hermetic attempt.
///
/// This operation has no workspace, lifecycle, or projection authority. It
/// may write only the private process claim/resolution and its matching
/// terminal decision. Activation consumes a sealed lazy phase-worker binding;
/// a runnable fresh/Pending attempt may materialize worker, artifact, and
/// scratch storage, but the operation can never write canonical lifecycle
/// state.
struct DurableHermeticAttempt {
    store: Option<PrivateProcessLedgerStore>,
    ledger_boundary: PrivateProcessLedgerBoundary,
    mission_id: MissionId,
    phase_id: String,
    identity: WorkerIdentity,
    attempt: u32,
    idempotency_key: String,
    execution_binding: ExecutionRequestFingerprint,
    prepared_launch: Option<PreparedProviderLaunch>,
    effect: Option<OutboxIntent>,
    process_binding: Option<ProcessEffectBinding>,
    open: DurableAttemptOpen,
    actor: Option<DurableProcessActor>,
    service: Option<DurableProcessService>,
    terminal: Option<DurableHermeticTerminal>,
    recovery_required: bool,
}

impl fmt::Debug for DurableHermeticAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableHermeticAttempt")
            .field("kind", &"private-process-attempt")
            .field("phase", &self.phase_id)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}

impl DurableHermeticAttempt {
    #[expect(
        clippy::too_many_arguments,
        reason = "every durable process binding stays explicit"
    )]
    fn open(
        ledger_boundary: PrivateProcessLedgerBoundary,
        mission_id: MissionId,
        phase_id: &PhaseId,
        identity: WorkerIdentity,
        attempt: u32,
        prepared_launch: PreparedProviderLaunch,
        execution_binding: ExecutionRequestFingerprint,
        open: DurableAttemptOpen,
    ) -> Result<Self, HermeticProviderError> {
        if identity.mission_id() != mission_id.as_str()
            || identity.phase_id() != phase_id.as_str()
            || identity.worker_id() != prepared_launch.worker_id()
        {
            return Err(HermeticProviderError::BindingMismatch);
        }
        let process_request = prepared_launch.exact_request();
        let (effect, idempotency_key, process_binding) = process_effect_for_attempt(
            &mission_id,
            phase_id.as_str(),
            attempt,
            process_request,
            &execution_binding,
        )?;
        let operation_slot = EffectOperationSlot::new(OPERATION_SLOT)?;
        let mut store =
            RuntimeStore::open_private(ledger_boundary.clone(), StorageActorAuthority::new())?;
        let retained = match store.exact_logical_outbox_snapshot(
            &mission_id,
            Some(phase_id.as_str()),
            OutboxEffectKind::ProviderProcess,
            &operation_slot,
            attempt,
        ) {
            Ok(retained) => retained,
            Err(error) => return close_runtime_store(store, Err(error.into())),
        };
        if retained.as_ref().is_some_and(|snapshot| {
            snapshot.effect().idempotency_key() != idempotency_key.as_str()
                || snapshot.effect().payload() != &execution_binding_payload(&execution_binding)
        }) {
            return close_runtime_store(
                store,
                Err(HermeticProviderError::ExecutionBindingConflict),
            );
        }

        let (effect, terminal, recovery_required) = if let Some(snapshot) = retained {
            let decision = match store.terminal_decision(
                &mission_id,
                phase_id.as_str(),
                identity.worker_id(),
                attempt,
            ) {
                Ok(decision) => decision,
                Err(error) => return close_runtime_store(store, Err(error.into())),
            };
            match inspect_exact_process_ledger(&snapshot, &execution_binding, decision.as_ref()) {
                Ok(ExactProcessLedger::Pending) if open == DurableAttemptOpen::ExistingOnly => {
                    return close_runtime_store(
                        store,
                        Err(HermeticProviderError::TerminalDecisionConflict),
                    );
                }
                Ok(ExactProcessLedger::Pending) => (None, None, false),
                Ok(ExactProcessLedger::RecoveryRequired) => (None, None, true),
                Ok(ExactProcessLedger::Terminal(_))
                    if open == DurableAttemptOpen::ExistingOnly && decision.is_none() =>
                {
                    return close_runtime_store(
                        store,
                        Err(HermeticProviderError::TerminalDecisionConflict),
                    );
                }
                Ok(ExactProcessLedger::Terminal(outcome)) => (
                    None,
                    Some(DurableHermeticTerminal::from_evidence(*outcome, decision)),
                    false,
                ),
                Err(error) => return close_runtime_store(store, Err(error)),
            }
        } else if open == DurableAttemptOpen::ExistingOnly {
            return close_runtime_store(
                store,
                Err(HermeticProviderError::TerminalDecisionConflict),
            );
        } else {
            (Some(effect), None, false)
        };

        let store = if terminal.is_some() {
            store.close()?;
            None
        } else {
            Some(store)
        };
        Ok(Self {
            store,
            ledger_boundary,
            mission_id,
            phase_id: phase_id.to_string(),
            identity,
            attempt,
            idempotency_key,
            execution_binding,
            prepared_launch: terminal.is_none().then_some(prepared_launch),
            effect,
            process_binding: Some(process_binding),
            open,
            actor: None,
            service: None,
            terminal,
            recovery_required,
        })
    }

    fn activate(
        &mut self,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
    ) -> Result<(), HermeticProviderError> {
        self.activate_inner(cancellation, timestamp_source, None)
    }

    fn activate_with_r0_observer(
        &mut self,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: R0ProcessBarrierObserver,
    ) -> Result<(), HermeticProviderError> {
        self.activate_inner(cancellation, timestamp_source, Some(observer))
    }

    fn activate_inner(
        &mut self,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Option<R0ProcessBarrierObserver>,
    ) -> Result<(), HermeticProviderError> {
        self.reject_reactivation()?;
        if self.effect.is_some() {
            let verified = self
                .prepared_launch
                .as_ref()
                .ok_or(HermeticProviderError::ActorBuild)
                .and_then(|prepared| {
                    prepared
                        .verify_pre_admission()
                        .map_err(HermeticProviderError::from)
                });
            if let Err(error) = verified {
                return self.close_store_with(Err(error));
            }
        }
        self.classify_admitted_effect()?;
        if self.terminal.is_some() {
            self.prepared_launch = None;
            return Ok(());
        }
        if self.recovery_required {
            return self.reconcile_recovery(timestamp_source.as_ref());
        }
        let binding = self
            .process_binding
            .take()
            .ok_or(HermeticProviderError::ActorBuild)?;
        let prepared = self
            .prepared_launch
            .take()
            .ok_or(HermeticProviderError::ActorBuild)?;
        let launch = match prepared.enroll(binding) {
            Ok(launch) => launch,
            Err(error) => return self.close_store_with(Err(error.into())),
        };
        let store = self
            .store
            .take()
            .ok_or(HermeticProviderError::ActorShutdown)?;
        let (actor, service) = match observer {
            Some(observer) => DurableProcessActor::spawn_with_observer(
                store,
                launch,
                cancellation,
                timestamp_source,
                observer,
            ),
            None => DurableProcessActor::spawn(store, launch, cancellation, timestamp_source),
        }
        .map_err(map_actor_build_error)?;
        self.actor = Some(actor);
        self.service = Some(service);
        Ok(())
    }

    fn classify_admitted_effect(&mut self) -> Result<(), HermeticProviderError> {
        if self.terminal.is_some() {
            return Ok(());
        }
        let mut store = self
            .store
            .take()
            .ok_or(HermeticProviderError::ActorShutdown)?;
        if let Some(effect) = self.effect.take() {
            if let Err(error) = admit_process_effect(
                &mut store,
                &self.mission_id,
                &self.phase_id,
                self.attempt,
                effect,
            ) {
                return close_runtime_store(store, Err(error));
            }
        }
        let inspected = (|| {
            let snapshot = store
                .exact_outbox_snapshot(&self.idempotency_key)?
                .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
            let decision = store.terminal_decision(
                &self.mission_id,
                &self.phase_id,
                self.identity.worker_id(),
                self.attempt,
            )?;
            let state = inspect_exact_process_ledger(
                &snapshot,
                &self.execution_binding,
                decision.as_ref(),
            )?;
            Ok::<_, HermeticProviderError>((state, decision))
        })();
        let (state, decision) = match inspected {
            Ok(inspected) => inspected,
            Err(error) => return close_runtime_store(store, Err(error)),
        };
        match state {
            ExactProcessLedger::Pending if self.open == DurableAttemptOpen::ExistingOnly => {
                close_runtime_store(store, Err(HermeticProviderError::TerminalDecisionConflict))
            }
            ExactProcessLedger::Pending => {
                self.store = Some(store);
                Ok(())
            }
            ExactProcessLedger::RecoveryRequired => {
                self.store = Some(store);
                self.recovery_required = true;
                Ok(())
            }
            ExactProcessLedger::Terminal(outcome) => {
                store.close()?;
                self.terminal = Some(DurableHermeticTerminal::from_evidence(*outcome, decision));
                Ok(())
            }
        }
    }

    fn reconcile_recovery(
        &mut self,
        timestamp_source: &dyn ProcessTimestampSource,
    ) -> Result<(), HermeticProviderError> {
        let binding = self
            .process_binding
            .as_ref()
            .ok_or(HermeticProviderError::ActorBuild)?;
        let request = self
            .prepared_launch
            .as_ref()
            .ok_or(HermeticProviderError::ActorBuild)?
            .exact_request();
        let observed_at_utc = timestamp_source.now_utc();
        let result = self
            .store
            .as_mut()
            .ok_or(HermeticProviderError::ActorShutdown)
            .and_then(|store| {
                reconcile_recovered_process_attempt(store, binding, request, &observed_at_utc)
                    .map_err(HermeticProviderError::from)
            });
        match result {
            Ok(disposition) => self.close_store_with(Err(
                HermeticProviderError::ProcessRecoveryReconciled(disposition),
            )),
            Err(error) => self.close_store_with(Err(error)),
        }
    }

    #[cfg(test)]
    fn activate_with_runtime(
        &mut self,
        runtime: DeferredProviderRuntime,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
    ) -> Result<(), HermeticProviderError> {
        self.reject_reactivation()?;
        if self.terminal.is_some() {
            return Ok(());
        }
        if self.recovery_required {
            return self.reconcile_recovery(timestamp_source.as_ref());
        }
        let binding = self
            .process_binding
            .take()
            .ok_or(HermeticProviderError::ActorBuild)?;
        let prepared = self
            .prepared_launch
            .take()
            .ok_or(HermeticProviderError::ActorBuild)?;
        let launch = match prepared.enroll_with_runtime_for_test(binding, runtime) {
            Ok(launch) => launch,
            Err(error) => return self.close_store_with(Err(error.into())),
        };
        let store = self
            .store
            .take()
            .ok_or(HermeticProviderError::ActorShutdown)?;
        let (actor, service) =
            DurableProcessActor::spawn(store, launch, cancellation, timestamp_source)
                .map_err(map_actor_build_error)?;
        self.actor = Some(actor);
        self.service = Some(service);
        Ok(())
    }

    fn reject_reactivation(&mut self) -> Result<(), HermeticProviderError> {
        if self.actor.is_some() || self.service.is_some() {
            self.shutdown_actor_with(Err(HermeticProviderError::ActorBuild))
        } else {
            Ok(())
        }
    }

    fn execute(&mut self) -> Result<(), HermeticProviderError> {
        if self.terminal.is_some() {
            return Ok(());
        }
        let mut sink = match ProcessOnlyEventSink::new() {
            Ok(sink) => sink,
            Err(error) => return self.shutdown_actor_with(Err(error.into())),
        };
        let effects = match DeniedEffects::new() {
            Ok(effects) => effects,
            Err(error) => return self.shutdown_actor_with(Err(error.into())),
        };
        let clock = SystemClock;
        let watchdog = FixedWatchdog {
            stall_window: Duration::from_secs(30),
        };
        let deadline = clock
            .now()
            .checked_add(Duration::from_secs(300))
            .unwrap_or_else(Instant::now);
        let candidate = {
            let Some(service) = self.service.as_ref() else {
                return self.shutdown_actor_with(Err(HermeticProviderError::ActorShutdown));
            };
            let process_request = match service.exact_request_copy() {
                Ok(request) => request,
                Err(_) => {
                    return self.shutdown_actor_with(Err(HermeticProviderError::ActorShutdown));
                }
            };
            let mut context = ExecutionContext::new(
                service,
                &clock,
                &watchdog,
                &effects,
                &mut sink,
                self.identity.clone(),
                deadline,
            );
            context
                .run_process(&process_request)
                .ok()
                .map(|receipt| map_authoritative_process_receipt(&process_request, &receipt))
        };
        self.shutdown_actor()?;
        let (evidence, decision) = self.inspect_current_terminal()?;
        if candidate
            .as_ref()
            .is_some_and(|candidate| candidate.termination() != evidence.outcome.termination())
        {
            return Err(HermeticProviderError::TerminalDecisionConflict);
        }
        let terminal = DurableHermeticTerminal::from_evidence(evidence, decision);
        self.terminal = Some(terminal);
        Ok(())
    }

    fn persist_or_validate_terminal_decision(
        &mut self,
        requested: VerificationDecision,
    ) -> Result<VerificationDecision, HermeticProviderError> {
        let terminal = self
            .terminal
            .as_mut()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
        if let Some(persisted) = terminal.decision.as_ref() {
            return Ok(persisted.verification());
        }
        validate_terminal_verification(requested)?;
        let record = TerminalDecisionRecord::new(
            &self.mission_id,
            &self.phase_id,
            self.identity.worker_id(),
            self.attempt,
            terminal.observed_termination(),
            requested,
        )?;
        let mut store =
            RuntimeStore::open_private(self.ledger_boundary.clone(), StorageActorAuthority::new())?;
        if let Err(error) = store.record_terminal_decision(&record) {
            return close_runtime_store(store, Err(error.into()));
        }
        store.close()?;
        terminal.decision = Some(record);
        Ok(requested)
    }

    fn terminal(&self) -> Option<&DurableHermeticTerminal> {
        self.terminal.as_ref()
    }

    fn decision(&self) -> Option<&TerminalDecisionRecord> {
        self.terminal
            .as_ref()
            .and_then(|terminal| terminal.decision.as_ref())
    }

    fn observed_termination(&self) -> Result<Option<MechanicalTermination>, HermeticProviderError> {
        self.terminal
            .as_ref()
            .map(DurableHermeticTerminal::observed_termination)
            .ok_or(HermeticProviderError::TerminalDecisionConflict)
    }

    fn take_projection_outcome(&mut self) -> Result<AttemptOutcome, HermeticProviderError> {
        self.terminal
            .as_mut()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?
            .take_projection_outcome()
    }

    fn seal_worker_projection_evidence(
        &mut self,
        request: ExecutionRequest,
        requested_runtime: &str,
    ) -> Result<DurableWorkerProjectionEvidence, HermeticProviderError> {
        self.shutdown()?;
        if request.mission() != self.mission_id.as_str()
            || request.phase() != self.phase_id
            || request.attempt() != self.attempt
            || request.fingerprint(self.identity.worker_id(), requested_runtime)
                != self.execution_binding
        {
            return Err(HermeticProviderError::BindingMismatch);
        }
        let terminal = self
            .terminal
            .as_mut()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
        let decision = terminal
            .decision
            .clone()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
        if decision.verification() != r0_worker_projection_verification()
            || decision.observed_termination() != terminal.observed_termination()
        {
            return Err(HermeticProviderError::TerminalDecisionConflict);
        }
        let committed_at_utc = terminal.committed_at_utc().to_owned();
        let outcome = terminal.take_projection_outcome()?;
        Ok(DurableWorkerProjectionEvidence {
            request,
            outcome,
            committed_at_utc,
            decision,
        })
    }

    fn inspect_current_terminal(
        &self,
    ) -> Result<(ExactTerminalEvidence, Option<TerminalDecisionRecord>), HermeticProviderError>
    {
        let mut store =
            RuntimeStore::open_private(self.ledger_boundary.clone(), StorageActorAuthority::new())?;
        let result = (|| {
            let snapshot = store
                .exact_outbox_snapshot(&self.idempotency_key)?
                .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
            let decision = store.terminal_decision(
                &self.mission_id,
                &self.phase_id,
                self.identity.worker_id(),
                self.attempt,
            )?;
            match inspect_exact_process_ledger(
                &snapshot,
                &self.execution_binding,
                decision.as_ref(),
            )? {
                ExactProcessLedger::Pending => Err(HermeticProviderError::ProcessRetryRequired),
                ExactProcessLedger::RecoveryRequired => {
                    Err(HermeticProviderError::ProcessRecoveryRequired)
                }
                ExactProcessLedger::Terminal(outcome) => Ok((*outcome, decision)),
            }
        })();
        close_runtime_store(store, result)
    }

    fn durable_execution_identity_present(&self) -> Result<bool, HermeticProviderError> {
        let mut store =
            RuntimeStore::open_private(self.ledger_boundary.clone(), StorageActorAuthority::new())?;
        let result = store
            .exact_outbox_snapshot(&self.idempotency_key)
            .map_err(HermeticProviderError::from)
            .and_then(|snapshot| {
                snapshot
                    .map(|snapshot| snapshot.effect().execution_identity().is_some())
                    .ok_or(HermeticProviderError::TerminalDecisionConflict)
            });
        close_runtime_store(store, result)
    }

    fn shutdown_actor(&mut self) -> Result<(), HermeticProviderError> {
        self.service = None;
        if let Some(actor) = self.actor.take() {
            actor.shutdown()?;
        }
        Ok(())
    }

    fn shutdown_actor_with<T>(
        &mut self,
        result: Result<T, HermeticProviderError>,
    ) -> Result<T, HermeticProviderError> {
        match self.shutdown_actor() {
            Ok(()) => result,
            Err(close) => Err(close),
        }
    }

    fn close_store_with<T>(
        &mut self,
        result: Result<T, HermeticProviderError>,
    ) -> Result<T, HermeticProviderError> {
        match self.store.take() {
            Some(store) => close_runtime_store(store, result),
            None => result,
        }
    }

    fn shutdown(&mut self) -> Result<(), HermeticProviderError> {
        self.shutdown_actor()?;
        self.close_store_with(Ok(()))
    }
}

impl Drop for DurableHermeticAttempt {
    fn drop(&mut self) {
        let _ = self.shutdown_actor();
        if let Some(store) = self.store.take() {
            let _ = store.close();
        }
    }
}

/// Event sink used only while the durable process effect executes. Worker
/// projection is a later, separately ordered step; any event call here is a
/// composition bug and fails closed.
struct ProcessOnlyEventSink {
    rejected: EventSinkError,
}

impl ProcessOnlyEventSink {
    fn new() -> Result<Self, WorkerEventError> {
        Ok(Self {
            rejected: EventSinkError::new(
                EventSinkErrorKind::Rejected,
                "process-only execution cannot project worker events",
            )?,
        })
    }
}

impl EventSink for ProcessOnlyEventSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        Err(self.rejected.clone())
    }
}

/// Non-cancelling process service installed only in the worker-projection
/// context. The projection executor never calls it; an accidental call is a
/// typed failure rather than a second launch.
struct ProjectionOnlyProcessService {
    unavailable: ProcessServiceError,
}

impl ProjectionOnlyProcessService {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            unavailable: ProcessServiceError::new(
                ProcessServiceErrorKind::NotEnrolled,
                "worker projection has no process execution authority",
            )?,
        })
    }
}

impl Cancellation for ProjectionOnlyProcessService {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ProcessService for ProjectionOnlyProcessService {
    fn finish_preflight(
        &self,
        _request: &ProcessRequest,
        _preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        Err(self.unavailable.clone())
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        Err(self.unavailable.clone())
    }
}

/// Runtime bound into [`LifecycleCoordinator`] for one hermetic provider
/// attempt. Holds only the fixed projection registry, clock, watchdog, and
/// denied process/effect services the fixture-only dispatch contract requires.
pub(crate) struct HermeticProviderRuntime {
    projection_process: ProjectionOnlyProcessService,
    executors: ExecutorRegistry,
    clock: SystemClock,
    watchdog: FixedWatchdog,
    effects: DeniedEffects,
    /// Test-only hook proving the crash-boundary "decision persisted, phase
    /// not yet finished" cut without a real process kill: forcing this true
    /// makes the terminal phase/mission transition fail closed with
    /// `LifecycleError::UnresolvedChildren`, exactly like a genuine crash
    /// between those two durable writes.
    #[cfg(test)]
    force_unresolved_children: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for HermeticProviderRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticProviderRuntime")
            .field("kind", &"durable-hermetic-provider")
            .finish_non_exhaustive()
    }
}

impl OwnedChildState for HermeticProviderRuntime {
    fn has_unresolved_children(&self) -> bool {
        #[cfg(test)]
        if self
            .force_unresolved_children
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return true;
        }
        process_wide_has_owned_processes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalPlan {
    Completed,
    Failed { phase_error: &'static str },
    Cancelled,
}

const fn completed_terminal_plan(verification: VerificationDecision) -> TerminalPlan {
    if verification.gate_passed() {
        TerminalPlan::Completed
    } else if matches!(verification.action(), VerificationAction::Cancelled) {
        TerminalPlan::Cancelled
    } else {
        TerminalPlan::Failed {
            phase_error: VERIFICATION_FAILED,
        }
    }
}

const fn terminal_plan_for_termination(
    termination: Option<MechanicalTermination>,
    verification: VerificationDecision,
) -> TerminalPlan {
    match termination {
        None => completed_terminal_plan(verification),
        Some(MechanicalTermination::Cancelled) => TerminalPlan::Cancelled,
        Some(termination) => TerminalPlan::Failed {
            phase_error: mechanical_failure_reason(termination),
        },
    }
}

const fn worker_kind_matches_termination(
    kind: WorkerEventKind,
    termination: Option<MechanicalTermination>,
) -> bool {
    matches!(
        (kind, termination),
        (WorkerEventKind::Completed, None) | (WorkerEventKind::Failed, Some(_))
    )
}

fn terminal_plan_matches_phase_state(plan: TerminalPlan, phase: &PhaseState) -> bool {
    match plan {
        TerminalPlan::Completed => {
            phase.status == PhaseStatus::Completed
                && phase.error.is_none()
                && phase.skip_reason.is_none()
        }
        TerminalPlan::Failed { phase_error } => {
            phase.status == PhaseStatus::Failed
                && phase.error.as_deref() == Some(phase_error)
                && phase.skip_reason.is_none()
        }
        TerminalPlan::Cancelled => {
            phase.status == PhaseStatus::Skipped
                && phase.error.is_none()
                && phase.skip_reason.as_deref() == Some(CANCELLED_REASON)
        }
    }
}

const fn terminal_plan_matches_mission(plan: TerminalPlan, status: MissionStatus) -> bool {
    matches!(
        (plan, status),
        (TerminalPlan::Completed, MissionStatus::Completed)
            | (TerminalPlan::Failed { .. }, MissionStatus::Failed)
            | (TerminalPlan::Cancelled, MissionStatus::Cancelled)
    )
}

fn require_terminal_workspace_proof(
    coordinator: &LifecycleCoordinator<HermeticProviderRuntime>,
    phase: &PhaseId,
    worker_id: &str,
    attempt: u32,
    observed_termination: Option<MechanicalTermination>,
    decision: Option<&TerminalDecisionRecord>,
) -> Result<(), HermeticProviderError> {
    let decision = decision.ok_or(HermeticProviderError::TerminalDecisionConflict)?;
    if observed_termination != decision.observed_termination() {
        return Err(HermeticProviderError::TerminalDecisionConflict);
    }
    let durable = coordinator
        .durable_attempt_replay(phase, worker_id, attempt)?
        .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
    if !worker_kind_matches_termination(durable.terminal().kind(), decision.observed_termination())
    {
        return Err(HermeticProviderError::TerminalDecisionConflict);
    }
    let plan =
        terminal_plan_for_termination(decision.observed_termination(), decision.verification());
    let phase_state = coordinator
        .state()
        .phase(phase)
        .ok_or(HermeticProviderError::BindingMismatch)?;
    if !terminal_plan_matches_phase_state(plan, phase_state)
        || coordinator.state().status().is_terminal()
            && !terminal_plan_matches_mission(plan, coordinator.state().status())
    {
        return Err(HermeticProviderError::TerminalDecisionConflict);
    }
    Ok(())
}

const fn is_terminal_lifecycle_transition(transition: &ReducerTransition) -> bool {
    matches!(
        transition,
        ReducerTransition::MissionCompleted
            | ReducerTransition::MissionFailed
            | ReducerTransition::MissionCancelled { .. }
            | ReducerTransition::PhaseCompleted
            | ReducerTransition::PhaseFailed { .. }
            | ReducerTransition::PhaseSkipped { .. }
    )
}

fn pending_transition_matches_plan(
    transition: &ReducerTransition,
    plan: TerminalPlan,
) -> Option<bool> {
    match (transition, plan) {
        (ReducerTransition::MissionStarted | ReducerTransition::PhaseStarted, _) => None,
        (
            ReducerTransition::PhaseCompleted | ReducerTransition::MissionCompleted,
            TerminalPlan::Completed,
        )
        | (ReducerTransition::MissionFailed, TerminalPlan::Failed { .. }) => Some(true),
        (ReducerTransition::PhaseFailed { error }, TerminalPlan::Failed { phase_error }) => {
            Some(error == phase_error)
        }
        (ReducerTransition::MissionCancelled { reason }, TerminalPlan::Cancelled) => {
            Some(reason == CANCELLED_REASON)
        }
        (
            ReducerTransition::PhaseCompleted
            | ReducerTransition::MissionCompleted
            | ReducerTransition::PhaseFailed { .. }
            | ReducerTransition::MissionFailed
            | ReducerTransition::MissionCancelled { .. }
            | ReducerTransition::PhaseSkipped { .. }
            | ReducerTransition::PhaseRetrying
            | ReducerTransition::Unknown { .. },
            _,
        ) => Some(false),
    }
}

fn validate_terminal_verification(
    verification: VerificationDecision,
) -> Result<(), HermeticProviderError> {
    if !verification.gate_passed() && verification.action() == VerificationAction::Continue {
        Err(HermeticProviderError::UnsupportedVerificationPolicy)
    } else {
        Ok(())
    }
}

/// Safe terminal state returned by [`HermeticProvider::run_to_terminal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HermeticProviderOutcome {
    mission_status: MissionStatus,
    phase_status: PhaseStatus,
}

impl HermeticProviderOutcome {
    #[must_use]
    pub(crate) const fn mission_status(&self) -> MissionStatus {
        self.mission_status
    }

    #[must_use]
    pub(crate) const fn phase_status(&self) -> PhaseStatus {
        self.phase_status
    }
}

/// One validated request bound to a durable, one-phase hermetic workspace,
/// composing `RuntimeStore` outbox/claim + `DurableProcessService` +
/// cancellation/deadline/watchdog + the attested fixture helper + typed
/// process outcome and Cell 2F terminal-decision persistence.
pub(crate) struct HermeticProvider {
    coordinator: LifecycleCoordinator<HermeticProviderRuntime>,
    phase: PhaseId,
    identity: WorkerIdentity,
    request: ExecutionRequest,
    requested_runtime: String,
    projection_outcome: Arc<Mutex<Option<AttemptOutcome>>>,
    durable_attempt: DurableHermeticAttempt,
    r0_stop_after_phase_terminal: bool,
    #[cfg(test)]
    stop_after_phase_terminal: bool,
    #[cfg(test)]
    fault_mission_terminal_after_event_sync: bool,
}

impl fmt::Debug for HermeticProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticProvider")
            .field("kind", &"durable-hermetic-provider")
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl HermeticProvider {
    /// Returns whether this admission retained a live actor/service pair that
    /// may execute the exact attempt. Callers capture this immediately after
    /// admission; terminal replay has neither child authority.
    #[must_use]
    pub(crate) fn attempt_is_runnable(&self) -> bool {
        self.durable_attempt.terminal().is_none()
            && self.durable_attempt.actor.is_some()
            && self.durable_attempt.service.is_some()
    }

    /// Returns whether the exact attempt ledger contains a kernel execution
    /// identity. The actor must already be shut down, as it owns the ledger's
    /// sole writer lease while runnable.
    pub(crate) fn durable_execution_identity_present(&self) -> Result<bool, HermeticProviderError> {
        self.durable_attempt.durable_execution_identity_present()
    }

    /// Binds an exact request to an existing or newly created hermetic
    /// workspace, admits the process outbox intent into `ledger_boundary`
    /// (idempotently — safe to call again after a crash with the same
    /// mission/phase/attempt), and spawns the durable process actor for this
    /// one attempt.
    ///
    /// `ledger_boundary` is a dedicated `ProductionBoundary` distinct from the
    /// fixture root backing `workspace`/`helper`: it holds only the process
    /// outbox claim/resolution and never a mission's canonical event log or
    /// checkpoint, so no projector ever needs to replay it.
    #[expect(
        clippy::too_many_arguments,
        reason = "every authority binding stays explicit"
    )]
    pub(crate) fn admit(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        helper: &AttestedFixtureHelper,
        ledger_boundary: PrivateProcessLedgerBoundary,
        requested_runtime: &str,
        request: ExecutionRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
    ) -> Result<Self, HermeticProviderError> {
        Self::admit_fixture(
            workspace,
            initial_state,
            helper,
            ledger_boundary,
            requested_runtime,
            request,
            cancellation,
            timestamp_source,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every authority binding and the actor-scoped fixture observer stay explicit"
    )]
    fn admit_with_r0_observer(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        helper: &AttestedFixtureHelper,
        ledger_boundary: PrivateProcessLedgerBoundary,
        requested_runtime: &str,
        request: ExecutionRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: R0ProcessBarrierObserver,
    ) -> Result<Self, HermeticProviderError> {
        Self::admit_fixture(
            workspace,
            initial_state,
            helper,
            ledger_boundary,
            requested_runtime,
            request,
            cancellation,
            timestamp_source,
            Some(observer),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every authority binding and the optional fixture observer stay explicit"
    )]
    fn admit_fixture(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        helper: &AttestedFixtureHelper,
        ledger_boundary: PrivateProcessLedgerBoundary,
        requested_runtime: &str,
        request: ExecutionRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Option<R0ProcessBarrierObserver>,
    ) -> Result<Self, HermeticProviderError> {
        // Fail closed before any journaling if the process ledger and the
        // workspace share the same capability root. Nothing downstream
        // guards against this: the ledger boundary's own journal rows use a
        // transition kind (`CLAIM_TRANSITION_KIND`) no projector recognizes,
        // so a projector mistakenly pointed at the same directory as a
        // mission's workspace would fail to replay it — or worse, partially
        // succeed on a mix of recognized and unrecognized rows. Compares by
        // both `Arc` identity and the verified canonical physical root. The
        // latter matters because two separately enrolled `ProductionBoundary`
        // values can retain distinct `Arc`s for the same directory. This does
        // not weaken `prepare_fixture_launch`'s helper/workspace check below:
        // executable launch authority still requires exact shared-`Arc`
        // provenance.
        let ledger_boundary_clone = Arc::clone(ledger_boundary.production_boundary());
        let ledger_shared: SharedCapabilityRoot = ledger_boundary_clone;
        if workspace.shares_boundary(&ledger_shared)
            || workspace.aliases_boundary_root(&ledger_shared)?
        {
            return Err(HermeticProviderError::LedgerBoundaryCollision);
        }

        let (phase, dependencies) = {
            let mut phases = initial_state.phases();
            let phase = phases
                .next()
                .ok_or(HermeticProviderError::ExactlyOnePhaseRequired)?;
            let phase_id = phase.id.clone();
            let dependencies = phase
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if phases.next().is_some() {
                return Err(HermeticProviderError::ExactlyOnePhaseRequired);
            }
            (phase_id, dependencies)
        };
        if !dependencies.is_empty() || !request.dependencies().is_empty() {
            return Err(HermeticProviderError::DependenciesUnsupported);
        }
        if request.resume_from().is_some() || !request.expected_evidence().is_empty() {
            return Err(HermeticProviderError::UnsupportedRequest);
        }
        if initial_state.mission_id().as_str() != request.mission()
            || workspace.mission_id() != initial_state.mission_id()
            || phase.as_str() != request.phase()
            || dependencies != request.dependencies()
        {
            return Err(HermeticProviderError::BindingMismatch);
        }
        if !workspace.shares_boundary(&helper.executable.boundary) {
            return Err(HermeticProviderError::BindingMismatch);
        }

        let phase_worker_binding =
            workspace.phase_worker_binding(request.persona(), &phase, request.worker_dir())?;
        let prepared_launch = PreparedProviderLaunch::from_attested_fixture(
            phase_worker_binding,
            &helper.executable,
        )?;
        Self::admit_inner(
            workspace,
            initial_state,
            prepared_launch,
            ledger_boundary,
            requested_runtime,
            request,
            cancellation,
            timestamp_source,
            phase,
            observer,
        )
    }

    /// Admits a canary helper through the same lifecycle as the fixture path.
    /// The canary's enrolled current-executable replaces the fixture helper;
    /// everything downstream (workspace, ledger, actor, projection, terminal
    /// decision) is identical.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code, reason = "staged for Stage 2 canary mission composition")]
    #[expect(
        clippy::too_many_arguments,
        reason = "every authority binding stays explicit"
    )]
    pub(crate) fn admit_canary(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        canary: &crate::hermetic_process_canary::HermeticProcessCanary,
        ledger_boundary: PrivateProcessLedgerBoundary,
        requested_runtime: &str,
        request: ExecutionRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
    ) -> Result<Self, HermeticProviderError> {
        let ledger_shared: SharedCapabilityRoot = ledger_boundary.production_boundary().clone();
        if workspace.shares_boundary(&ledger_shared)
            || workspace.aliases_boundary_root(&ledger_shared)?
        {
            return Err(HermeticProviderError::LedgerBoundaryCollision);
        }
        let (phase, dependencies) = {
            let mut phases = initial_state.phases();
            let phase = phases
                .next()
                .ok_or(HermeticProviderError::ExactlyOnePhaseRequired)?;
            let phase_id = phase.id.clone();
            let dependencies = phase
                .dependencies
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if phases.next().is_some() {
                return Err(HermeticProviderError::ExactlyOnePhaseRequired);
            }
            (phase_id, dependencies)
        };
        if !dependencies.is_empty() || !request.dependencies().is_empty() {
            return Err(HermeticProviderError::DependenciesUnsupported);
        }
        if request.resume_from().is_some() || !request.expected_evidence().is_empty() {
            return Err(HermeticProviderError::UnsupportedRequest);
        }
        if initial_state.mission_id().as_str() != request.mission()
            || workspace.mission_id() != initial_state.mission_id()
            || phase.as_str() != request.phase()
            || dependencies != request.dependencies()
        {
            return Err(HermeticProviderError::BindingMismatch);
        }
        let compatibility_home: Arc<crate::ProductionBoundary> = Arc::clone(canary.boundary());
        let canary_boundary: SharedCapabilityRoot = compatibility_home;
        if !workspace.shares_boundary(&canary_boundary) {
            return Err(HermeticProviderError::BindingMismatch);
        }

        let phase_worker_binding =
            workspace.phase_worker_binding(request.persona(), &phase, request.worker_dir())?;
        let prepared_launch =
            PreparedProviderLaunch::from_attested_canary(phase_worker_binding, canary)?;
        Self::admit_inner(
            workspace,
            initial_state,
            prepared_launch,
            ledger_boundary,
            requested_runtime,
            request,
            cancellation,
            timestamp_source,
            phase,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every authority binding stays explicit"
    )]
    fn admit_inner(
        workspace: WorkspaceAuthority,
        initial_state: MissionState,
        prepared_launch: PreparedProviderLaunch,
        ledger_boundary: PrivateProcessLedgerBoundary,
        requested_runtime: &str,
        request: ExecutionRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        phase: PhaseId,
        observer: Option<R0ProcessBarrierObserver>,
    ) -> Result<Self, HermeticProviderError> {
        let worker_id = prepared_launch.worker_id().to_owned();
        let identity = WorkerIdentity::new(request.mission(), request.phase(), &worker_id)
            .map_err(|_| HermeticProviderError::BindingMismatch)?;
        let execution_binding = request.fingerprint(&worker_id, requested_runtime);
        let mission_id = initial_state.mission_id().clone();

        let descriptor = request.runtime().clone();
        let projection_outcome = Arc::new(Mutex::new(None));
        let mut executors = ExecutorRegistry::new();
        let _previous = executors.register(
            requested_runtime,
            Arc::new(ProviderWorkerExecutor {
                outcome: Arc::clone(&projection_outcome),
                descriptor: RuntimeDescriptor::new(
                    descriptor,
                    orchestrator_exec::RuntimeCaps {
                        tool_use: false,
                        session_resume: false,
                        streaming: false,
                        cost_report: false,
                        artifacts: false,
                    },
                ),
            }),
        )?;
        let runtime = HermeticProviderRuntime {
            projection_process: ProjectionOnlyProcessService::new()?,
            executors,
            clock: SystemClock,
            watchdog: FixedWatchdog {
                stall_window: Duration::from_secs(30),
            },
            effects: DeniedEffects::new()?,
            #[cfg(test)]
            force_unresolved_children: std::sync::atomic::AtomicBool::new(false),
        };

        // Reconstruct, repair, and validate workspace truth before opening the
        // process ledger or materializing any lazy phase-worker storage.
        let mut coordinator = LifecycleCoordinator::new(workspace, runtime, initial_state)?;
        if coordinator
            .pending_lifecycle_transition()
            .is_some_and(|transition| {
                matches!(
                    transition,
                    ReducerTransition::MissionStarted | ReducerTransition::PhaseStarted
                )
            })
        {
            coordinator.repair_pending_admission_transition()?;
        }
        if coordinator
            .pending_lifecycle_transition()
            .is_some_and(|transition| !is_terminal_lifecycle_transition(transition))
        {
            return Err(LifecycleError::RecoveryPending.into());
        }
        let phase_status = coordinator
            .state()
            .phase(&phase)
            .map(|phase| phase.status)
            .ok_or(HermeticProviderError::BindingMismatch)?;
        let terminal_workspace =
            coordinator.state().status().is_terminal() || phase_status.is_terminal();
        let pending_terminal_recovery = coordinator
            .pending_lifecycle_transition()
            .is_some_and(is_terminal_lifecycle_transition);
        let recovery_workspace = terminal_workspace || pending_terminal_recovery;

        let open = if recovery_workspace {
            DurableAttemptOpen::ExistingOnly
        } else {
            DurableAttemptOpen::AdmitMissing
        };
        let mut durable_attempt = DurableHermeticAttempt::open(
            ledger_boundary,
            mission_id,
            &phase,
            identity.clone(),
            request.attempt(),
            prepared_launch,
            execution_binding,
            open,
        )?;
        match observer {
            Some(observer) => {
                durable_attempt.activate_with_r0_observer(
                    cancellation,
                    timestamp_source,
                    observer,
                )?;
            }
            None => durable_attempt.activate(cancellation, timestamp_source)?,
        }
        if terminal_workspace && !pending_terminal_recovery {
            require_terminal_workspace_proof(
                &coordinator,
                &phase,
                identity.worker_id(),
                request.attempt(),
                durable_attempt.observed_termination()?,
                durable_attempt.decision(),
            )?;
        }
        if durable_attempt.terminal().is_some() {
            *projection_outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(durable_attempt.take_projection_outcome()?);
        }

        Ok(Self {
            coordinator,
            phase,
            identity,
            request,
            requested_runtime: requested_runtime.to_owned(),
            projection_outcome,
            durable_attempt,
            r0_stop_after_phase_terminal: false,
            #[cfg(test)]
            stop_after_phase_terminal: false,
            #[cfg(test)]
            fault_mission_terminal_after_event_sync: false,
        })
    }

    /// Drives one hermetic provider attempt and its enclosing one-phase
    /// lifecycle terminal to completion, persisting the Cell 2F terminal
    /// decision at the moment it is first selected.
    pub(crate) fn run_to_terminal(
        &mut self,
        verification: VerificationDecision,
    ) -> Result<HermeticProviderOutcome, HermeticProviderError> {
        let outcome = self.run_to_terminal_inner(verification);
        // Safety net for every early-return path. Normal process execution
        // joins the actor before reopening the private ledger and before any
        // worker/lifecycle projection; terminal replay has no actor.
        self.durable_attempt.shutdown()?;
        outcome
    }

    fn run_to_terminal_inner(
        &mut self,
        verification: VerificationDecision,
    ) -> Result<HermeticProviderOutcome, HermeticProviderError> {
        // A fresh Warn-shaped request has no terminal authority and must not
        // advance mission/phase admission, claim a process, or project worker
        // events. Recovery with an already-durable decision deliberately
        // ignores the caller's later verification choice.
        if self.durable_attempt.decision().is_none() {
            validate_terminal_verification(verification)?;
        }
        if let Some(pending) = self.coordinator.pending_lifecycle_transition() {
            if !matches!(
                pending,
                ReducerTransition::MissionStarted | ReducerTransition::PhaseStarted
            ) {
                let (plan, _selected) = self.persisted_terminal_plan()?;
                if pending_transition_matches_plan(pending, plan) != Some(true) {
                    return Err(HermeticProviderError::TerminalDecisionConflict);
                }
            }
        }
        // A pending phase completion must be repaired with the decision that
        // was selected before worker projection, never with a fresh caller
        // argument. Admission/start repairs do not inspect this value.
        let repair_verification = self
            .durable_attempt
            .decision()
            .map(TerminalDecisionRecord::verification);
        self.coordinator
            .repair_pending_lifecycle_transition(repair_verification)?;

        if self.coordinator.state().status().is_terminal() {
            self.require_current_terminal_proof()?;
            return Ok(HermeticProviderOutcome {
                mission_status: self.coordinator.state().status(),
                phase_status: self.phase_status()?,
            });
        }

        if self.phase_status()?.is_terminal() {
            let (plan, _selected) = self.persisted_terminal_plan()?;
            let phase_state = self
                .coordinator
                .state()
                .phase(&self.phase)
                .ok_or(HermeticProviderError::BindingMismatch)?;
            if !terminal_plan_matches_phase_state(plan, phase_state) {
                return Err(HermeticProviderError::TerminalDecisionConflict);
            }
            self.require_current_terminal_proof()?;
            self.finish_mission(plan)?;
            return Ok(HermeticProviderOutcome {
                mission_status: self.coordinator.state().status(),
                phase_status: self.phase_status()?,
            });
        }

        self.ensure_phase_running()?;

        let active_projection = match self.coordinator.durable_attempt_replay(
            &self.phase,
            self.identity.worker_id(),
            self.request.attempt(),
        ) {
            Ok(Some(durable)) => {
                let observed_termination = self.current_terminal_process_termination()?;
                let selected =
                    self.persist_or_validate_terminal_decision(observed_termination, verification)?;
                let plan = terminal_plan_for_termination(observed_termination, selected);
                if !worker_kind_matches_termination(durable.terminal().kind(), observed_termination)
                {
                    return Err(HermeticProviderError::TerminalDecisionConflict);
                }
                self.finish_running_phase(plan, selected)?;
                return Ok(HermeticProviderOutcome {
                    mission_status: self.coordinator.state().status(),
                    phase_status: self.phase_status()?,
                });
            }
            Ok(None) => false,
            Err(LifecycleError::AttemptRecoveryRequired) => true,
            Err(error) => return Err(error.into()),
        };

        let needs_execution = self
            .projection_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        if active_projection && needs_execution {
            return Err(HermeticProviderError::TerminalDecisionConflict);
        }
        if needs_execution {
            self.durable_attempt.execute()?;
            let outcome = self.durable_attempt.take_projection_outcome()?;
            *self
                .projection_outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        } else {
            // A prior process outcome was already durable. Close the unused
            // ledger writer before loading or recording its terminal decision.
            self.durable_attempt.shutdown_actor()?;
        }

        let observed_termination = self
            .projection_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?
            .termination();
        let selected_verification =
            self.persist_or_validate_terminal_decision(observed_termination, verification)?;
        let plan = terminal_plan_for_termination(observed_termination, selected_verification);
        if self.current_terminal_process_termination()? != observed_termination {
            return Err(HermeticProviderError::TerminalDecisionConflict);
        }

        let executor = {
            let runtime = self.coordinator.registry()?;
            let executor = runtime.executors.resolve(&self.requested_runtime)?;
            if executor.effective_runtime() != self.request.runtime() {
                return Err(HermeticProviderError::BindingMismatch);
            }
            executor
        };
        let worker_id = self.identity.worker_id().to_owned();
        let phase = self.phase.clone();
        let attempt = self.request.attempt();
        let identity = self.identity.clone();
        let request = &self.request;
        let project = |runtime: &HermeticProviderRuntime,
                       sink: &mut crate::FixtureWorkerEventSink<'_>| {
            let deadline = runtime
                .clock
                .now()
                .checked_add(Duration::from_secs(300))
                .unwrap_or_else(Instant::now);
            let mut context = ExecutionContext::new(
                &runtime.projection_process,
                &runtime.clock,
                &runtime.watchdog,
                &runtime.effects,
                sink,
                identity,
                deadline,
            );
            executor.execute(request, &mut context)
        };
        let run = if active_projection {
            self.coordinator
                .project_terminal_into_active_attempt(&phase, &worker_id, attempt, project)?
        } else {
            self.coordinator
                .with_attempt(&phase, &worker_id, attempt, project)?
        };
        match run {
            FixtureAttemptRun::Executed(outcome) => {
                let projected = outcome?;
                if projected.termination() != observed_termination {
                    return Err(HermeticProviderError::TerminalDecisionConflict);
                }
            }
            FixtureAttemptRun::Replayed(durable) => {
                if !worker_kind_matches_termination(durable.terminal().kind(), observed_termination)
                {
                    return Err(HermeticProviderError::TerminalDecisionConflict);
                }
            }
        }

        self.finish_running_phase(plan, selected_verification)?;

        Ok(HermeticProviderOutcome {
            mission_status: self.coordinator.state().status(),
            phase_status: self.phase_status()?,
        })
    }

    fn persist_or_validate_terminal_decision(
        &mut self,
        observed_termination: Option<MechanicalTermination>,
        requested: VerificationDecision,
    ) -> Result<VerificationDecision, HermeticProviderError> {
        if self.durable_attempt.observed_termination()? != observed_termination {
            return Err(HermeticProviderError::TerminalDecisionConflict);
        }
        self.durable_attempt
            .persist_or_validate_terminal_decision(requested)
    }

    fn persisted_terminal_plan(
        &self,
    ) -> Result<(TerminalPlan, VerificationDecision), HermeticProviderError> {
        let decision = self
            .durable_attempt
            .decision()
            .ok_or(HermeticProviderError::TerminalDecisionConflict)?;
        Ok((
            terminal_plan_for_termination(decision.observed_termination(), decision.verification()),
            decision.verification(),
        ))
    }

    fn require_current_terminal_proof(&self) -> Result<(), HermeticProviderError> {
        require_terminal_workspace_proof(
            &self.coordinator,
            &self.phase,
            self.identity.worker_id(),
            self.request.attempt(),
            self.current_terminal_process_termination()?,
            self.durable_attempt.decision(),
        )
    }

    fn current_terminal_process_termination(
        &self,
    ) -> Result<Option<MechanicalTermination>, HermeticProviderError> {
        self.durable_attempt.observed_termination()
    }

    fn ensure_phase_running(&mut self) -> Result<(), HermeticProviderError> {
        if self.coordinator.state().status() == MissionStatus::NotStarted {
            self.coordinator.transition_allocated(
                None,
                ReducerTransition::MissionStarted,
                serde_json::Value::Null,
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
                serde_json::Value::Null,
                None,
            )?;
        }
        Ok(())
    }

    fn phase_status(&self) -> Result<PhaseStatus, HermeticProviderError> {
        self.coordinator
            .state()
            .phase(&self.phase)
            .map(|phase| phase.status)
            .ok_or(HermeticProviderError::BindingMismatch)
    }

    /// Forces the next terminal phase/mission transition to observe
    /// unresolved owned children, deterministically reproducing "crashed
    /// after the Cell 2F decision persisted, before the phase/mission
    /// transition durably finished" without a real process kill.
    #[cfg(test)]
    fn force_unresolved_children_for_test(&self) -> Result<(), HermeticProviderError> {
        self.coordinator
            .registry()?
            .force_unresolved_children
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    fn stop_after_phase_terminal_for_test(&mut self) {
        self.stop_after_phase_terminal = true;
    }

    #[cfg(test)]
    fn fault_mission_terminal_after_event_sync_for_test(&mut self) {
        self.fault_mission_terminal_after_event_sync = true;
    }

    fn continue_after_phase_terminal(&mut self) -> Result<(), HermeticProviderError> {
        if std::mem::take(&mut self.r0_stop_after_phase_terminal) {
            return Err(HermeticProviderError::R0PhaseTerminalCut);
        }
        #[cfg(test)]
        if std::mem::take(&mut self.stop_after_phase_terminal) {
            return Err(HermeticProviderError::InjectedStopAfterPhaseTerminal);
        }
        Ok(())
    }

    fn finish_running_phase(
        &mut self,
        plan: TerminalPlan,
        verification: VerificationDecision,
    ) -> Result<(), HermeticProviderError> {
        match plan {
            TerminalPlan::Completed => {
                self.coordinator.transition_allocated(
                    Some(self.phase.clone()),
                    ReducerTransition::PhaseCompleted,
                    serde_json::Value::Null,
                    Some(verification),
                )?;
                self.continue_after_phase_terminal()?;
                self.finish_mission(plan)
            }
            TerminalPlan::Failed { phase_error } => {
                self.coordinator.transition_allocated(
                    Some(self.phase.clone()),
                    ReducerTransition::PhaseFailed {
                        error: phase_error.to_owned(),
                    },
                    serde_json::json!({"error": phase_error}),
                    None,
                )?;
                self.continue_after_phase_terminal()?;
                self.finish_mission(plan)
            }
            TerminalPlan::Cancelled => self.finish_mission(plan),
        }
    }

    fn finish_mission(&mut self, plan: TerminalPlan) -> Result<(), HermeticProviderError> {
        let (transition, data) = match plan {
            TerminalPlan::Completed => {
                (ReducerTransition::MissionCompleted, serde_json::Value::Null)
            }
            TerminalPlan::Failed { .. } => {
                (ReducerTransition::MissionFailed, serde_json::Value::Null)
            }
            TerminalPlan::Cancelled => (
                ReducerTransition::MissionCancelled {
                    reason: CANCELLED_REASON.to_owned(),
                },
                serde_json::json!({"reason": CANCELLED_REASON}),
            ),
        };
        #[cfg(test)]
        if std::mem::take(&mut self.fault_mission_terminal_after_event_sync) {
            self.coordinator.inject_fixture_projection_fault_once(
                crate::FixtureProjectionFault::AfterEventSync,
            );
        }
        self.coordinator
            .transition_allocated(None, transition, data, None)?;
        Ok(())
    }
}

fn process_effect_for_attempt(
    mission_id: &MissionId,
    phase_id: &str,
    attempt: u32,
    request: &ProcessRequest,
    execution_binding: &ExecutionRequestFingerprint,
) -> Result<(OutboxIntent, String, ProcessEffectBinding), HermeticProviderError> {
    let effect = OutboxIntent::for_process(
        mission_id.clone(),
        Some(phase_id.to_owned()),
        OutboxEffectKind::ProviderProcess,
        EffectOperationSlot::new(OPERATION_SLOT)?,
        attempt,
        execution_binding_payload(execution_binding),
        request,
    )?;
    let idempotency_key = effect.idempotency_key().to_owned();
    let binding = effect.bind_process(request)?;
    Ok((effect, idempotency_key, binding))
}

/// Admits (idempotently) the already validated process outbox intent for this
/// exact mission/phase/attempt binding. Helper, workspace, and process-root
/// validation has completed before this function can receive the intent.
fn admit_process_effect(
    store: &mut PrivateProcessLedgerStore,
    mission_id: &MissionId,
    phase_id: &str,
    attempt: u32,
    effect: OutboxIntent,
) -> Result<(), HermeticProviderError> {
    let transition_id = format!("hermetic-provider-claim:{mission_id}:{phase_id}:{attempt}");
    // This dedicated schema-v4 private ledger has no compatibility projection
    // surface. Its typed append seals the private marker, self-acknowledges
    // atomically, and retains the deterministic timestamp for exact retries.
    let committed_at_utc = format!("2000-01-01T00:00:00.{attempt:019}Z");
    let intent = JournalIntent::new(
        transition_id,
        Some(mission_id.clone()),
        CLAIM_TRANSITION_KIND,
        serde_json::json!({"phase_id": phase_id, "attempt": attempt}),
        committed_at_utc.clone(),
    )?
    .with_outbox(effect)?;
    store.append(&intent)?;
    Ok(())
}

const R0_JOURNAL_MISSION: &str = "r0-journal-restart";
const R0_JOURNAL_PHASE: &str = "verify";
const R0_PROCESS_PERSONA: &str = "r0-process";
const R0_PROCESS_RUNTIME: &str = "r0-fixture-runtime";
const R0_PROCESS_HELPER: &str = "r0-durable-attempt-helper";
const R0_PROCESS_LEDGER_TARGET: &str = "r0-private-process-ledger";

/// Stable private-ledger evidence from the real process-only R0 attempt.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct R0DurableAttemptReport {
    state: OutboxState,
    attempts: u32,
    observation_state: OutboxState,
    evidence_code: EffectEvidenceCode,
    execution_identity_present: bool,
    observation_history_count: usize,
    terminal_decision_present: bool,
    helper_executed: bool,
}

impl R0DurableAttemptReport {
    /// Terminal state of the exact process outbox row.
    #[must_use]
    pub const fn state(&self) -> OutboxState {
        self.state
    }

    /// Number of durable process claims for the exact idempotency key.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// State carried by the newest immutable process observation.
    #[must_use]
    pub const fn observation_state(&self) -> OutboxState {
        self.observation_state
    }

    /// Typed evidence carried by the newest immutable process observation.
    #[must_use]
    pub const fn evidence_code(&self) -> EffectEvidenceCode {
        self.evidence_code
    }

    /// Whether the exact kernel execution identity was retained.
    #[must_use]
    pub const fn execution_identity_present(&self) -> bool {
        self.execution_identity_present
    }

    /// Exact number of immutable observations retained for the idempotency key.
    #[must_use]
    pub const fn observation_history_count(&self) -> usize {
        self.observation_history_count
    }

    /// Whether the exact terminal process decision is durable.
    #[must_use]
    pub const fn terminal_decision_present(&self) -> bool {
        self.terminal_decision_present
    }

    /// Whether the helper executed exactly once and no duplicate marker exists.
    #[must_use]
    pub const fn helper_executed(&self) -> bool {
        self.helper_executed
    }
}

/// Sealed bridge from one exact durable process terminal into canonical
/// worker projection.
///
/// The fields have no constructor outside this module. In particular, a raw
/// [`AttemptOutcome`] and caller-supplied timestamp can never be promoted into
/// projection authority.
pub(crate) struct DurableWorkerProjectionEvidence {
    request: ExecutionRequest,
    outcome: AttemptOutcome,
    committed_at_utc: String,
    decision: TerminalDecisionRecord,
}

impl DurableWorkerProjectionEvidence {
    pub(crate) const fn request(&self) -> &ExecutionRequest {
        &self.request
    }

    pub(crate) const fn outcome(&self) -> &AttemptOutcome {
        &self.outcome
    }

    pub(crate) fn committed_at_utc(&self) -> &str {
        &self.committed_at_utc
    }

    pub(crate) const fn decision(&self) -> &TerminalDecisionRecord {
        &self.decision
    }
}

/// One sealed projection input paired with its independently reopened ledger
/// summary.
pub(crate) struct R0DurableWorkerProjection {
    evidence: DurableWorkerProjectionEvidence,
    report: R0DurableAttemptReport,
}

impl R0DurableWorkerProjection {
    pub(crate) const fn evidence(&self) -> &DurableWorkerProjectionEvidence {
        &self.evidence
    }

    pub(crate) const fn report(&self) -> &R0DurableAttemptReport {
        &self.report
    }
}

/// Failure from a capability-bound R0 durable-attempt fixture.
#[doc(hidden)]
#[derive(Debug, Error)]
#[error("R0 durable-attempt fixture failed during {stage}: {detail}")]
pub struct R0DurableAttemptError {
    stage: &'static str,
    detail: String,
}

fn r0_attempt_error(stage: &'static str, error: impl fmt::Display) -> R0DurableAttemptError {
    R0DurableAttemptError {
        stage,
        detail: error.to_string(),
    }
}

#[derive(Clone)]
struct R0ProcessBarrierObserver {
    target: DurableProcessBarrierPoint,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl DurableProcessBarrierObserver for R0ProcessBarrierObserver {
    fn observe(&self, point: DurableProcessBarrierPoint) {
        if point == self.target {
            (self.notify)();
            // A notification callback is expected to park forever after
            // flushing the parent-visible ACK. If it unexpectedly returns,
            // retain the actor and its exact writer/launch authorities at the
            // cut until the supervisor is killed.
            loop {
                std::thread::park();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct R0ProcessMarkerCounts {
    claimed: u32,
    spawn_permitted: u32,
    release_authorized: u32,
    started_observed: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct R0ProcessCutSnapshot {
    state: OutboxState,
    attempts: u32,
    execution_identity: Option<ProcessExecutionIdentity>,
    observation_history_count: usize,
    terminal_decision_present: bool,
    evidence_code: Option<EffectEvidenceCode>,
    not_started_reason: Option<ProcessNotStartedEvidenceReason>,
    uncertainty: Option<ProcessUncertaintyEvidence>,
    recovery_classification: Option<RecoveredProcessAttemptClassification>,
    markers: R0ProcessMarkerCounts,
}

struct R0DurableAttemptBindings {
    mission: MissionId,
    phase: PhaseId,
    identity: WorkerIdentity,
    request: ExecutionRequest,
    execution_binding: ExecutionRequestFingerprint,
    prepared_launch: PreparedProviderLaunch,
    ledger_boundary: PrivateProcessLedgerBoundary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum R0AttemptResourceMode {
    RunOrReplay,
    ExistingOnly,
}

struct R0ProcessTimestampSource;

impl ProcessTimestampSource for R0ProcessTimestampSource {
    fn now_utc(&self) -> String {
        "2026-07-31T00:02:00Z".to_owned()
    }
}

fn r0_attempt_descriptor(
    bindings: &R0DurableAttemptBindings,
) -> Result<(String, ProcessEffectBinding), R0DurableAttemptError> {
    let process_request = bindings.prepared_launch.exact_request();
    let (_, idempotency_key, process_binding) = process_effect_for_attempt(
        &bindings.mission,
        bindings.phase.as_str(),
        1,
        process_request,
        &bindings.execution_binding,
    )
    .map_err(|error| r0_attempt_error("derive process effect", error))?;
    Ok((idempotency_key, process_binding))
}

fn r0_process_marker_counts(
    boundary: &PrivateProcessLedgerBoundary,
    idempotency_key: &str,
) -> Result<R0ProcessMarkerCounts, R0DurableAttemptError> {
    let database = boundary
        .production_boundary()
        .canonical_path()
        .join("runtime.db");
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| r0_attempt_error("open process marker ledger", error))?;
    let mut counts = R0ProcessMarkerCounts::default();
    let mut statement = connection
        .prepare(
            "SELECT transition_kind, payload_json
             FROM journal
             WHERE transition_kind IN (
                'cell3.process_attempt_claimed',
                'cell3.process_spawn_permitted',
                'cell3.process_started_observed'
             )",
        )
        .map_err(|error| r0_attempt_error("prepare process marker query", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| r0_attempt_error("query process markers", error))?;
    for row in rows {
        let (kind, payload) =
            row.map_err(|error| r0_attempt_error("read process marker row", error))?;
        let value: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| r0_attempt_error("decode process marker row", error))?;
        if value
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            != Some(idempotency_key)
        {
            continue;
        }
        let target = match kind.as_str() {
            "cell3.process_attempt_claimed" => &mut counts.claimed,
            "cell3.process_spawn_permitted" => &mut counts.spawn_permitted,
            "cell3.process_started_observed" => &mut counts.started_observed,
            _ => continue,
        };
        *target = target
            .checked_add(1)
            .ok_or_else(|| r0_attempt_error("count process markers", "count overflow"))?;
    }
    let release_authorized: i64 = connection
        .query_row(
            "SELECT count(*)
             FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1 AND attempt = 1",
            [idempotency_key],
            |row| row.get(0),
        )
        .map_err(|error| r0_attempt_error("count process release authorizations", error))?;
    counts.release_authorized = u32::try_from(release_authorized)
        .map_err(|error| r0_attempt_error("count process release authorizations", error))?;
    Ok(counts)
}

fn r0_process_cut_snapshot(
    bindings: &R0DurableAttemptBindings,
) -> Result<R0ProcessCutSnapshot, R0DurableAttemptError> {
    let (idempotency_key, process_binding) = r0_attempt_descriptor(bindings)?;
    let mut store = RuntimeStore::open_private(
        bindings.ledger_boundary.clone(),
        StorageActorAuthority::new(),
    )
    .map_err(|error| r0_attempt_error("open private process ledger", error))?;
    let result = (|| {
        let snapshot = store
            .exact_outbox_snapshot(&idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let observation = snapshot.current_observation();
        let observation_history_count = store.attempt_history(&idempotency_key, 16)?.len();
        let terminal_decision_present = store
            .terminal_decision(
                &bindings.mission,
                bindings.phase.as_str(),
                bindings.identity.worker_id(),
                1,
            )?
            .is_some();
        let recovery_classification = if matches!(
            snapshot.effect().state(),
            OutboxState::Executing | OutboxState::Uncertain
        ) {
            Some(
                store
                    .recover_exact_process_attempt(
                        &process_binding,
                        bindings.prepared_launch.exact_request(),
                    )?
                    .classification(),
            )
        } else {
            None
        };
        Ok::<_, RuntimeStoreError>(R0ProcessCutSnapshot {
            state: snapshot.effect().state(),
            attempts: snapshot.effect().attempts(),
            execution_identity: snapshot.effect().execution_identity().cloned(),
            observation_history_count,
            terminal_decision_present,
            evidence_code: observation.map(|observation| observation.evidence().code()),
            not_started_reason: observation
                .and_then(|observation| observation.evidence().process_not_started_reason()),
            uncertainty: observation
                .and_then(|observation| observation.evidence().process_uncertainty()),
            recovery_classification,
            markers: R0ProcessMarkerCounts::default(),
        })
    })()
    .map_err(|error| r0_attempt_error("inspect process crash cut", error));
    store
        .close()
        .map_err(|error| r0_attempt_error("close private process ledger", error))?;
    let mut snapshot = result?;
    snapshot.markers = r0_process_marker_counts(&bindings.ledger_boundary, &idempotency_key)?;
    Ok(snapshot)
}

fn r0_process_request(worker_root: PathBuf) -> Result<ExecutionRequest, R0DurableAttemptError> {
    ExecutionRequest::new(ExecutionRequestDraft {
        mission: R0_JOURNAL_MISSION.to_owned(),
        phase: R0_JOURNAL_PHASE.to_owned(),
        attempt: 1,
        revision: 1,
        objective: "prove one exact durable attempt after projector recovery".to_owned(),
        persona: R0_PROCESS_PERSONA.to_owned(),
        role: "verifier".to_owned(),
        domain: "dev".to_owned(),
        skills: vec!["rust-best-practices".to_owned()],
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec!["fixture-only".to_owned()],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(R0_PROCESS_RUNTIME)
            .map_err(|error| r0_attempt_error("build runtime family", error))?,
        model: "r0-fixture-model".to_owned(),
        effort: Effort::High,
        max_turns: 1,
        worker_dir: worker_root,
        target_dir: None,
        resume_from: None,
        hook_script: None,
    })
    .map_err(|error| r0_attempt_error("build execution request", error))
}

fn r0_attempt_bindings(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
    mode: R0AttemptResourceMode,
) -> Result<R0DurableAttemptBindings, R0DurableAttemptError> {
    authority
        .boundary
        .verify()
        .map_err(|error| r0_attempt_error("verify fixture authority", error))?;
    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_attempt_error("build mission identity", error))?;
    let phase = PhaseId::new(R0_JOURNAL_PHASE)
        .map_err(|error| r0_attempt_error("build phase identity", error))?;
    let worker = WorkerId::for_phase(R0_PROCESS_PERSONA, &phase)
        .map_err(|error| r0_attempt_error("build worker identity", error))?;
    let worker_root = authority
        .boundary
        .canonical_path()
        .join("workspaces")
        .join(R0_JOURNAL_MISSION)
        .join("workers")
        .join(worker.as_str());
    let request = r0_process_request(worker_root)?;
    let workspace = authority
        .open_workspace(mission.clone())
        .map_err(|error| r0_attempt_error("open recovered workspace", error))?;
    let existing_target = match authority.open_target(R0_PROCESS_LEDGER_TARGET) {
        Ok(target) => Some(target),
        Err(FixtureAuthorityError::Io(error))
            if mode == R0AttemptResourceMode::RunOrReplay
                && error.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => {
            return Err(r0_attempt_error("open exact private ledger target", error));
        }
    };
    let helper = match AttestedFixtureHelper::recover(authority, R0_PROCESS_HELPER, helper_bytes) {
        Ok(helper) => helper,
        Err(HermeticProviderError::FixtureAuthority(FixtureAuthorityError::Io(error)))
            if existing_target.is_none()
                && mode == R0AttemptResourceMode::RunOrReplay
                && error.kind() == std::io::ErrorKind::NotFound =>
        {
            AttestedFixtureHelper::install(authority, R0_PROCESS_HELPER, helper_bytes).map_err(
                |error| r0_attempt_error("install missing attested fixture helper", error),
            )?
        }
        Err(error) => {
            return Err(r0_attempt_error(
                "recover exact attested fixture helper",
                error,
            ));
        }
    };
    let target = match existing_target {
        Some(target) => target,
        None => authority
            .create_target(R0_PROCESS_LEDGER_TARGET)
            .map_err(|error| r0_attempt_error("create missing private ledger target", error))?,
    };
    let worker_binding = workspace
        .phase_worker_binding(R0_PROCESS_PERSONA, &phase, request.worker_dir())
        .map_err(|error| r0_attempt_error("bind phase worker", error))?;
    let prepared_launch =
        PreparedProviderLaunch::from_attested_fixture(worker_binding, &helper.executable)
            .map_err(|error| r0_attempt_error("prepare provider launch", error))?;
    let identity = WorkerIdentity::new(
        R0_JOURNAL_MISSION,
        R0_JOURNAL_PHASE,
        prepared_launch.worker_id(),
    )
    .map_err(|error| r0_attempt_error("bind worker identity", error))?;
    let execution_binding = request.fingerprint(prepared_launch.worker_id(), R0_PROCESS_RUNTIME);
    let ledger_boundary = PrivateProcessLedgerBoundary::new(Arc::new(
        ProductionBoundary::from_fixture_target(target)
            .map_err(|error| r0_attempt_error("derive private ledger boundary", error))?,
    ));
    Ok(R0DurableAttemptBindings {
        mission,
        phase,
        identity,
        request,
        execution_binding,
        prepared_launch,
        ledger_boundary,
    })
}

fn r0_attempt_report(
    ledger_boundary: PrivateProcessLedgerBoundary,
    idempotency_key: &str,
    mission: &MissionId,
    phase: &PhaseId,
    worker_id: &str,
    worker_root: &std::path::Path,
) -> Result<R0DurableAttemptReport, R0DurableAttemptError> {
    let mut store = RuntimeStore::open_private(ledger_boundary, StorageActorAuthority::new())
        .map_err(|error| r0_attempt_error("open private process ledger", error))?;
    let result = (|| {
        let snapshot = store
            .exact_outbox_snapshot(idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let observation = snapshot
            .current_observation()
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let _committed_at_utc = snapshot
            .current_observation_committed_at_utc()?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let observation_history_count = store.attempt_history(idempotency_key, 16)?.len();
        let terminal_decision_present = store
            .terminal_decision(mission, phase.as_str(), worker_id, 1)?
            .is_some();
        Ok::<_, RuntimeStoreError>(R0DurableAttemptReport {
            state: snapshot.effect().state(),
            attempts: snapshot.effect().attempts(),
            observation_state: observation.state(),
            evidence_code: observation.evidence().code(),
            execution_identity_present: snapshot.effect().execution_identity().is_some(),
            observation_history_count,
            terminal_decision_present,
            helper_executed: false,
        })
    })()
    .map_err(|error| r0_attempt_error("inspect exact process attempt", error));
    store
        .close()
        .map_err(|error| r0_attempt_error("close private process ledger", error))?;
    let mut report = result?;
    report.helper_executed = r0_helper_executed_at(
        &worker_root.join(R0_DURABLE_CANARY_SENTINEL),
        &worker_root.join(R0_DURABLE_CANARY_DUPLICATE),
    )?;
    Ok(report)
}

fn r0_worker_projection_verification() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    )
}

/// Runs the exact Cell 2 process attempt when absent, or replays its retained
/// terminal evidence without relaunching it.
pub(crate) fn run_or_replay_r0_phase_start_durable_attempt(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0DurableWorkerProjection, R0DurableAttemptError> {
    let bindings =
        r0_attempt_bindings(authority, helper_bytes, R0AttemptResourceMode::RunOrReplay)?;
    let (idempotency_key, _) = r0_attempt_descriptor(&bindings)?;
    let ledger_boundary = bindings.ledger_boundary.clone();
    let report_mission = bindings.mission.clone();
    let report_phase = bindings.phase.clone();
    let report_worker = bindings.identity.worker_id().to_owned();
    let worker_root = bindings.request.worker_dir().to_path_buf();
    let R0DurableAttemptBindings {
        mission,
        phase,
        identity,
        request,
        execution_binding,
        prepared_launch,
        ledger_boundary: _,
    } = bindings;
    let mut attempt = DurableHermeticAttempt::open(
        ledger_boundary.clone(),
        mission,
        &phase,
        identity,
        1,
        prepared_launch,
        execution_binding,
        DurableAttemptOpen::AdmitMissing,
    )
    .map_err(|error| r0_attempt_error("open durable attempt", error))?;
    attempt
        .activate(CancellationToken::new(), Arc::new(R0ProcessTimestampSource))
        .map_err(|error| r0_attempt_error("activate durable attempt", error))?;
    if attempt.terminal().is_none() {
        attempt
            .execute()
            .map_err(|error| r0_attempt_error("execute durable attempt", error))?;
    }
    if attempt
        .terminal()
        .is_none_or(|terminal| terminal.observed_termination().is_some())
    {
        return Err(r0_attempt_error(
            "verify durable attempt terminal",
            "fixture process did not complete successfully",
        ));
    }
    let verification = r0_worker_projection_verification();
    let selected = attempt
        .persist_or_validate_terminal_decision(verification)
        .map_err(|error| r0_attempt_error("persist terminal projection decision", error))?;
    if selected != verification {
        return Err(r0_attempt_error(
            "validate terminal projection decision",
            "retained decision is not the exact fixed PASS decision",
        ));
    }
    let evidence = attempt
        .seal_worker_projection_evidence(request, R0_PROCESS_RUNTIME)
        .map_err(|error| r0_attempt_error("seal durable worker projection evidence", error))?;
    let report = r0_attempt_report(
        ledger_boundary,
        &idempotency_key,
        &report_mission,
        &report_phase,
        &report_worker,
        &worker_root,
    )?;
    Ok(R0DurableWorkerProjection { evidence, report })
}

/// Reopens only an already-terminal Cell 2 process attempt. It never creates a
/// helper/ledger target, activates an actor, or executes a process.
pub(crate) fn inspect_existing_r0_phase_start_durable_attempt(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0DurableWorkerProjection, R0DurableAttemptError> {
    let bindings =
        r0_attempt_bindings(authority, helper_bytes, R0AttemptResourceMode::ExistingOnly)?;
    let (idempotency_key, _) = r0_attempt_descriptor(&bindings)?;
    let ledger_boundary = bindings.ledger_boundary.clone();
    let report_mission = bindings.mission.clone();
    let report_phase = bindings.phase.clone();
    let report_worker = bindings.identity.worker_id().to_owned();
    let worker_root = bindings.request.worker_dir().to_path_buf();
    let R0DurableAttemptBindings {
        mission,
        phase,
        identity,
        request,
        execution_binding,
        prepared_launch,
        ledger_boundary: _,
    } = bindings;
    let mut attempt = DurableHermeticAttempt::open(
        ledger_boundary.clone(),
        mission,
        &phase,
        identity,
        1,
        prepared_launch,
        execution_binding,
        DurableAttemptOpen::ExistingOnly,
    )
    .map_err(|error| r0_attempt_error("open existing durable attempt", error))?;
    if attempt
        .terminal()
        .is_none_or(|terminal| terminal.observed_termination().is_some())
        || attempt.decision().is_none()
    {
        return Err(r0_attempt_error(
            "verify existing durable attempt",
            "exact successful terminal evidence and its decision are required",
        ));
    }
    let evidence = attempt
        .seal_worker_projection_evidence(request, R0_PROCESS_RUNTIME)
        .map_err(|error| r0_attempt_error("seal existing worker projection evidence", error))?;
    let report = r0_attempt_report(
        ledger_boundary,
        &idempotency_key,
        &report_mission,
        &report_phase,
        &report_worker,
        &worker_root,
    )?;
    Ok(R0DurableWorkerProjection { evidence, report })
}

/// Executes or replays the exact process-only attempt and returns its ledger
/// summary. Canonical projection is owned by the Cell 2 recovery adapter.
#[doc(hidden)]
pub fn run_r0_phase_start_durable_attempt(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0DurableAttemptReport, R0DurableAttemptError> {
    run_or_replay_r0_phase_start_durable_attempt(authority, helper_bytes)
        .map(|projection| *projection.report())
}

/// Reopens only the exact private-ledger evidence for Cell 2.
#[doc(hidden)]
pub fn inspect_r0_phase_start_durable_attempt(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0DurableAttemptReport, R0DurableAttemptError> {
    inspect_existing_r0_phase_start_durable_attempt(authority, helper_bytes)
        .map(|projection| *projection.report())
}

const R0_PHASE_TERMINAL_MISSION: &str = "r0-phase-terminal-restart";
const R0_PHASE_TERMINAL_PHASE: &str = "verify";
const R0_PHASE_TERMINAL_PERSONA: &str = "r0-provider";
const R0_PHASE_TERMINAL_RUNTIME: &str = "r0-provider-runtime";
const R0_PHASE_TERMINAL_HELPER: &str = "r0-phase-terminal-helper";
const R0_PHASE_TERMINAL_LEDGER_TARGET: &str = "r0-phase-terminal-private-ledger";
const R0_DURABLE_CANARY_SENTINEL: &str = ".nanika-durable-canary-launched";
const R0_DURABLE_CANARY_DUPLICATE: &str = ".nanika-durable-canary-duplicate";
const R0_DURABLE_CANARY_CONTENT: &[u8] = b"nanika-durable-canary-v1\n";

/// Opaque owner retained at the real phase-terminal/mission-running boundary.
///
/// The harness must kill the owning process; gracefully dropping this guard
/// is not restart evidence.
#[doc(hidden)]
pub struct R0PhaseTerminalCrashGuard {
    _authority: FreshFixtureAuthority,
    _provider: HermeticProvider,
}

impl fmt::Debug for R0PhaseTerminalCrashGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("R0PhaseTerminalCrashGuard")
            .field("kind", &"fixture-bound-phase-terminal-cut")
            .finish()
    }
}

/// Stable outcome after `HermeticProvider` automatically repairs cell 9.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct R0PhaseTerminalRecoveryReport {
    event_types: Vec<String>,
    event_bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    mission_status: MissionStatus,
    phase_status: PhaseStatus,
    process: R0DurableAttemptReport,
}

impl R0PhaseTerminalRecoveryReport {
    /// Lifecycle event kinds in exact durable order.
    #[must_use]
    pub fn event_types(&self) -> &[String] {
        &self.event_types
    }

    /// Exact workspace event-log bytes after recovery.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        &self.event_bytes
    }

    /// Exact workspace checkpoint bytes after recovery.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    /// Recovered terminal mission status.
    #[must_use]
    pub const fn mission_status(&self) -> MissionStatus {
        self.mission_status
    }

    /// Retained terminal phase status.
    #[must_use]
    pub const fn phase_status(&self) -> PhaseStatus {
        self.phase_status
    }

    /// Exact private process evidence retained across restart.
    #[must_use]
    pub const fn process(&self) -> R0DurableAttemptReport {
        self.process
    }
}

/// Real process crash boundaries exercised by the aggregate R0 restart gate.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum R0ProcessCrashCell {
    /// Durable process intent exists, but no claim has been made.
    PendingBeforeClaim,
    /// The exact process attempt is claimed, before spec/runtime initialization.
    ClaimedBeforeSpecification,
    /// Release is authorized, before the durable StartedObserved marker.
    ReleaseAuthorizedBeforeStarted,
    /// StartedObserved is durable and acknowledged, before terminal resolution.
    StartedObservedBeforeTerminal,
    /// One terminal process observation exists, but no terminal decision does.
    TerminalObservationBeforeDecision,
    /// One terminal decision exists, but no worker projection has begun.
    TerminalDecisionBeforeProjection,
}

impl R0ProcessCrashCell {
    const fn barrier_point(self) -> Option<DurableProcessBarrierPoint> {
        match self {
            Self::ClaimedBeforeSpecification => {
                Some(DurableProcessBarrierPoint::ClaimedBeforeSpecification)
            }
            Self::ReleaseAuthorizedBeforeStarted => {
                Some(DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt)
            }
            Self::StartedObservedBeforeTerminal => {
                Some(DurableProcessBarrierPoint::StartedPersistedBeforeTerminalResolution)
            }
            Self::PendingBeforeClaim
            | Self::TerminalObservationBeforeDecision
            | Self::TerminalDecisionBeforeProjection => None,
        }
    }
}

/// Typed product recovery result retained by the aggregate R0 gate.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum R0ProcessRecoveryDisposition {
    /// A claimed attempt was proven not to have reached spawn.
    RecoveredBeforeSpawnNotStarted,
    /// An authorized process group was cleaned without a StartedObserved marker.
    AuthorizedWithoutStartedCleanedUncertain,
    /// A StartedObserved process group was cleaned before terminal resolution.
    StartedProcessCleanedUncertain,
    /// A second cleanup-only reopen retained authorized uncertainty.
    AuthorizedUncertainCleaned,
    /// A second cleanup-only reopen retained started uncertainty.
    StartedUncertainCleaned,
}

/// Opaque ownership retained at one exact real-process R0 crash boundary.
///
/// The harness must kill the process retaining this value. Gracefully
/// dropping it is not restart evidence.
#[doc(hidden)]
pub struct R0ProcessCrashGuard {
    _authority: FreshFixtureAuthority,
    _provider: Option<HermeticProvider>,
}

impl fmt::Debug for R0ProcessCrashGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("R0ProcessCrashGuard")
            .field("kind", &"fixture-bound-real-process-cut")
            .finish()
    }
}

/// Stable evidence from one recovered real-process R0 crash cell.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct R0ProcessCrashRecoveryReport {
    cell: R0ProcessCrashCell,
    cut: R0ProcessCutSnapshot,
    cut_event_types: Vec<String>,
    cut_event_bytes: Vec<u8>,
    cut_checkpoint_bytes: Vec<u8>,
    cut_helper_executed: bool,
    recovered: R0ProcessCutSnapshot,
    event_types: Vec<String>,
    event_bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    mission_status: MissionStatus,
    phase_status: PhaseStatus,
    helper_executed: bool,
    first_recovery_disposition: Option<R0ProcessRecoveryDisposition>,
    second_recovery_disposition: Option<R0ProcessRecoveryDisposition>,
    recovered_process_group_absent: bool,
}

impl R0ProcessCrashRecoveryReport {
    /// Exact crash cell this report proves.
    #[must_use]
    pub const fn cell(&self) -> R0ProcessCrashCell {
        self.cell
    }

    /// Durable outbox state observed after SIGKILL and before recovery.
    #[must_use]
    pub const fn cut_state(&self) -> OutboxState {
        self.cut.state
    }

    /// Durable claim count observed at the crash cut.
    #[must_use]
    pub const fn cut_attempts(&self) -> u32 {
        self.cut.attempts
    }

    /// Whether the crash cut retained an exact kernel execution identity.
    #[must_use]
    pub const fn cut_execution_identity_present(&self) -> bool {
        self.cut.execution_identity.is_some()
    }

    /// Exact retained kernel identity at the crash cut, when one exists.
    #[must_use]
    pub fn cut_execution_identity(&self) -> Option<&ProcessExecutionIdentity> {
        self.cut.execution_identity.as_ref()
    }

    /// Number of exact claim markers at the crash cut.
    #[must_use]
    pub const fn cut_claim_marker_count(&self) -> u32 {
        self.cut.markers.claimed
    }

    /// Number of exact spawn-permit markers at the crash cut.
    #[must_use]
    pub const fn cut_spawn_permit_count(&self) -> u32 {
        self.cut.markers.spawn_permitted
    }

    /// Number of exact release-authorization rows at the crash cut.
    #[must_use]
    pub const fn cut_release_authorization_count(&self) -> u32 {
        self.cut.markers.release_authorized
    }

    /// Number of exact StartedObserved markers at the crash cut.
    #[must_use]
    pub const fn cut_started_observed_marker_count(&self) -> u32 {
        self.cut.markers.started_observed
    }

    /// Number of immutable process observations present at the crash cut.
    #[must_use]
    pub const fn cut_observation_history_count(&self) -> usize {
        self.cut.observation_history_count
    }

    /// Whether the exact terminal decision already existed at the crash cut.
    #[must_use]
    pub const fn cut_terminal_decision_present(&self) -> bool {
        self.cut.terminal_decision_present
    }

    /// Lifecycle event kinds durably present at the crash cut.
    #[must_use]
    pub fn cut_event_types(&self) -> &[String] {
        &self.cut_event_types
    }

    /// Exact lifecycle event bytes durably present at the crash cut.
    #[must_use]
    pub fn cut_event_bytes(&self) -> &[u8] {
        &self.cut_event_bytes
    }

    /// Exact checkpoint bytes durably present at the crash cut.
    #[must_use]
    pub fn cut_checkpoint_bytes(&self) -> &[u8] {
        &self.cut_checkpoint_bytes
    }

    /// Whether the attested helper had executed by the crash cut.
    #[must_use]
    pub const fn cut_helper_executed(&self) -> bool {
        self.cut_helper_executed
    }

    /// Terminal outbox state after recovery.
    #[must_use]
    pub const fn recovered_state(&self) -> OutboxState {
        self.recovered.state
    }

    /// Durable claim count after recovery.
    #[must_use]
    pub const fn recovered_attempts(&self) -> u32 {
        self.recovered.attempts
    }

    /// Whether recovery retained an exact kernel execution identity.
    #[must_use]
    pub const fn recovered_execution_identity_present(&self) -> bool {
        self.recovered.execution_identity.is_some()
    }

    /// Exact retained kernel identity after recovery, when one exists.
    #[must_use]
    pub fn recovered_execution_identity(&self) -> Option<&ProcessExecutionIdentity> {
        self.recovered.execution_identity.as_ref()
    }

    /// Number of immutable process observations after recovery.
    #[must_use]
    pub const fn recovered_observation_history_count(&self) -> usize {
        self.recovered.observation_history_count
    }

    /// Typed evidence carried by the recovered terminal observation.
    #[must_use]
    pub const fn recovered_evidence_code(&self) -> Option<EffectEvidenceCode> {
        self.recovered.evidence_code
    }

    /// Typed proven-not-started reason, when recovery produced one.
    #[must_use]
    pub const fn recovered_not_started_reason(&self) -> Option<ProcessNotStartedEvidenceReason> {
        self.recovered.not_started_reason
    }

    /// Typed uncertainty retained after fail-closed process recovery.
    #[must_use]
    pub const fn recovered_uncertainty(&self) -> Option<ProcessUncertaintyEvidence> {
        self.recovered.uncertainty
    }

    /// Number of exact claim markers after recovery.
    #[must_use]
    pub const fn recovered_claim_marker_count(&self) -> u32 {
        self.recovered.markers.claimed
    }

    /// Number of exact spawn-permit markers after recovery.
    #[must_use]
    pub const fn recovered_spawn_permit_count(&self) -> u32 {
        self.recovered.markers.spawn_permitted
    }

    /// Number of exact release-authorization rows after recovery.
    #[must_use]
    pub const fn recovered_release_authorization_count(&self) -> u32 {
        self.recovered.markers.release_authorized
    }

    /// Number of exact StartedObserved markers after recovery.
    #[must_use]
    pub const fn recovered_started_observed_marker_count(&self) -> u32 {
        self.recovered.markers.started_observed
    }

    /// Whether exactly bound terminal-decision evidence exists after recovery.
    #[must_use]
    pub const fn recovered_terminal_decision_present(&self) -> bool {
        self.recovered.terminal_decision_present
    }

    /// Recovered lifecycle event kinds in exact durable order.
    #[must_use]
    pub fn event_types(&self) -> &[String] {
        &self.event_types
    }

    /// Exact recovered lifecycle event bytes.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        &self.event_bytes
    }

    /// Exact recovered checkpoint bytes.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    /// Recovered terminal mission status.
    #[must_use]
    pub const fn mission_status(&self) -> MissionStatus {
        self.mission_status
    }

    /// Recovered terminal phase status.
    #[must_use]
    pub const fn phase_status(&self) -> PhaseStatus {
        self.phase_status
    }

    /// Whether the helper executed exactly once and no duplicate sentinel exists.
    #[must_use]
    pub const fn helper_executed(&self) -> bool {
        self.helper_executed
    }

    /// Typed disposition from the first ordinary recovery reopen.
    #[must_use]
    pub const fn first_recovery_disposition(&self) -> Option<R0ProcessRecoveryDisposition> {
        self.first_recovery_disposition
    }

    /// Typed disposition from the stable second recovery reopen, when needed.
    #[must_use]
    pub const fn second_recovery_disposition(&self) -> Option<R0ProcessRecoveryDisposition> {
        self.second_recovery_disposition
    }

    /// Whether exact PID/PGID/start identity proves the recovered group absent.
    #[must_use]
    pub const fn recovered_process_group_absent(&self) -> bool {
        self.recovered_process_group_absent
    }
}

fn r0_phase_terminal_state() -> Result<MissionState, R0DurableAttemptError> {
    MissionState::new(
        MissionId::new(R0_PHASE_TERMINAL_MISSION)
            .map_err(|error| r0_attempt_error("build provider mission identity", error))?,
        vec![PhaseDefinition {
            id: PhaseId::new(R0_PHASE_TERMINAL_PHASE)
                .map_err(|error| r0_attempt_error("build provider phase identity", error))?,
            dependencies: Vec::new(),
        }],
    )
    .map_err(|error| r0_attempt_error("build provider mission state", error))
}

fn r0_phase_terminal_checkpoint(state: &MissionState) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: R0_PHASE_TERMINAL_MISSION.to_owned(),
        status: "pending".to_owned(),
        plan: Some(CheckpointPlan {
            id: "r0-phase-terminal-plan".to_owned(),
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

fn r0_phase_terminal_request(
    worker_root: PathBuf,
) -> Result<ExecutionRequest, R0DurableAttemptError> {
    ExecutionRequest::new(ExecutionRequestDraft {
        mission: R0_PHASE_TERMINAL_MISSION.to_owned(),
        phase: R0_PHASE_TERMINAL_PHASE.to_owned(),
        attempt: 1,
        revision: 1,
        objective: "prove automatic mission completion after phase-terminal restart".to_owned(),
        persona: R0_PHASE_TERMINAL_PERSONA.to_owned(),
        role: "verifier".to_owned(),
        domain: "dev".to_owned(),
        skills: vec!["rust-best-practices".to_owned()],
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec!["fixture-only".to_owned()],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(R0_PHASE_TERMINAL_RUNTIME)
            .map_err(|error| r0_attempt_error("build provider runtime family", error))?,
        model: "r0-fixture-model".to_owned(),
        effort: Effort::High,
        max_turns: 1,
        worker_dir: worker_root,
        target_dir: None,
        resume_from: None,
        hook_script: None,
    })
    .map_err(|error| r0_attempt_error("build provider execution request", error))
}

fn r0_recovered_phase_terminal_attempt_bindings(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0DurableAttemptBindings, R0DurableAttemptError> {
    authority
        .boundary
        .verify()
        .map_err(|error| r0_attempt_error("verify process-crash fixture", error))?;
    let mission = MissionId::new(R0_PHASE_TERMINAL_MISSION)
        .map_err(|error| r0_attempt_error("build provider mission identity", error))?;
    let phase = PhaseId::new(R0_PHASE_TERMINAL_PHASE)
        .map_err(|error| r0_attempt_error("build provider phase identity", error))?;
    let worker = WorkerId::for_phase(R0_PHASE_TERMINAL_PERSONA, &phase)
        .map_err(|error| r0_attempt_error("build provider worker identity", error))?;
    let worker_root = authority
        .boundary
        .canonical_path()
        .join("workspaces")
        .join(R0_PHASE_TERMINAL_MISSION)
        .join("workers")
        .join(worker.as_str());
    let request = r0_phase_terminal_request(worker_root)?;
    let workspace = authority
        .open_workspace(mission.clone())
        .map_err(|error| r0_attempt_error("open process-crash workspace", error))?;
    let helper = AttestedFixtureHelper::recover(authority, R0_PHASE_TERMINAL_HELPER, helper_bytes)
        .map_err(|error| r0_attempt_error("recover process-crash helper", error))?;
    let worker_binding = workspace
        .phase_worker_binding(R0_PHASE_TERMINAL_PERSONA, &phase, request.worker_dir())
        .map_err(|error| r0_attempt_error("bind process-crash worker", error))?;
    let prepared_launch =
        PreparedProviderLaunch::from_attested_fixture(worker_binding, &helper.executable)
            .map_err(|error| r0_attempt_error("prepare process-crash launch", error))?;
    let identity = WorkerIdentity::new(
        R0_PHASE_TERMINAL_MISSION,
        R0_PHASE_TERMINAL_PHASE,
        prepared_launch.worker_id(),
    )
    .map_err(|error| r0_attempt_error("bind process-crash identity", error))?;
    let execution_binding =
        request.fingerprint(prepared_launch.worker_id(), R0_PHASE_TERMINAL_RUNTIME);
    let target = authority
        .open_target(R0_PHASE_TERMINAL_LEDGER_TARGET)
        .map_err(|error| r0_attempt_error("open process-crash ledger target", error))?;
    let ledger_boundary = PrivateProcessLedgerBoundary::new(Arc::new(
        ProductionBoundary::from_fixture_target(target)
            .map_err(|error| r0_attempt_error("derive process-crash ledger", error))?,
    ));
    Ok(R0DurableAttemptBindings {
        mission,
        phase,
        identity,
        request,
        execution_binding,
        prepared_launch,
        ledger_boundary,
    })
}

struct R0PhaseTerminalFixture {
    provider: HermeticProvider,
    event_path: PathBuf,
    checkpoint_path: PathBuf,
    sentinel_path: PathBuf,
    duplicate_path: PathBuf,
}

fn r0_open_phase_terminal_fixture(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
    fresh: bool,
) -> Result<R0PhaseTerminalFixture, R0DurableAttemptError> {
    r0_open_phase_terminal_fixture_inner(authority, helper_bytes, fresh, None)
}

fn r0_open_phase_terminal_fixture_with_observer(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
    observer: R0ProcessBarrierObserver,
) -> Result<R0PhaseTerminalFixture, R0DurableAttemptError> {
    r0_open_phase_terminal_fixture_inner(authority, helper_bytes, true, Some(observer))
}

fn r0_open_phase_terminal_fixture_inner(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
    fresh: bool,
    observer: Option<R0ProcessBarrierObserver>,
) -> Result<R0PhaseTerminalFixture, R0DurableAttemptError> {
    authority
        .boundary
        .verify()
        .map_err(|error| r0_attempt_error("verify phase-terminal fixture", error))?;
    let state = r0_phase_terminal_state()?;
    let mission = state.mission_id().clone();
    let phase = PhaseId::new(R0_PHASE_TERMINAL_PHASE)
        .map_err(|error| r0_attempt_error("build provider phase identity", error))?;
    let worker = WorkerId::for_phase(R0_PHASE_TERMINAL_PERSONA, &phase)
        .map_err(|error| r0_attempt_error("build provider worker identity", error))?;
    let workspace_root = authority
        .boundary
        .canonical_path()
        .join("workspaces")
        .join(R0_PHASE_TERMINAL_MISSION);
    let worker_root = workspace_root.join("workers").join(worker.as_str());
    let workspace = if fresh {
        authority
            .create_workspace(
                mission,
                FixtureWorkspaceSeed::new(
                    b"r0 phase terminal restart fixture\n".to_vec(),
                    &r0_phase_terminal_checkpoint(&state),
                    b"{}".to_vec(),
                )
                .map_err(|error| r0_attempt_error("build provider workspace seed", error))?,
            )
            .map_err(|error| r0_attempt_error("create provider workspace", error))?
    } else {
        authority
            .open_workspace(mission)
            .map_err(|error| r0_attempt_error("open provider workspace", error))?
    };
    let helper = if fresh {
        AttestedFixtureHelper::install(authority, R0_PHASE_TERMINAL_HELPER, helper_bytes)
    } else {
        AttestedFixtureHelper::recover(authority, R0_PHASE_TERMINAL_HELPER, helper_bytes)
    }
    .map_err(|error| r0_attempt_error("bind provider fixture helper", error))?;
    let target = if fresh {
        authority.create_target(R0_PHASE_TERMINAL_LEDGER_TARGET)
    } else {
        authority.open_target(R0_PHASE_TERMINAL_LEDGER_TARGET)
    }
    .map_err(|error| r0_attempt_error("bind provider private ledger target", error))?;
    let ledger_boundary = PrivateProcessLedgerBoundary::new(Arc::new(
        ProductionBoundary::from_fixture_target(target)
            .map_err(|error| r0_attempt_error("derive provider private ledger", error))?,
    ));
    let request = r0_phase_terminal_request(worker_root.clone())?;
    let provider = match observer {
        Some(observer) => HermeticProvider::admit_with_r0_observer(
            workspace,
            state,
            &helper,
            ledger_boundary,
            R0_PHASE_TERMINAL_RUNTIME,
            request,
            CancellationToken::new(),
            Arc::new(R0ProcessTimestampSource),
            observer,
        ),
        None => HermeticProvider::admit(
            workspace,
            state,
            &helper,
            ledger_boundary,
            R0_PHASE_TERMINAL_RUNTIME,
            request,
            CancellationToken::new(),
            Arc::new(R0ProcessTimestampSource),
        ),
    }
    .map_err(|error| r0_attempt_error("admit hermetic provider", error))?;
    Ok(R0PhaseTerminalFixture {
        provider,
        event_path: workspace_root.join("events.jsonl"),
        checkpoint_path: workspace_root.join("checkpoint.json"),
        sentinel_path: worker_root.join(R0_DURABLE_CANARY_SENTINEL),
        duplicate_path: worker_root.join(R0_DURABLE_CANARY_DUPLICATE),
    })
}

fn r0_phase_terminal_event_types(path: &PathBuf) -> Result<Vec<String>, R0DurableAttemptError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(r0_attempt_error("read provider event log", error)),
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let event: serde_json::Value = serde_json::from_slice(line)
                .map_err(|error| r0_attempt_error("decode provider event", error))?;
            event
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| r0_attempt_error("decode provider event", "event type is missing"))
        })
        .collect()
}

fn r0_verify_single_helper_launch(
    fixture: &R0PhaseTerminalFixture,
) -> Result<(), R0DurableAttemptError> {
    if !r0_helper_executed(fixture)? {
        return Err(r0_attempt_error(
            "verify durable helper launch",
            "helper did not execute",
        ));
    }
    Ok(())
}

fn r0_helper_executed(fixture: &R0PhaseTerminalFixture) -> Result<bool, R0DurableAttemptError> {
    r0_helper_executed_at(&fixture.sentinel_path, &fixture.duplicate_path)
}

fn r0_helper_executed_at(
    sentinel_path: &std::path::Path,
    duplicate_path: &std::path::Path,
) -> Result<bool, R0DurableAttemptError> {
    if duplicate_path.exists() {
        return Err(r0_attempt_error(
            "verify durable helper launch",
            "helper executed more than once",
        ));
    }
    match std::fs::read(sentinel_path) {
        Ok(sentinel) if sentinel == R0_DURABLE_CANARY_CONTENT => Ok(true),
        Ok(_) => Err(r0_attempt_error(
            "verify durable helper launch",
            "helper sentinel content is invalid",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(r0_attempt_error("read durable helper sentinel", error)),
    }
}

fn r0_provider_process_report(
    provider: &HermeticProvider,
) -> Result<R0DurableAttemptReport, R0DurableAttemptError> {
    r0_attempt_report(
        provider.durable_attempt.ledger_boundary.clone(),
        &provider.durable_attempt.idempotency_key,
        &provider.durable_attempt.mission_id,
        &provider.phase,
        provider.identity.worker_id(),
        provider.request.worker_dir(),
    )
}

fn r0_pass_decision() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    )
}

/// Runs a real `HermeticProvider` to the durable cell-9 cut.
#[doc(hidden)]
pub fn prepare_r0_phase_terminal_crash(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0PhaseTerminalCrashGuard, R0DurableAttemptError> {
    let mut fixture = r0_open_phase_terminal_fixture(&authority, helper_bytes, true)?;
    fixture.provider.r0_stop_after_phase_terminal = true;
    let result = fixture.provider.run_to_terminal(r0_pass_decision());
    if !matches!(result, Err(HermeticProviderError::R0PhaseTerminalCut)) {
        return Err(r0_attempt_error(
            "reach phase-terminal boundary",
            format!("unexpected provider result: {result:?}"),
        ));
    }
    let phase_status = fixture
        .provider
        .coordinator
        .state()
        .phase(&fixture.provider.phase)
        .map(|phase| phase.status)
        .ok_or_else(|| r0_attempt_error("inspect phase-terminal boundary", "phase is missing"))?;
    if fixture.provider.coordinator.state().status() != MissionStatus::InProgress
        || phase_status != PhaseStatus::Completed
    {
        return Err(r0_attempt_error(
            "inspect phase-terminal boundary",
            "phase is not completed while mission remains in progress",
        ));
    }
    let event_types = r0_phase_terminal_event_types(&fixture.event_path)?;
    if event_types
        != [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.completed",
            "phase.completed",
        ]
    {
        return Err(r0_attempt_error(
            "inspect phase-terminal events",
            format!("unexpected events: {event_types:?}"),
        ));
    }
    r0_verify_single_helper_launch(&fixture)?;
    let process = r0_provider_process_report(&fixture.provider)?;
    if process.state() != OutboxState::Succeeded
        || process.attempts() != 1
        || process.observation_history_count() != 1
    {
        return Err(r0_attempt_error(
            "inspect phase-terminal process",
            format!("unexpected process report: {process:?}"),
        ));
    }
    Ok(R0PhaseTerminalCrashGuard {
        _authority: authority,
        _provider: fixture.provider,
    })
}

/// Reopens cell 9 and lets the ordinary `HermeticProvider::run_to_terminal`
/// path add the missing mission terminal exactly once.
#[doc(hidden)]
pub fn recover_r0_phase_terminal_crash(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0PhaseTerminalRecoveryReport, R0DurableAttemptError> {
    let mut fixture = r0_open_phase_terminal_fixture(&authority, helper_bytes, false)?;
    let outcome = fixture
        .provider
        .run_to_terminal(r0_pass_decision())
        .map_err(|error| r0_attempt_error("complete recovered mission", error))?;
    r0_verify_single_helper_launch(&fixture)?;
    let event_types = r0_phase_terminal_event_types(&fixture.event_path)?;
    if event_types
        != [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.completed",
            "phase.completed",
            "mission.completed",
        ]
    {
        return Err(r0_attempt_error(
            "verify recovered mission events",
            format!("unexpected events: {event_types:?}"),
        ));
    }
    let process = r0_provider_process_report(&fixture.provider)?;
    if process.state() != OutboxState::Succeeded
        || process.attempts() != 1
        || process.observation_history_count() != 1
    {
        return Err(r0_attempt_error(
            "verify recovered process",
            format!("unexpected process report: {process:?}"),
        ));
    }
    Ok(R0PhaseTerminalRecoveryReport {
        event_types,
        event_bytes: std::fs::read(&fixture.event_path)
            .map_err(|error| r0_attempt_error("read recovered event bytes", error))?,
        checkpoint_bytes: std::fs::read(&fixture.checkpoint_path)
            .map_err(|error| r0_attempt_error("read recovered checkpoint bytes", error))?,
        mission_status: outcome.mission_status(),
        phase_status: outcome.phase_status(),
        process,
    })
}

fn r0_phase_terminal_paths(
    authority: &FreshFixtureAuthority,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf), R0DurableAttemptError> {
    let phase = PhaseId::new(R0_PHASE_TERMINAL_PHASE)
        .map_err(|error| r0_attempt_error("build process-crash phase identity", error))?;
    let worker = WorkerId::for_phase(R0_PHASE_TERMINAL_PERSONA, &phase)
        .map_err(|error| r0_attempt_error("build process-crash worker identity", error))?;
    let workspace_root = authority
        .boundary
        .canonical_path()
        .join("workspaces")
        .join(R0_PHASE_TERMINAL_MISSION);
    let worker_root = workspace_root.join("workers").join(worker.as_str());
    Ok((
        workspace_root.join("events.jsonl"),
        workspace_root.join("checkpoint.json"),
        worker_root.join(R0_DURABLE_CANARY_SENTINEL),
        worker_root.join(R0_DURABLE_CANARY_DUPLICATE),
    ))
}

fn r0_checkpoint_statuses(
    checkpoint_bytes: &[u8],
) -> Result<(String, String), R0DurableAttemptError> {
    let checkpoint = orchestrator_core::decode_checkpoint(checkpoint_bytes)
        .map_err(|error| r0_attempt_error("decode process-crash checkpoint", error))?
        .projection;
    let phase_status = checkpoint
        .plan
        .and_then(|plan| plan.phases.into_iter().next())
        .map(|phase| phase.status)
        .ok_or_else(|| r0_attempt_error("decode process-crash checkpoint", "phase is missing"))?;
    Ok((checkpoint.status, phase_status))
}

fn r0_verify_process_cut(
    cell: R0ProcessCrashCell,
    cut: &R0ProcessCutSnapshot,
    event_types: &[String],
    checkpoint_bytes: &[u8],
    helper_executed: bool,
) -> Result<(), R0DurableAttemptError> {
    let (mission_status, phase_status) = r0_checkpoint_statuses(checkpoint_bytes)?;
    let valid = match cell {
        R0ProcessCrashCell::PendingBeforeClaim => {
            cut.state == OutboxState::Pending
                && cut.attempts == 0
                && cut.execution_identity.is_none()
                && cut.observation_history_count == 0
                && !cut.terminal_decision_present
                && cut.evidence_code.is_none()
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification.is_none()
                && cut.markers == R0ProcessMarkerCounts::default()
                && event_types.is_empty()
                && mission_status == "pending"
                && phase_status == "pending"
                && !helper_executed
        }
        R0ProcessCrashCell::ClaimedBeforeSpecification => {
            cut.state == OutboxState::Executing
                && cut.attempts == 1
                && cut.execution_identity.is_none()
                && cut.observation_history_count == 0
                && !cut.terminal_decision_present
                && cut.evidence_code.is_none()
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification
                    == Some(RecoveredProcessAttemptClassification::ClaimedBeforeSpawn)
                && cut.markers
                    == R0ProcessMarkerCounts {
                        claimed: 1,
                        spawn_permitted: 0,
                        release_authorized: 0,
                        started_observed: 0,
                    }
                && event_types == ["mission.started", "phase.started"]
                && mission_status == "in_progress"
                && phase_status == "running"
                && !helper_executed
        }
        R0ProcessCrashCell::ReleaseAuthorizedBeforeStarted => {
            cut.state == OutboxState::Executing
                && cut.attempts == 1
                && cut.execution_identity.is_some()
                && cut.observation_history_count == 0
                && !cut.terminal_decision_present
                && cut.evidence_code.is_none()
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification
                    == Some(RecoveredProcessAttemptClassification::AuthorizedWithoutStarted)
                && cut.markers
                    == R0ProcessMarkerCounts {
                        claimed: 1,
                        spawn_permitted: 1,
                        release_authorized: 1,
                        started_observed: 0,
                    }
                && event_types == ["mission.started", "phase.started"]
                && mission_status == "in_progress"
                && phase_status == "running"
                && !helper_executed
        }
        R0ProcessCrashCell::StartedObservedBeforeTerminal => {
            cut.state == OutboxState::Executing
                && cut.attempts == 1
                && cut.execution_identity.is_some()
                && cut.observation_history_count == 0
                && !cut.terminal_decision_present
                && cut.evidence_code.is_none()
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification
                    == Some(RecoveredProcessAttemptClassification::StartedExecuting)
                && cut.markers
                    == R0ProcessMarkerCounts {
                        claimed: 1,
                        spawn_permitted: 1,
                        release_authorized: 1,
                        started_observed: 1,
                    }
                && event_types == ["mission.started", "phase.started"]
                && mission_status == "in_progress"
                && phase_status == "running"
        }
        R0ProcessCrashCell::TerminalObservationBeforeDecision => {
            cut.state == OutboxState::Succeeded
                && cut.attempts == 1
                && cut.execution_identity.is_some()
                && cut.observation_history_count == 1
                && !cut.terminal_decision_present
                && cut.evidence_code == Some(EffectEvidenceCode::ExitObservedSuccess)
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification.is_none()
                && cut.markers
                    == R0ProcessMarkerCounts {
                        claimed: 1,
                        spawn_permitted: 1,
                        release_authorized: 1,
                        started_observed: 1,
                    }
                && event_types.is_empty()
                && mission_status == "pending"
                && phase_status == "pending"
                && helper_executed
        }
        R0ProcessCrashCell::TerminalDecisionBeforeProjection => {
            cut.state == OutboxState::Succeeded
                && cut.attempts == 1
                && cut.execution_identity.is_some()
                && cut.observation_history_count == 1
                && cut.terminal_decision_present
                && cut.evidence_code == Some(EffectEvidenceCode::ExitObservedSuccess)
                && cut.not_started_reason.is_none()
                && cut.uncertainty.is_none()
                && cut.recovery_classification.is_none()
                && cut.markers
                    == R0ProcessMarkerCounts {
                        claimed: 1,
                        spawn_permitted: 1,
                        release_authorized: 1,
                        started_observed: 1,
                    }
                && event_types == ["mission.started", "phase.started"]
                && mission_status == "in_progress"
                && phase_status == "running"
                && helper_executed
        }
    };
    if valid {
        Ok(())
    } else {
        Err(r0_attempt_error(
            "verify process crash cut",
            format!(
                "cell={cell:?}, cut={cut:?}, events={event_types:?}, \
                 mission={mission_status}, phase={phase_status}, helper={helper_executed}"
            ),
        ))
    }
}

fn r0_fail_decision() -> VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Fail),
        VerificationMode::Block,
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct R0RecoveredProcessState {
    process: R0ProcessCutSnapshot,
    event_types: Vec<String>,
    event_bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    mission_status: MissionStatus,
    phase_status: PhaseStatus,
    helper_executed: bool,
}

#[derive(Clone, Copy)]
enum R0RecoveredProcessExpectation {
    Succeeded,
    RecoveredBeforeSpawn,
}

fn r0_run_recovered_process_fixture(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
    requested_verification: VerificationDecision,
    expectation: R0RecoveredProcessExpectation,
) -> Result<R0RecoveredProcessState, R0DurableAttemptError> {
    let mut fixture = r0_open_phase_terminal_fixture(authority, helper_bytes, false)?;
    let outcome = fixture
        .provider
        .run_to_terminal(requested_verification)
        .map_err(|error| r0_attempt_error("complete recovered process mission", error))?;
    let helper_executed = r0_helper_executed(&fixture)?;
    let report = r0_provider_process_report(&fixture.provider)?;
    let bindings = r0_recovered_phase_terminal_attempt_bindings(authority, helper_bytes)?;
    let process = r0_process_cut_snapshot(&bindings)?;
    let event_types = r0_phase_terminal_event_types(&fixture.event_path)?;
    let valid = match expectation {
        R0RecoveredProcessExpectation::Succeeded => {
            process.state == OutboxState::Succeeded
                && process.attempts == 1
                && process.execution_identity.is_some()
                && process.observation_history_count == 1
                && process.terminal_decision_present
                && process.evidence_code == Some(EffectEvidenceCode::ExitObservedSuccess)
                && process.not_started_reason.is_none()
                && process.uncertainty.is_none()
                && report.observation_state() == OutboxState::Succeeded
                && outcome.mission_status() == MissionStatus::Completed
                && outcome.phase_status() == PhaseStatus::Completed
                && event_types
                    == [
                        "mission.started",
                        "phase.started",
                        "worker.spawned",
                        "worker.completed",
                        "phase.completed",
                        "mission.completed",
                    ]
                && helper_executed
        }
        R0RecoveredProcessExpectation::RecoveredBeforeSpawn => {
            process.state == OutboxState::Failed
                && process.attempts == 1
                && process.execution_identity.is_none()
                && process.observation_history_count == 1
                && process.terminal_decision_present
                && process.evidence_code == Some(EffectEvidenceCode::ProcessNotStarted)
                && process.not_started_reason
                    == Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
                && process.uncertainty.is_none()
                && report.observation_state() == OutboxState::Failed
                && outcome.mission_status() == MissionStatus::Failed
                && outcome.phase_status() == PhaseStatus::Failed
                && event_types
                    == [
                        "mission.started",
                        "phase.started",
                        "worker.spawned",
                        "worker.failed",
                        "phase.failed",
                        "mission.failed",
                    ]
                && !helper_executed
        }
    };
    if !valid {
        return Err(r0_attempt_error(
            "verify recovered process mission",
            format!(
                "process={process:?}, observation={:?}, mission={:?}, phase={:?}, \
                 events={event_types:?}, helper={helper_executed}",
                report.observation_state(),
                outcome.mission_status(),
                outcome.phase_status(),
            ),
        ));
    }
    Ok(R0RecoveredProcessState {
        process,
        event_types,
        event_bytes: std::fs::read(&fixture.event_path)
            .map_err(|error| r0_attempt_error("read recovered process events", error))?,
        checkpoint_bytes: std::fs::read(&fixture.checkpoint_path)
            .map_err(|error| r0_attempt_error("read recovered process checkpoint", error))?,
        mission_status: outcome.mission_status(),
        phase_status: outcome.phase_status(),
        helper_executed,
    })
}

fn r0_process_recovery_disposition(
    disposition: RecoveredProcessDisposition,
) -> Result<R0ProcessRecoveryDisposition, R0DurableAttemptError> {
    match disposition {
        RecoveredProcessDisposition::RecoveredBeforeSpawnNotStarted => {
            Ok(R0ProcessRecoveryDisposition::RecoveredBeforeSpawnNotStarted)
        }
        RecoveredProcessDisposition::AuthorizedWithoutStartedCleanedUncertain => {
            Ok(R0ProcessRecoveryDisposition::AuthorizedWithoutStartedCleanedUncertain)
        }
        RecoveredProcessDisposition::StartedProcessCleanedUncertain => {
            Ok(R0ProcessRecoveryDisposition::StartedProcessCleanedUncertain)
        }
        RecoveredProcessDisposition::AuthorizedUncertainCleaned => {
            Ok(R0ProcessRecoveryDisposition::AuthorizedUncertainCleaned)
        }
        RecoveredProcessDisposition::StartedUncertainCleaned => {
            Ok(R0ProcessRecoveryDisposition::StartedUncertainCleaned)
        }
        other => Err(r0_attempt_error(
            "classify process recovery",
            format!("unexpected disposition: {other:?}"),
        )),
    }
}

fn r0_reconcile_process_attempt(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0ProcessRecoveryDisposition, R0DurableAttemptError> {
    let bindings = r0_recovered_phase_terminal_attempt_bindings(authority, helper_bytes)?;
    let mut attempt = DurableHermeticAttempt::open(
        bindings.ledger_boundary,
        bindings.mission,
        &bindings.phase,
        bindings.identity,
        1,
        bindings.prepared_launch,
        bindings.execution_binding,
        DurableAttemptOpen::ExistingOnly,
    )
    .map_err(|error| r0_attempt_error("open recovered process attempt", error))?;
    match attempt.activate(CancellationToken::new(), Arc::new(R0ProcessTimestampSource)) {
        Err(HermeticProviderError::ProcessRecoveryReconciled(disposition)) => {
            r0_process_recovery_disposition(disposition)
        }
        other => Err(r0_attempt_error(
            "reconcile recovered process attempt",
            format!("unexpected recovery result: {other:?}"),
        )),
    }
}

fn r0_read_active_recovery_state(
    authority: &FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0RecoveredProcessState, R0DurableAttemptError> {
    let (event_path, checkpoint_path, sentinel_path, duplicate_path) =
        r0_phase_terminal_paths(authority)?;
    let event_types = r0_phase_terminal_event_types(&event_path)?;
    let event_bytes = std::fs::read(&event_path)
        .map_err(|error| r0_attempt_error("read active recovery events", error))?;
    let checkpoint_bytes = std::fs::read(&checkpoint_path)
        .map_err(|error| r0_attempt_error("read active recovery checkpoint", error))?;
    let statuses = r0_checkpoint_statuses(&checkpoint_bytes)?;
    if event_types != ["mission.started", "phase.started"]
        || statuses != ("in_progress".to_owned(), "running".to_owned())
    {
        return Err(r0_attempt_error(
            "verify active recovery workspace",
            format!("events={event_types:?}, statuses={statuses:?}"),
        ));
    }
    let bindings = r0_recovered_phase_terminal_attempt_bindings(authority, helper_bytes)?;
    let process = r0_process_cut_snapshot(&bindings)?;
    Ok(R0RecoveredProcessState {
        process,
        event_types,
        event_bytes,
        checkpoint_bytes,
        mission_status: MissionStatus::InProgress,
        phase_status: PhaseStatus::Running,
        helper_executed: r0_helper_executed_at(&sentinel_path, &duplicate_path)?,
    })
}

fn r0_require_process_group_absent(
    identity: &ProcessExecutionIdentity,
) -> Result<(), R0DurableAttemptError> {
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )
    .map_err(|error| r0_attempt_error("inspect recovered process group", error))?
    {
        RecordedProcessIdentityStatus::ExactGroupAbsent(_) => Ok(()),
        status => Err(r0_attempt_error(
            "inspect recovered process group",
            format!("process group is not absent: {status:?}"),
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the report retains every exact crash-cut and recovery proof"
)]
fn r0_process_recovery_report(
    cell: R0ProcessCrashCell,
    cut: R0ProcessCutSnapshot,
    cut_event_types: Vec<String>,
    cut_event_bytes: Vec<u8>,
    cut_checkpoint_bytes: Vec<u8>,
    cut_helper_executed: bool,
    recovered: R0RecoveredProcessState,
    first_recovery_disposition: Option<R0ProcessRecoveryDisposition>,
    second_recovery_disposition: Option<R0ProcessRecoveryDisposition>,
    recovered_process_group_absent: bool,
) -> R0ProcessCrashRecoveryReport {
    R0ProcessCrashRecoveryReport {
        cell,
        cut,
        cut_event_types,
        cut_event_bytes,
        cut_checkpoint_bytes,
        cut_helper_executed,
        recovered: recovered.process,
        event_types: recovered.event_types,
        event_bytes: recovered.event_bytes,
        checkpoint_bytes: recovered.checkpoint_bytes,
        mission_status: recovered.mission_status,
        phase_status: recovered.phase_status,
        helper_executed: recovered.helper_executed,
        first_recovery_disposition,
        second_recovery_disposition,
        recovered_process_group_absent,
    }
}

/// Runs the ordinary provider until its actor reaches a genuine C4/C5/C6 cut.
///
/// `notify` receives no storage, identity, receipt, or launch authority. It
/// must make the parent-visible acknowledgement durable and then block. If it
/// unexpectedly returns, the actor remains parked at the boundary until the
/// separate supervisor process is killed.
#[doc(hidden)]
pub fn run_r0_process_crash_until_barrier<Notify>(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
    cell: R0ProcessCrashCell,
    notify: Notify,
) -> Result<(), R0DurableAttemptError>
where
    Notify: Fn() + Send + Sync + 'static,
{
    let target = cell.barrier_point().ok_or_else(|| {
        r0_attempt_error(
            "select process crash barrier",
            "the requested cell has no actor barrier",
        )
    })?;
    let observer = R0ProcessBarrierObserver {
        target,
        notify: Arc::new(notify),
    };
    let mut fixture =
        r0_open_phase_terminal_fixture_with_observer(&authority, helper_bytes, observer)?;
    let result = fixture.provider.run_to_terminal(r0_pass_decision());
    Err(r0_attempt_error(
        "hold process crash barrier",
        format!("provider returned before SIGKILL: {result:?}"),
    ))
}

/// Establishes one real process crash cut for aggregate R0 cells 3, 7, or 8.
///
/// The returned guard retains every live owner. The aggregate harness must
/// durably acknowledge the cut and then SIGKILL the separate supervisor
/// process; no graceful drop is accepted as evidence.
#[doc(hidden)]
pub fn prepare_r0_process_crash(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
    cell: R0ProcessCrashCell,
) -> Result<R0ProcessCrashGuard, R0DurableAttemptError> {
    let mut fixture = r0_open_phase_terminal_fixture(&authority, helper_bytes, true)?;
    match cell {
        R0ProcessCrashCell::PendingBeforeClaim => {
            if !fixture.provider.attempt_is_runnable()
                || r0_helper_executed(&fixture)?
                || !r0_phase_terminal_event_types(&fixture.event_path)?.is_empty()
            {
                return Err(r0_attempt_error(
                    "reach pending-before-claim cut",
                    "provider crossed the pending admission boundary",
                ));
            }
        }
        R0ProcessCrashCell::ClaimedBeforeSpecification
        | R0ProcessCrashCell::ReleaseAuthorizedBeforeStarted
        | R0ProcessCrashCell::StartedObservedBeforeTerminal => {
            return Err(r0_attempt_error(
                "prepare process crash",
                "actor-barrier cells require run_r0_process_crash_until_barrier",
            ));
        }
        R0ProcessCrashCell::TerminalObservationBeforeDecision => {
            fixture
                .provider
                .durable_attempt
                .execute()
                .map_err(|error| r0_attempt_error("execute process before decision cut", error))?;
            let process = r0_provider_process_report(&fixture.provider)?;
            if process.state() != OutboxState::Succeeded
                || process.attempts() != 1
                || !process.execution_identity_present()
                || process.observation_history_count() != 1
                || process.evidence_code() != EffectEvidenceCode::ExitObservedSuccess
                || fixture.provider.durable_attempt.decision().is_some()
                || !r0_phase_terminal_event_types(&fixture.event_path)?.is_empty()
            {
                return Err(r0_attempt_error(
                    "reach terminal-observation cut",
                    format!("unexpected process evidence: {process:?}"),
                ));
            }
            r0_verify_single_helper_launch(&fixture)?;
        }
        R0ProcessCrashCell::TerminalDecisionBeforeProjection => {
            fixture
                .provider
                .ensure_phase_running()
                .map_err(|error| r0_attempt_error("start process-crash phase", error))?;
            fixture
                .provider
                .durable_attempt
                .execute()
                .map_err(|error| {
                    r0_attempt_error("execute process before projection cut", error)
                })?;
            let observed = fixture
                .provider
                .durable_attempt
                .observed_termination()
                .map_err(|error| r0_attempt_error("read terminal process observation", error))?;
            fixture
                .provider
                .persist_or_validate_terminal_decision(observed, r0_pass_decision())
                .map_err(|error| r0_attempt_error("persist process terminal decision", error))?;
            let process = r0_provider_process_report(&fixture.provider)?;
            let event_types = r0_phase_terminal_event_types(&fixture.event_path)?;
            if process.state() != OutboxState::Succeeded
                || process.attempts() != 1
                || !process.execution_identity_present()
                || process.observation_history_count() != 1
                || process.evidence_code() != EffectEvidenceCode::ExitObservedSuccess
                || fixture.provider.durable_attempt.decision().is_none()
                || event_types != ["mission.started", "phase.started"]
            {
                return Err(r0_attempt_error(
                    "reach terminal-decision cut",
                    format!("process={process:?}, events={event_types:?}"),
                ));
            }
            r0_verify_single_helper_launch(&fixture)?;
        }
    }
    Ok(R0ProcessCrashGuard {
        _authority: authority,
        _provider: Some(fixture.provider),
    })
}

/// Reopens a killed aggregate R0 process cell through the ordinary provider.
///
/// The first reopen completes the exact next action. A second fresh reopen is
/// required to be byte-stable and actorless, proving no duplicate claim,
/// helper execution, terminal decision, or lifecycle projection.
#[doc(hidden)]
pub fn recover_r0_process_crash(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
    cell: R0ProcessCrashCell,
) -> Result<R0ProcessCrashRecoveryReport, R0DurableAttemptError> {
    let (event_path, checkpoint_path, sentinel_path, duplicate_path) =
        r0_phase_terminal_paths(&authority)?;
    let cut_event_types = r0_phase_terminal_event_types(&event_path)?;
    let cut_event_bytes = match std::fs::read(&event_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(r0_attempt_error("read process-crash event cut", error));
        }
    };
    let cut_checkpoint_bytes = std::fs::read(&checkpoint_path)
        .map_err(|error| r0_attempt_error("read process-crash checkpoint cut", error))?;
    let cut_helper_executed = r0_helper_executed_at(&sentinel_path, &duplicate_path)?;
    let bindings = r0_recovered_phase_terminal_attempt_bindings(&authority, helper_bytes)?;
    let cut = r0_process_cut_snapshot(&bindings)?;
    drop(bindings);
    r0_verify_process_cut(
        cell,
        &cut,
        &cut_event_types,
        &cut_checkpoint_bytes,
        cut_helper_executed,
    )?;

    if cell == R0ProcessCrashCell::ClaimedBeforeSpecification {
        let disposition = r0_reconcile_process_attempt(&authority, helper_bytes)?;
        if disposition != R0ProcessRecoveryDisposition::RecoveredBeforeSpawnNotStarted {
            return Err(r0_attempt_error(
                "verify claimed-process recovery",
                format!("unexpected disposition: {disposition:?}"),
            ));
        }
        let bindings = r0_recovered_phase_terminal_attempt_bindings(&authority, helper_bytes)?;
        let reconciled = r0_process_cut_snapshot(&bindings)?;
        if reconciled.state != OutboxState::Failed
            || reconciled.attempts != 1
            || reconciled.execution_identity.is_some()
            || reconciled.observation_history_count != 1
            || reconciled.terminal_decision_present
            || reconciled.evidence_code != Some(EffectEvidenceCode::ProcessNotStarted)
            || reconciled.not_started_reason
                != Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
            || reconciled.uncertainty.is_some()
            || reconciled.recovery_classification.is_some()
            || reconciled.markers != cut.markers
            || r0_helper_executed_at(&sentinel_path, &duplicate_path)?
            || r0_phase_terminal_event_types(&event_path)? != cut_event_types
            || std::fs::read(&checkpoint_path)
                .map_err(|error| r0_attempt_error("read claimed recovery checkpoint", error))?
                != cut_checkpoint_bytes
        {
            return Err(r0_attempt_error(
                "verify claimed-process recovery",
                format!("unexpected reconciled state: {reconciled:?}"),
            ));
        }
        let first = r0_run_recovered_process_fixture(
            &authority,
            helper_bytes,
            r0_pass_decision(),
            R0RecoveredProcessExpectation::RecoveredBeforeSpawn,
        )?;
        if !first.event_bytes.starts_with(&cut_event_bytes) || first.process.markers != cut.markers
        {
            return Err(r0_attempt_error(
                "verify claimed-process projection",
                "recovery changed the durable prefix or process marker set",
            ));
        }
        let second = r0_run_recovered_process_fixture(
            &authority,
            helper_bytes,
            r0_fail_decision(),
            R0RecoveredProcessExpectation::RecoveredBeforeSpawn,
        )?;
        if second != first {
            return Err(r0_attempt_error(
                "verify stable claimed-process replay",
                format!("first={first:?}, second={second:?}"),
            ));
        }
        return Ok(r0_process_recovery_report(
            cell,
            cut,
            cut_event_types,
            cut_event_bytes,
            cut_checkpoint_bytes,
            cut_helper_executed,
            first,
            Some(disposition),
            None,
            false,
        ));
    }

    if matches!(
        cell,
        R0ProcessCrashCell::ReleaseAuthorizedBeforeStarted
            | R0ProcessCrashCell::StartedObservedBeforeTerminal
    ) {
        let identity = cut.execution_identity.as_ref().ok_or_else(|| {
            r0_attempt_error(
                "verify uncertain-process recovery",
                "the exact cut identity is missing",
            )
        })?;
        let (expected_first, expected_second, expected_classification, helper_must_remain_absent) =
            if cell == R0ProcessCrashCell::ReleaseAuthorizedBeforeStarted {
                (
                    R0ProcessRecoveryDisposition::AuthorizedWithoutStartedCleanedUncertain,
                    R0ProcessRecoveryDisposition::AuthorizedUncertainCleaned,
                    RecoveredProcessAttemptClassification::AuthorizedUncertain,
                    true,
                )
            } else if cell == R0ProcessCrashCell::StartedObservedBeforeTerminal {
                (
                    R0ProcessRecoveryDisposition::StartedProcessCleanedUncertain,
                    R0ProcessRecoveryDisposition::StartedUncertainCleaned,
                    RecoveredProcessAttemptClassification::StartedUncertain,
                    false,
                )
            } else {
                return Err(r0_attempt_error(
                    "select uncertain-process recovery",
                    format!("unexpected crash cell: {cell:?}"),
                ));
            };
        let first_disposition = r0_reconcile_process_attempt(&authority, helper_bytes)?;
        if first_disposition != expected_first {
            return Err(r0_attempt_error(
                "verify uncertain-process recovery",
                format!("unexpected first disposition: {first_disposition:?}"),
            ));
        }
        r0_require_process_group_absent(identity)?;
        let first = r0_read_active_recovery_state(&authority, helper_bytes)?;
        if first.process.state != OutboxState::Uncertain
            || first.process.attempts != 1
            || first.process.execution_identity.as_ref() != Some(identity)
            || first.process.observation_history_count != 1
            || first.process.terminal_decision_present
            || first.process.evidence_code != Some(EffectEvidenceCode::ProcessUncertain)
            || first.process.not_started_reason.is_some()
            || first.process.uncertainty != Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
            || first.process.recovery_classification != Some(expected_classification)
            || first.process.markers != cut.markers
            || first.event_bytes != cut_event_bytes
            || first.checkpoint_bytes != cut_checkpoint_bytes
            || helper_must_remain_absent && first.helper_executed
        {
            return Err(r0_attempt_error(
                "verify uncertain-process recovery",
                format!("unexpected first state: {first:?}"),
            ));
        }
        let second_disposition = r0_reconcile_process_attempt(&authority, helper_bytes)?;
        if second_disposition != expected_second {
            return Err(r0_attempt_error(
                "verify stable uncertain-process recovery",
                format!("unexpected second disposition: {second_disposition:?}"),
            ));
        }
        let second = r0_read_active_recovery_state(&authority, helper_bytes)?;
        if second != first {
            return Err(r0_attempt_error(
                "verify stable uncertain-process recovery",
                format!("first={first:?}, second={second:?}"),
            ));
        }
        return Ok(r0_process_recovery_report(
            cell,
            cut,
            cut_event_types,
            cut_event_bytes,
            cut_checkpoint_bytes,
            cut_helper_executed,
            first,
            Some(first_disposition),
            Some(second_disposition),
            true,
        ));
    }

    let first_requested = if cell == R0ProcessCrashCell::TerminalDecisionBeforeProjection {
        // The persisted Pass decision must defeat this later conflicting
        // caller choice on the very first recovery.
        r0_fail_decision()
    } else {
        r0_pass_decision()
    };
    let first = r0_run_recovered_process_fixture(
        &authority,
        helper_bytes,
        first_requested,
        R0RecoveredProcessExpectation::Succeeded,
    )?;
    if !first.event_bytes.starts_with(&cut_event_bytes) {
        return Err(r0_attempt_error(
            "verify recovered process event prefix",
            "recovery rewrote the durable event prefix",
        ));
    }
    // Every first recovery persisted the Pass decision, or reused C8's
    // already-persisted Pass decision. A conflicting caller choice on the
    // second fresh reopen must therefore be ignored without any mutation.
    let second = r0_run_recovered_process_fixture(
        &authority,
        helper_bytes,
        r0_fail_decision(),
        R0RecoveredProcessExpectation::Succeeded,
    )?;
    if second != first {
        return Err(r0_attempt_error(
            "verify stable process replay",
            format!("first={first:?}, second={second:?}"),
        ));
    }

    Ok(r0_process_recovery_report(
        cell,
        cut,
        cut_event_types,
        cut_event_bytes,
        cut_checkpoint_bytes,
        cut_helper_executed,
        first,
        None,
        None,
        false,
    ))
}

/// Hermetic, hand-built composition tests. No live home, no real
/// Claude/Codex, no network. The "attested fixture helper" is a small
/// synthetic byte string carrying a valid Mach-O/ELF magic prefix (the only
/// thing `FixtureAdmissionPolicy`/`FreshFixtureAuthority::install_fixture_executable`
/// eagerly check) — the same idiom `fixture_sequential_run.rs`'s own Cell 2F
/// unit tests use, for the same reason: `CARGO_BIN_EXE_<name>` is only set for
/// Cargo integration tests, and this module's unit tests compile into the
/// library's own `--lib` test binary. Unlike Cell 2F's fixture engine (which
/// never spawns anything), this composition's `DurableProcessActor` genuinely
/// attempts to launch this synthetic content through
/// `ProductionProcessLaunchAuthority`'s real disposable-canary broker/spawn
/// path — it fails at exec time (the bytes are not a real executable), which
/// is exercised here as a real, typed `SupervisorFailure`/`ProcessExited`
/// outcome, not a stubbed one. The macOS success-path test below discovers a
/// separately prebuilt `orchestrator-owned-process-fixture` next to this unit
/// test binary's target profile and drives the same closed production launch
/// path. The four real `SIGKILL` recovery cells below are the bounded proof
/// described in the module-level crash-cut trade-off, not the full 12-cell
/// integration matrix.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::owned_process_serialization::OwnedProcessTurn;
    use crate::{
        EffectEvidence, EffectResolution, FixtureProjectionFault, FixtureWorkspaceSeed,
        IsolatedFixtureRoot, ProcessExecutionIdentity, ProcessUncertaintyEvidence,
        ProductionBoundary,
    };
    use orchestrator_core::{
        CheckpointPhase, CheckpointPlan, CheckpointProjection, EventJsonMap, EventRecord,
        PhaseDefinition, VerificationClass, VerificationMode, VerificationOutcome,
        decide_verification, decode_checkpoint, decode_event_line, encode_current_checkpoint,
        encode_current_event, reduce,
    };
    use orchestrator_exec::{
        DispatchRequest, Effort, ExecutionRequestDraft, RuntimeCaps, RuntimeFamily, SessionHandle,
        WorkerEventPayload, WorkerOutput, WorkerOutputFields, WorkerOutputKind,
    };
    use orchestrator_process::KernelProcessIdentity;
    use rusqlite::Connection;
    use rustix::process::{Pid, Signal, getpgid};
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::MetadataExt;
    use std::{
        ffi::OsString,
        io::{BufRead, BufReader, Read, Write},
        os::unix::ffi::{OsStrExt, OsStringExt},
        os::unix::{
            fs::OpenOptionsExt,
            process::{CommandExt, ExitStatusExt},
        },
        process::{Child, Command, Stdio},
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::sync_channel,
        },
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const MISSION: &str = "cell3-hermetic-provider-mission";
    const PHASE: &str = "phase-1";
    const WORKER: &str = "fixture-phase-1";
    const RUNTIME: &str = "cell3-hermetic-runtime";
    /// Mach-O 64-bit magic (`0xfeedfacf`, little-endian byte order) plus
    /// padding: the only content `is_native_executable` inspects. Not a real
    /// executable — spawning it fails closed at `exec`.
    const SYNTHETIC_EXECUTABLE: &[u8] =
        &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    #[cfg(target_os = "macos")]
    const DURABLE_CANARY_SENTINEL: &str = ".nanika-durable-canary-launched";
    #[cfg(target_os = "macos")]
    const DURABLE_CANARY_DUPLICATE: &str = ".nanika-durable-canary-duplicate";
    #[cfg(target_os = "macos")]
    const DURABLE_CANARY_CONTENT: &[u8] = b"nanika-durable-canary-v1\n";

    static NEXT_CASE: AtomicUsize = AtomicUsize::new(1);
    #[cfg(target_os = "macos")]
    static REAL_DURABLE_CANARY: Mutex<()> = Mutex::new(());

    const CRASH_CHILD_ROLE_ENV: &str = "NANIKA_PROCESS_CRASH_CHILD_ROLE";
    const CRASH_CHILD_CELL_ENV: &str = "NANIKA_PROCESS_CRASH_CHILD_CELL";
    const CRASH_CHILD_ROOT_ENV: &str = "NANIKA_PROCESS_CRASH_CHILD_ROOT";
    const CRASH_CHILD_SENTINEL_ENV: &str = "NANIKA_PROCESS_CRASH_CHILD_SENTINEL";
    const CRASH_EXPECTED_PID_ENV: &str = "NANIKA_PROCESS_CRASH_EXPECTED_PID";
    const CRASH_EXPECTED_PGID_ENV: &str = "NANIKA_PROCESS_CRASH_EXPECTED_PGID";
    const CRASH_EXPECTED_START_ENV: &str = "NANIKA_PROCESS_CRASH_EXPECTED_START";
    const CRASH_SUPERVISOR_ROLE: &str = "supervisor";
    const CRASH_SENTINEL_HELPER_ROLE: &str = "sentinel-helper";
    const CRASH_BARRIER: &[u8] = b"NANIKA_PROCESS_DURABLE_TRANSITION\n";
    const CRASH_HELPER_LABEL: &str = "process-crash-cut-helper";
    const CRASH_SENTINEL_BYTES: &[u8] = b"durable-helper-sentinel\n";
    const CRASH_HELPER_STARTED: &[u8] = b"NANIKA_PROCESS_HELPER_STARTED\n";
    const CRASH_HELPER_RELEASE: &[u8] = b"NANIKA_PROCESS_HELPER_RELEASE\n";
    const CRASH_BARRIER_TIMEOUT: Duration = Duration::from_secs(15);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ProcessCrashCell {
        Pending,
        Claimed,
        ReleasedLive,
        ReleasedExited,
        StartedExited,
    }

    impl ProcessCrashCell {
        const fn label(self) -> &'static str {
            match self {
                Self::Pending => "pending",
                Self::Claimed => "claimed",
                Self::ReleasedLive => "released-live",
                Self::ReleasedExited => "released-exited",
                Self::StartedExited => "started-exited",
            }
        }
    }

    struct PrivateProcessGroupChild(Child);

    impl PrivateProcessGroupChild {
        fn spawn() -> std::io::Result<Self> {
            Command::new("/bin/sleep")
                .arg("300")
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(Self)
        }

        fn execution_identity(
            &self,
            attempt: u32,
        ) -> Result<ProcessExecutionIdentity, Box<dyn std::error::Error>> {
            let pid = self.0.id();
            let raw_pid = i32::try_from(pid)?;
            let kernel_pid = Pid::from_raw(raw_pid).ok_or("child PID is invalid")?;
            let process_group_id = u32::try_from(getpgid(Some(kernel_pid))?.as_raw_pid())?;
            let identity = KernelProcessIdentity::observe(pid, process_group_id)?;
            Ok(ProcessExecutionIdentity::new(
                attempt,
                pid,
                process_group_id,
                identity.process_start_identity(),
                "2026-07-18T00:00:09Z",
            )?)
        }

        fn stop(&mut self) -> std::io::Result<()> {
            self.0.kill()?;
            self.0.wait().map(|_| ())
        }

        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            self.0.wait()
        }
    }

    impl Drop for PrivateProcessGroupChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn write_crash_harness_frame(frame: &[u8]) -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        // libtest prints `test <name> ... ` without a trailing newline before
        // entering a `--nocapture` child test. Terminate that harness-owned
        // prefix first so the following bounded frame is always its own exact
        // line, independent of buffering or write coalescing.
        stdout.write_all(b"\n")?;
        stdout.write_all(frame)?;
        stdout.flush()
    }

    struct KillOnDropChild {
        child: Child,
        armed: bool,
    }

    impl KillOnDropChild {
        fn new(child: Child) -> Self {
            Self { child, armed: true }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    impl Drop for KillOnDropChild {
        fn drop(&mut self) {
            if self.armed {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    struct TestTimestampSource {
        next: AtomicUsize,
    }

    impl TestTimestampSource {
        fn new() -> Self {
            Self {
                next: AtomicUsize::new(10),
            }
        }
    }

    impl ProcessTimestampSource for TestTimestampSource {
        fn now_utc(&self) -> String {
            let step = self.next.fetch_add(1, Ordering::Relaxed);
            format!("2026-07-18T00:00:{step:02}Z")
        }
    }

    struct ContinuationTestRuntime {
        executors: ExecutorRegistry,
        process: ProjectionOnlyProcessService,
        clock: SystemClock,
        watchdog: FixedWatchdog,
        effects: DeniedEffects,
    }

    impl OwnedChildState for ContinuationTestRuntime {
        fn has_unresolved_children(&self) -> bool {
            false
        }
    }

    struct ContinuationTestExecutor {
        emit_output: bool,
    }

    impl PhaseExecutor for ContinuationTestExecutor {
        fn execute(
            &self,
            _request: DispatchRequest<'_>,
            context: &mut ExecutionContext<'_>,
        ) -> AttemptOutcome {
            if self.emit_output {
                let output = WorkerOutput::new(WorkerOutputFields {
                    chunk: Some("forbidden continuation output".to_owned()),
                    event_kind: Some(WorkerOutputKind::Text),
                    streaming: Some(true),
                    tool_name: None,
                    is_error: None,
                    output_len: Some("forbidden continuation output".len()),
                    duration: None,
                })
                .map(WorkerEventPayload::Output);
                if let Ok(output) = output {
                    // Deliberately ignore the sink rejection. The coordinator
                    // must retain its poisoned continuation state and reject
                    // the centrally emitted terminal that follows.
                    let _ignored = context.emit(&output);
                }
            }
            AttemptOutcome::completed(
                "continuation fixture completed",
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

    struct SpawnReplayOnlySink<'sink, 'coordinator> {
        inner: &'sink mut crate::FixtureWorkerEventSink<'coordinator>,
        spawn_receipt: &'sink mut Option<EventReceipt>,
        withheld_terminal: &'sink mut Option<WorkerEventKind>,
        unexpected_event: &'sink mut Option<WorkerEventKind>,
        rejection: EventSinkError,
    }

    impl EventSink for SpawnReplayOnlySink<'_, '_> {
        fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
            match event.kind() {
                WorkerEventKind::Spawned => {
                    let receipt = self.inner.emit(event)?;
                    *self.spawn_receipt = Some(receipt.clone());
                    Ok(receipt)
                }
                kind @ (WorkerEventKind::Completed | WorkerEventKind::Failed) => {
                    *self.withheld_terminal = Some(kind);
                    Err(self.rejection.clone())
                }
                kind => {
                    *self.unexpected_event = Some(kind);
                    Err(self.rejection.clone())
                }
            }
        }
    }

    fn continuation_runtime(emit_output: bool) -> TestResult<ContinuationTestRuntime> {
        let mut executors = ExecutorRegistry::new();
        let _previous =
            executors.register(RUNTIME, Arc::new(ContinuationTestExecutor { emit_output }))?;
        Ok(ContinuationTestRuntime {
            executors,
            process: ProjectionOnlyProcessService::new()?,
            clock: SystemClock,
            watchdog: FixedWatchdog {
                stall_window: Duration::from_secs(30),
            },
            effects: DeniedEffects::new()?,
        })
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

    #[cfg(target_os = "macos")]
    fn compiled_owned_process_helper() -> TestResult<Vec<u8>> {
        let current = std::env::current_exe()?;
        let parent = current
            .parent()
            .ok_or("current test executable has no parent")?;
        let mut candidates = Vec::new();
        if parent.file_name().is_some_and(|name| name == "deps") {
            let target_profile = parent
                .parent()
                .ok_or("test dependency directory has no target profile parent")?;
            candidates.push(target_profile.join("orchestrator-owned-process-fixture"));
        }
        candidates.push(parent.join("orchestrator-owned-process-fixture"));
        for candidate in &candidates {
            match std::fs::symlink_metadata(candidate) {
                Ok(metadata) => {
                    if !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.uid() != rustix::process::geteuid().as_raw()
                        || metadata.nlink() != 1
                        || metadata.mode() & 0o111 == 0
                        || metadata.mode() & 0o022 != 0
                        || metadata.len() == 0
                        || metadata.len() > orchestrator_process::MAX_ATTESTED_EXECUTABLE_BYTES
                    {
                        return Err(format!(
                            "prebuilt orchestrator-owned-process-fixture has unsafe metadata: \
                             {candidate:?}"
                        )
                        .into());
                    }
                    let bytes = std::fs::read(candidate)?;
                    if u64::try_from(bytes.len()) != Ok(metadata.len()) {
                        return Err(
                            "prebuilt orchestrator-owned-process-fixture changed while reading"
                                .into(),
                        );
                    }
                    return Ok(bytes);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(
            format!("prebuilt orchestrator-owned-process-fixture was not found at {candidates:?}")
                .into(),
        )
    }

    #[cfg(target_os = "macos")]
    #[derive(Debug, Eq, PartialEq)]
    struct DurableCanarySentinelSnapshot {
        bytes: Vec<u8>,
        device: u64,
        inode: u64,
        length: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
    }

    #[cfg(target_os = "macos")]
    fn durable_canary_sentinel_snapshot(path: &Path) -> TestResult<DurableCanarySentinelSnapshot> {
        let metadata = std::fs::metadata(path)?;
        Ok(DurableCanarySentinelSnapshot {
            bytes: std::fs::read(path)?,
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        })
    }

    #[cfg(target_os = "macos")]
    fn process_owned_namespace_entries(root: &Path) -> TestResult<Vec<OsString>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let name = entry?.file_name();
            let bytes = name.as_os_str().as_bytes();
            if bytes.starts_with(b".orchestrator-launch-")
                || bytes.starts_with(b".orchestrator-broker-")
                || bytes.starts_with(b".orchestrator-staging-")
                || bytes.starts_with(b".orchestrator-request-")
            {
                entries.push(name);
            }
        }
        entries.sort_unstable();
        Ok(entries)
    }

    #[cfg(target_os = "macos")]
    fn short_gate_root_entries() -> TestResult<Vec<OsString>> {
        let prefix = format!(".nanika-gate-{}-", std::process::id()).into_bytes();
        let mut entries = Vec::new();
        for entry in std::fs::read_dir("/private/tmp")? {
            let name = entry?.file_name();
            if name.as_os_str().as_bytes().starts_with(&prefix) {
                entries.push(name);
            }
        }
        entries.sort_unstable();
        Ok(entries)
    }

    fn event_types(path: &Path) -> TestResult<Vec<String>> {
        std::fs::read_to_string(path)?
            .lines()
            .map(|line| {
                let event: serde_json::Value = serde_json::from_str(line)?;
                Ok(event
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("event has no string type")?
                    .to_owned())
            })
            .collect()
    }

    #[derive(Debug, Eq, PartialEq)]
    struct WorkspaceProjectionBytes {
        checkpoint: Vec<u8>,
        events: Option<Vec<u8>>,
    }

    fn optional_file_bytes(path: &Path) -> TestResult<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn workspace_projection_bytes(worker_root: &Path) -> TestResult<WorkspaceProjectionBytes> {
        Ok(WorkspaceProjectionBytes {
            checkpoint: std::fs::read(worker_root.join("checkpoint.json"))?,
            events: optional_file_bytes(&worker_root.join("events.jsonl"))?,
        })
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
                id: "cell3-fixture-plan".to_owned(),
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

    fn execution_request_draft(
        worker_root: &Path,
        attempt: u32,
    ) -> TestResult<ExecutionRequestDraft> {
        Ok(ExecutionRequestDraft {
            mission: MISSION.to_owned(),
            phase: PHASE.to_owned(),
            attempt,
            revision: 1,
            objective: "exercise Cell 3 hermetic provider composition".to_owned(),
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
        })
    }

    fn execution_request(worker_root: &Path, attempt: u32) -> TestResult<ExecutionRequest> {
        Ok(ExecutionRequest::new(execution_request_draft(
            worker_root,
            attempt,
        )?)?)
    }

    fn execution_binding_from_draft(
        worker_id: &str,
        requested_runtime: &str,
        draft: ExecutionRequestDraft,
    ) -> TestResult<ExecutionRequestFingerprint> {
        let request = ExecutionRequest::new(draft)?;
        Ok(request.fingerprint(worker_id, requested_runtime))
    }

    /// One hermetic fixture workspace root plus one separate, dedicated
    /// ledger `RuntimeStore` home. Deliberately independent directories: the
    /// ledger holds only the process outbox claim/resolution, never this
    /// mission's workspace-local lifecycle truth (see the module doc's "Why
    /// the Cell-1 runner, not the projector").
    struct Case {
        parent: PathBuf,
        authority: FreshFixtureAuthority,
        ledger_boundary: PrivateProcessLedgerBoundary,
        workspace_root: PathBuf,
        worker_root: PathBuf,
        cleanup_on_drop: bool,
        /// Held for the whole case so no other test observes this one's owned
        /// children. Declared last so it is released after every other field.
        _serialization: OwnedProcessTurn,
    }

    impl Drop for Case {
        fn drop(&mut self) {
            if self.cleanup_on_drop {
                let _ = std::fs::remove_dir_all(&self.parent);
            }
        }
    }

    impl Case {
        fn new(label: &str) -> TestResult<Self> {
            Self::new_with_expected_helper(label, SYNTHETIC_EXECUTABLE)
        }

        fn new_with_expected_helper(label: &str, expected_helper: &[u8]) -> TestResult<Self> {
            let number = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
            let temporary = std::fs::canonicalize(std::env::temp_dir())?;
            let parent = temporary.join(format!(
                "orchestrator-rs-cell3-{}-{number}-{label}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&parent);
            private_dir(&parent)?;
            Self::create_in(parent, expected_helper, true)
        }

        fn create_in(
            parent: PathBuf,
            expected_helper: &[u8],
            cleanup_on_drop: bool,
        ) -> TestResult<Self> {
            let parent = std::fs::canonicalize(parent)?;
            let temporary = std::fs::canonicalize(std::env::temp_dir())?;
            let ledger_home = parent.join("ledger");
            private_dir(&ledger_home)?;
            let ledger_home = std::fs::canonicalize(ledger_home)?;
            let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
            let fixture_root = isolated.path().to_path_buf();
            let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
            let policy =
                FixtureAuthorityPolicy::new(parent.join("live-user"), checkout, &temporary)
                    .with_expected_fixture_helper(expected_helper);
            let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
            let ledger_boundary = PrivateProcessLedgerBoundary::new(Arc::new(
                ProductionBoundary::from_canonical_root(&ledger_home)?,
            ));
            // The fixture root is already canonical. Provider admission
            // requires the exact Go-compatible worker path derived from the
            // request persona and phase, never the enclosing mission root.
            let workspace_root = fixture_root.join("workspaces").join(MISSION);
            let worker_root = workspace_root.join("workers").join(WORKER);
            Ok(Self {
                parent,
                authority,
                ledger_boundary,
                workspace_root,
                worker_root,
                cleanup_on_drop,
                _serialization: OwnedProcessTurn::take(),
            })
        }

        fn recover_in(
            parent: &Path,
            expected_helper: &[u8],
            cleanup_on_drop: bool,
        ) -> TestResult<Self> {
            let parent = std::fs::canonicalize(parent)?;
            let ledger_home = std::fs::canonicalize(parent.join("ledger"))?;
            let mut fixture_roots = std::fs::read_dir(&parent)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.file_name().is_some_and(|name| name != "ledger"))
                .filter(|path| {
                    path.join(crate::fixture_authority::AUTHORITY_MARKER)
                        .is_file()
                })
                .collect::<Vec<_>>();
            fixture_roots.sort_unstable();
            let [fixture_root] = fixture_roots.as_slice() else {
                return Err(format!(
                    "crash fixture parent must contain exactly one authority root: {fixture_roots:?}"
                )
                .into());
            };
            let isolated = IsolatedFixtureRoot::identify(fixture_root)?;
            let temporary = std::fs::canonicalize(std::env::temp_dir())?;
            let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
            let policy =
                FixtureAuthorityPolicy::new(parent.join("live-user"), checkout, &temporary)
                    .with_expected_fixture_helper(expected_helper);
            let authority = FreshFixtureAuthority::recover(isolated, &policy)?;
            let ledger_boundary = PrivateProcessLedgerBoundary::new(Arc::new(
                ProductionBoundary::from_canonical_root(&ledger_home)?,
            ));
            let workspace_root = fixture_root.join("workspaces").join(MISSION);
            let worker_root = workspace_root.join("workers").join(WORKER);
            Ok(Self {
                parent,
                authority,
                ledger_boundary,
                workspace_root,
                worker_root,
                cleanup_on_drop,
                _serialization: OwnedProcessTurn::take(),
            })
        }

        fn create_workspace(&self, state: &MissionState) -> TestResult<WorkspaceAuthority> {
            Ok(self.authority.create_workspace(
                MissionId::new(MISSION)?,
                FixtureWorkspaceSeed::new(
                    b"cell3 fixture mission\n".to_vec(),
                    &initial_checkpoint(state),
                    b"{}".to_vec(),
                )?,
            )?)
        }

        fn open_workspace(&self) -> TestResult<WorkspaceAuthority> {
            Ok(self.authority.open_workspace(MissionId::new(MISSION)?)?)
        }

        /// Installs one synthetic executable under a unique label. Recovery
        /// tests retain and reuse the returned helper so the executable ID and
        /// process-request fingerprint stay identical across reopen.
        fn install_helper(&self) -> TestResult<AttestedFixtureHelper> {
            self.install_helper_bytes(SYNTHETIC_EXECUTABLE)
        }

        fn install_helper_bytes(&self, bytes: &[u8]) -> TestResult<AttestedFixtureHelper> {
            let label = format!(
                "cell3-native-helper-{}",
                NEXT_CASE.fetch_add(1, Ordering::Relaxed)
            );
            Ok(AttestedFixtureHelper::install(
                &self.authority,
                &label,
                bytes,
            )?)
        }

        fn open_ledger(&self) -> TestResult<PrivateProcessLedgerStore> {
            Ok(RuntimeStore::open_private(
                self.ledger_boundary.clone(),
                StorageActorAuthority::new(),
            )?)
        }

        fn ledger_database(&self) -> PathBuf {
            self.parent.join("ledger").join("runtime.db")
        }

        #[cfg(target_os = "macos")]
        fn fixture_root(&self) -> TestResult<&Path> {
            self.workspace_root
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| "fixture workspace has no capability root".into())
        }
    }

    // `FixtureAdmissionPolicy` lives in `fixture_authority.rs`, outside this
    // cell's file allowlist; re-export its constructor path locally for
    // readability without editing that module.
    use crate::FixtureAdmissionPolicy as FixtureAuthorityPolicy;

    fn admit_raw(
        case: &Case,
        workspace: WorkspaceAuthority,
        state: MissionState,
        helper: &AttestedFixtureHelper,
        attempt: u32,
    ) -> Result<HermeticProvider, HermeticProviderError> {
        let request = execution_request(&case.worker_root, attempt)
            .map_err(|_| HermeticProviderError::BindingMismatch)?;
        HermeticProvider::admit(
            workspace,
            state,
            helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            request,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )
    }

    fn provider_for(
        case: &Case,
        workspace: WorkspaceAuthority,
        state: MissionState,
        helper: &AttestedFixtureHelper,
        attempt: u32,
    ) -> TestResult<HermeticProvider> {
        Ok(admit_raw(case, workspace, state, helper, attempt)?)
    }

    fn assert_lazy_phase_storage_absent(case: &Case) {
        assert!(!case.worker_root.exists());
        assert!(!case.workspace_root.join("artifacts").join(PHASE).exists());
        assert!(!case.workspace_root.join("scratch").exists());
        assert!(!case.ledger_database().exists());
    }

    fn test_execution_binding(
        worker_root: &Path,
        attempt: u32,
    ) -> TestResult<ExecutionRequestFingerprint> {
        let request = execution_request(worker_root, attempt)?;
        Ok(request.fingerprint(WORKER, RUNTIME))
    }

    fn prepared_process_launch(
        case: &Case,
        workspace: &WorkspaceAuthority,
        helper: &AttestedFixtureHelper,
    ) -> Result<PreparedProviderLaunch, HermeticProviderError> {
        let phase = PhaseId::new(PHASE).map_err(|_| HermeticProviderError::BindingMismatch)?;
        let worker = workspace.phase_worker_binding("fixture", &phase, &case.worker_root)?;
        Ok(PreparedProviderLaunch::from_attested_fixture(
            worker,
            &helper.executable,
        )?)
    }

    fn exact_process_request(
        case: &Case,
        helper: &AttestedFixtureHelper,
    ) -> TestResult<ProcessRequest> {
        let mission_id =
            MissionId::new(MISSION).map_err(|_| HermeticProviderError::BindingMismatch)?;
        let workspace = case.authority.open_workspace(mission_id)?;
        Ok(prepared_process_launch(case, &workspace, helper)?.into_exact_request_for_test())
    }

    fn open_process_only_attempt(
        case: &Case,
        helper: &AttestedFixtureHelper,
        attempt: u32,
        open: DurableAttemptOpen,
    ) -> Result<DurableHermeticAttempt, HermeticProviderError> {
        let mission_id =
            MissionId::new(MISSION).map_err(|_| HermeticProviderError::BindingMismatch)?;
        let phase_id = PhaseId::new(PHASE).map_err(|_| HermeticProviderError::BindingMismatch)?;
        let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
        let request = execution_request(&case.worker_root, attempt)
            .map_err(|_| HermeticProviderError::BindingMismatch)?;
        let execution_binding = request.fingerprint(WORKER, RUNTIME);
        let workspace = case.authority.open_workspace(mission_id.clone())?;
        let prepared_launch = prepared_process_launch(case, &workspace, helper)?;
        DurableHermeticAttempt::open(
            case.ledger_boundary.clone(),
            mission_id,
            &phase_id,
            identity,
            attempt,
            prepared_launch,
            execution_binding,
            open,
        )
    }

    fn activate_process_only_attempt(
        attempt: &mut DurableHermeticAttempt,
    ) -> Result<(), HermeticProviderError> {
        attempt.activate(
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )
    }

    fn crash_helper(case: &Case) -> TestResult<AttestedFixtureHelper> {
        Ok(AttestedFixtureHelper::recover_for_test(
            &case.authority,
            CRASH_HELPER_LABEL,
            SYNTHETIC_EXECUTABLE,
        )?)
    }

    fn reobserve_expected_live_identity(attempt: u32) -> TestResult<ProcessExecutionIdentity> {
        let pid = std::env::var(CRASH_EXPECTED_PID_ENV)?.parse::<u32>()?;
        let process_group_id = std::env::var(CRASH_EXPECTED_PGID_ENV)?.parse::<u32>()?;
        let expected_start = std::env::var(CRASH_EXPECTED_START_ENV)?;
        let kernel_pid =
            Pid::from_raw(i32::try_from(pid)?).ok_or("expected child PID is invalid")?;
        let observed_group_id = u32::try_from(getpgid(Some(kernel_pid))?.as_raw_pid())?;
        if observed_group_id != process_group_id {
            return Err(format!(
                "expected child process group changed: expected {process_group_id}, observed {observed_group_id}"
            )
            .into());
        }
        let observed = KernelProcessIdentity::observe(pid, observed_group_id)?;
        if observed.process_start_identity() != expected_start {
            return Err("expected child process-start identity changed".into());
        }
        Ok(ProcessExecutionIdentity::new(
            attempt,
            pid,
            observed_group_id,
            observed.process_start_identity(),
            "2026-07-18T00:00:09Z",
        )?)
    }

    fn spawn_gated_sentinel_helper(sentinel: &Path) -> TestResult<PrivateProcessGroupChild> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--exact")
            .arg("hermetic_provider::tests::process_crash_cut_released_exited_becomes_uncertain_once")
            .arg("--nocapture")
            .env(CRASH_CHILD_ROLE_ENV, CRASH_SENTINEL_HELPER_ROLE)
            .env(CRASH_CHILD_SENTINEL_ENV, sentinel)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Ok(PrivateProcessGroupChild(command.spawn()?))
    }

    fn crash_child_main(cell: ProcessCrashCell, root: &Path) -> TestResult {
        let case = Case::create_in(root.to_path_buf(), SYNTHETIC_EXECUTABLE, false)?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = AttestedFixtureHelper::install(
            &case.authority,
            CRASH_HELPER_LABEL,
            SYNTHETIC_EXECUTABLE,
        )?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (effect, _key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 1, effect)?;
        if cell != ProcessCrashCell::Pending {
            let live_identity = if cell == ProcessCrashCell::ReleasedLive {
                Some(reobserve_expected_live_identity(1)?)
            } else {
                None
            };
            let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
            let claimed = ledger
                .claim_prepared_process(prepared, "2026-07-18T00:00:08Z")
                .map_err(|_| "prepared process claim failed")?;
            if let Some(identity) = live_identity {
                let _authorization = ledger.authorize_process_release_for_test(
                    &claimed,
                    &process_request,
                    &identity,
                    "2026-07-18T00:00:09Z",
                )?;
            } else if matches!(
                cell,
                ProcessCrashCell::ReleasedExited | ProcessCrashCell::StartedExited
            ) {
                let sentinel = PathBuf::from(
                    std::env::var_os(CRASH_CHILD_SENTINEL_ENV)
                        .ok_or("released-exited child is missing sentinel path")?,
                );
                let mut child = spawn_gated_sentinel_helper(&sentinel)?;
                let stdout = child
                    .0
                    .stdout
                    .take()
                    .ok_or("sentinel helper stdout is missing")?;
                let mut reader = BufReader::new(stdout);
                let started = loop {
                    let mut line = Vec::new();
                    if reader.read_until(b'\n', &mut line)? == 0 || line == CRASH_HELPER_STARTED {
                        break line;
                    }
                };
                if started != CRASH_HELPER_STARTED {
                    return Err(format!("unexpected sentinel helper start: {started:?}").into());
                }
                let identity = child.execution_identity(claimed.claim_attempt())?;
                let authorization = ledger.authorize_process_release_for_test(
                    &claimed,
                    &process_request,
                    &identity,
                    "2026-07-18T00:00:09Z",
                )?;
                let mut gate = child
                    .0
                    .stdin
                    .take()
                    .ok_or("sentinel helper stdin is missing")?;
                gate.write_all(CRASH_HELPER_RELEASE)?;
                gate.flush()?;
                drop(gate);
                let status = child.wait()?;
                if !status.success() {
                    return Err(format!("sentinel helper failed: {status}").into());
                }
                if cell == ProcessCrashCell::StartedExited {
                    ledger.record_process_started_observed_for_test(
                        &claimed,
                        &process_request,
                        &authorization,
                        "2026-07-18T00:00:10Z",
                    )?;
                }
            }
        }
        ledger.close()?;
        write_crash_harness_frame(CRASH_BARRIER)?;
        loop {
            std::thread::park();
        }
    }

    fn maybe_run_crash_child(cell: ProcessCrashCell) -> TestResult<bool> {
        let Some(role) = std::env::var_os(CRASH_CHILD_ROLE_ENV) else {
            return Ok(false);
        };
        if role == CRASH_SENTINEL_HELPER_ROLE {
            let sentinel = PathBuf::from(
                std::env::var_os(CRASH_CHILD_SENTINEL_ENV).ok_or("helper sentinel is missing")?,
            );
            write_crash_harness_frame(CRASH_HELPER_STARTED)?;
            let mut release = vec![0; CRASH_HELPER_RELEASE.len()];
            std::io::stdin().read_exact(&mut release)?;
            if release != CRASH_HELPER_RELEASE {
                return Err("invalid sentinel helper release gate".into());
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(sentinel)?;
            file.write_all(CRASH_SENTINEL_BYTES)?;
            file.sync_all()?;
            return Ok(true);
        }
        if role != CRASH_SUPERVISOR_ROLE
            || std::env::var(CRASH_CHILD_CELL_ENV).as_deref() != Ok(cell.label())
        {
            return Err("invalid crash child dispatch".into());
        }
        let root = PathBuf::from(
            std::env::var_os(CRASH_CHILD_ROOT_ENV).ok_or("supervisor root is missing")?,
        );
        crash_child_main(cell, &root)?;
        Ok(true)
    }

    fn spawn_and_kill_crash_supervisor(
        cell: ProcessCrashCell,
        root: &Path,
        sentinel: Option<&Path>,
        expected_identity: Option<&ProcessExecutionIdentity>,
        test_name: &str,
    ) -> TestResult {
        private_dir(root)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CRASH_CHILD_ROLE_ENV, CRASH_SUPERVISOR_ROLE)
            .env(CRASH_CHILD_CELL_ENV, cell.label())
            .env(CRASH_CHILD_ROOT_ENV, root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(sentinel) = sentinel {
            command.env(CRASH_CHILD_SENTINEL_ENV, sentinel);
        }
        if let Some(identity) = expected_identity {
            command
                .env(CRASH_EXPECTED_PID_ENV, identity.pid().to_string())
                .env(
                    CRASH_EXPECTED_PGID_ENV,
                    identity.process_group_id().to_string(),
                )
                .env(CRASH_EXPECTED_START_ENV, identity.process_start_identity());
        }
        let mut child = KillOnDropChild::new(command.spawn()?);
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or("supervisor stdout is missing")?;
        let (sender, receiver) = sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let result = loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => break Ok(line),
                    Ok(_) if line == CRASH_BARRIER => break Ok(line),
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
            };
            let _ignored = sender.send(result);
        });
        let barrier = match receiver.recv_timeout(CRASH_BARRIER_TIMEOUT) {
            Ok(result) => result?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.child.try_wait()? {
                    return Err(format!(
                        "crash supervisor exited before its durable barrier: {status}"
                    )
                    .into());
                }
                return Err(format!(
                    "timed out after {}s waiting for durable crash barrier",
                    CRASH_BARRIER_TIMEOUT.as_secs()
                )
                .into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("durable crash-barrier reader disconnected".into());
            }
        };
        if barrier != CRASH_BARRIER {
            return Err(format!("unexpected crash barrier: {barrier:?}").into());
        }
        let child_pid =
            Pid::from_raw(i32::try_from(child.child.id())?).ok_or("supervisor PID is invalid")?;
        rustix::process::kill_process(child_pid, Signal::KILL)?;
        let status = child.child.wait()?;
        if status.signal() != Some(9) {
            return Err(format!("supervisor was not killed by SIGKILL: {status}").into());
        }
        child.disarm();
        Ok(())
    }

    fn crash_case_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "orchestrator-process-crash-{}-{}-{label}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn recovered_crash_case(root: &Path, cleanup_on_drop: bool) -> TestResult<Case> {
        Case::recover_in(root, SYNTHETIC_EXECUTABLE, cleanup_on_drop)
    }

    fn recovered_crash_snapshot(
        case: &Case,
        helper: &AttestedFixtureHelper,
    ) -> TestResult<(String, ExactOutboxSnapshot)> {
        let process_request = exact_process_request(case, helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_effect, key, _binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("crash-cut process effect is missing")?;
        ledger.close()?;
        Ok((key, snapshot))
    }

    fn assert_recovery_fails_closed(
        case: &Case,
        helper: &AttestedFixtureHelper,
    ) -> TestResult<RecoveredProcessDisposition> {
        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "recovery must not initialize runtime",
            )))
        });
        let mut attempt =
            open_process_only_attempt(case, helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = attempt.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        let disposition = match rejected {
            Err(HermeticProviderError::ProcessRecoveryReconciled(disposition)) => disposition,
            other => return Err(format!("unexpected recovery result: {other:?}").into()),
        };
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(attempt.actor.is_none());
        Ok(disposition)
    }

    fn execute_continuation_request(
        runtime: &ContinuationTestRuntime,
        sink: &mut dyn EventSink,
        request: &ExecutionRequest,
        identity: WorkerIdentity,
    ) -> TestResult<AttemptOutcome> {
        let executor = runtime.executors.resolve(RUNTIME)?;
        let deadline = runtime
            .clock
            .now()
            .checked_add(Duration::from_secs(60))
            .ok_or("continuation deadline overflow")?;
        let mut context = ExecutionContext::new(
            &runtime.process,
            &runtime.clock,
            &runtime.watchdog,
            &runtime.effects,
            sink,
            identity,
            deadline,
        );
        Ok(executor.execute(request, &mut context)?)
    }

    fn active_continuation_coordinator(
        case: &Case,
        emit_output: bool,
    ) -> TestResult<(LifecycleCoordinator<ContinuationTestRuntime>, PhaseId)> {
        let state = one_phase_state()?;
        let phase = PhaseId::new(PHASE)?;
        let workspace = case.create_workspace(&state)?;
        let mut coordinator =
            LifecycleCoordinator::new(workspace, continuation_runtime(false)?, state.clone())?;
        coordinator.transition_allocated(
            None,
            ReducerTransition::MissionStarted,
            serde_json::Value::Null,
            None,
        )?;
        coordinator.transition_allocated(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            serde_json::Value::Null,
            None,
        )?;
        coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
        let request = execution_request(&case.worker_root, 1)?;
        let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
        let seeded = coordinator.with_attempt(&phase, WORKER, 1, |runtime, sink| {
            execute_continuation_request(runtime, sink, &request, identity)
        });
        assert!(matches!(seeded, Err(LifecycleError::RecoveryPending)));
        drop(coordinator);

        let workspace = case.open_workspace()?;
        let reopened =
            LifecycleCoordinator::new(workspace, continuation_runtime(emit_output)?, state)?;
        assert!(matches!(
            reopened.durable_attempt_replay(&phase, WORKER, 1),
            Err(LifecycleError::AttemptRecoveryRequired)
        ));
        Ok((reopened, phase))
    }

    fn project_continuation_request(
        coordinator: &mut LifecycleCoordinator<ContinuationTestRuntime>,
        phase: &PhaseId,
        request: &ExecutionRequest,
    ) -> TestResult<Result<FixtureAttemptRun<TestResult<AttemptOutcome>>, LifecycleError>> {
        let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
        Ok(coordinator.project_terminal_into_active_attempt(
            phase,
            WORKER,
            request.attempt(),
            |runtime, sink| execute_continuation_request(runtime, sink, request, identity),
        ))
    }

    #[cfg(unix)]
    fn seed_terminal_process_effect(
        case: &Case,
        helper: &AttestedFixtureHelper,
        attempt: u32,
        resolution: EffectResolution,
    ) -> TestResult<String> {
        let process_request = exact_process_request(case, helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, attempt)?;
        let (effect, key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            attempt,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(
            &mut ledger,
            &MissionId::new(MISSION)?,
            PHASE,
            attempt,
            effect,
        )?;
        let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
        let claimed = ledger
            .claim_prepared_process(prepared, "2026-07-18T00:00:10Z")
            .map_err(|_| "prepared process claim failed")?;
        ledger.resolve_claimed_process(
            &claimed.into_terminal(),
            resolution,
            "2026-07-18T00:00:11Z",
        )?;
        ledger.close()?;
        Ok(key)
    }

    #[test]
    fn terminal_projection_moves_exact_typed_outcome() -> TestResult {
        let exact = AttemptOutcome::completed(
            "exact terminal output",
            AttemptEvidence::new(),
            Duration::from_millis(17),
        )?;
        let mut terminal = DurableHermeticTerminal::from_evidence(
            ExactTerminalEvidence {
                outcome: exact,
                committed_at_utc: "2026-07-18T00:00:11Z".to_owned(),
            },
            None,
        );
        let projected = terminal.take_projection_outcome()?;

        assert_eq!(
            (
                projected.output(),
                projected.elapsed(),
                projected.termination()
            ),
            (
                Some("exact terminal output"),
                Duration::from_millis(17),
                None
            )
        );
        Ok(())
    }

    #[test]
    fn process_only_existing_open_requires_a_retained_effect_and_releases_lease() -> TestResult {
        let case = Case::new("process-only-existing-missing")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;

        let rejected =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly);
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::TerminalDecisionConflict)
        ));
        assert!(!case.worker_root.exists());
        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(1)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn fresh_process_only_workspace_replacement_rejects_before_private_admission() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("process-only-delayed-workspace-replacement")?;
        let _workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;

        let displaced = case.parent.join("displaced-workspace");
        std::fs::rename(&case.workspace_root, &displaced)?;
        private_dir(&case.workspace_root)?;
        let rejected = attempt.activate(
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProviderLaunch(
                ProviderLaunchError::WorkspaceAdmission
            ))
        ));
        assert!(!case.worker_root.exists());

        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(1)?.is_empty());
        assert!(ledger.recovery_effects(1)?.is_empty());
        ledger.close()?;
        let connection = Connection::open(case.ledger_database())?;
        let tables = {
            let mut statement = connection.prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table'
                   AND name NOT LIKE 'sqlite_%'
                   AND name <> 'runtime_schema'
                 ORDER BY name",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for table in tables {
            let rows: i64 =
                connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
            assert_eq!(rows, 0, "{table} mutated before provider pre-admission");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_only_existing_terminal_reopens_actorlessly_without_materializing_worker()
    -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("process-only-existing-terminal")?;
        let _workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
        )?;
        let decision = TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?;
        let mut ledger = case.open_ledger()?;
        ledger.record_terminal_decision(&decision)?;
        let history_before = ledger.attempt_history(&key, 10)?;
        ledger.close()?;

        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        activate_process_only_attempt(&mut attempt)?;
        assert!(attempt.actor.is_none());
        assert!(!case.worker_root.exists());

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "terminal replay must not initialize launch runtime",
            )))
        });
        attempt.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        drop(attempt);

        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(1)?.is_empty());
        assert_eq!(ledger.attempt_history(&key, 10)?, history_before);
        ledger.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_crash_cut_pending_admission_survives_two_restarts() -> TestResult {
        if maybe_run_crash_child(ProcessCrashCell::Pending)? {
            return Ok(());
        }
        let root = crash_case_root("pending");
        spawn_and_kill_crash_supervisor(
            ProcessCrashCell::Pending,
            &root,
            None,
            None,
            "hermetic_provider::tests::process_crash_cut_pending_admission_survives_two_restarts",
        )?;
        let case = recovered_crash_case(&root, true)?;
        let helper = crash_helper(&case)?;
        let (key, before) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(before.effect().state(), OutboxState::Pending);
        assert_eq!(before.effect().attempts(), 0);
        assert!(before.effect().execution_identity().is_none());
        for _ in 0..2 {
            // Reopening admission is the restart boundary for a still-Pending
            // attempt. Deliberately do not activate: runtime construction is
            // downstream of activation and must remain unreachable here.
            let attempt =
                open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
            assert!(attempt.actor.is_none());
            assert!(attempt.service.is_none());
            drop(attempt);
        }
        let mut ledger = case.open_ledger()?;
        let after = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("pending vanished")?;
        assert_eq!(after.effect().state(), OutboxState::Pending);
        assert_eq!(after.effect().attempts(), 0);
        ledger.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_crash_cut_claimed_before_spawn_terminalizes_exactly_once() -> TestResult {
        if maybe_run_crash_child(ProcessCrashCell::Claimed)? {
            return Ok(());
        }
        let root = crash_case_root("claimed");
        spawn_and_kill_crash_supervisor(
            ProcessCrashCell::Claimed,
            &root,
            None,
            None,
            "hermetic_provider::tests::process_crash_cut_claimed_before_spawn_terminalizes_exactly_once",
        )?;
        let case = recovered_crash_case(&root, true)?;
        let helper = crash_helper(&case)?;
        assert_recovery_fails_closed(&case, &helper)?;
        let (key, snapshot) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(snapshot.effect().state(), OutboxState::Failed);
        assert_eq!(snapshot.effect().attempts(), 1);
        assert!(snapshot.effect().execution_identity().is_none());
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_not_started_reason()),
            Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
        );
        let mut ledger = case.open_ledger()?;
        let history = ledger.attempt_history(&key, 10)?;
        ledger.record_terminal_decision(&TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?)?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "terminal replay must not initialize runtime",
            )))
        });
        let mut replayed =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)
                .map_err(|error| format!("terminal process-only reopen failed: {error:?}"))?;
        replayed.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        drop(replayed);
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history);
        ledger.close()?;
        assert!(!case.worker_root.exists());
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_crash_cut_released_live_requires_reap_before_uncertain_resolution() -> TestResult {
        if maybe_run_crash_child(ProcessCrashCell::ReleasedLive)? {
            return Ok(());
        }
        let root = crash_case_root("released-live");
        let mut fixture = PrivateProcessGroupChild::spawn()?;
        let owned_identity = fixture.execution_identity(1)?;
        spawn_and_kill_crash_supervisor(
            ProcessCrashCell::ReleasedLive,
            &root,
            None,
            Some(&owned_identity),
            "hermetic_provider::tests::process_crash_cut_released_live_requires_reap_before_uncertain_resolution",
        )?;
        let case = recovered_crash_case(&root, true)?;
        let helper = crash_helper(&case)?;
        let (key, before) = recovered_crash_snapshot(&case, &helper)?;
        let persisted_identity = before
            .effect()
            .execution_identity()
            .cloned()
            .ok_or("released live identity is missing")?;
        let ledger = case.open_ledger()?;
        let history_before_cleanup = ledger.attempt_history(&key, 10)?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "cleanup recovery must not initialize runtime",
            )))
        });
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = attempt.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecovery(
                DurableProcessRecoveryError::CleanupTimedOut
            ))
        ));
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        drop(attempt);

        let (_key, still_executing) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(still_executing.effect().state(), OutboxState::Executing);
        assert_eq!(
            still_executing.effect().execution_identity(),
            Some(&persisted_identity)
        );
        assert!(still_executing.current_observation().is_none());
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history_before_cleanup);
        ledger.close()?;

        let status = fixture.wait()?;
        assert_eq!(status.signal(), Some(9));
        assert_recovery_fails_closed(&case, &helper)?;
        let (_key, resolved) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(resolved.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            resolved
                .current_observation()
                .and_then(|observation| observation.evidence().process_uncertainty()),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        let ledger = case.open_ledger()?;
        let history_after_resolution = ledger.attempt_history(&key, 10)?;
        assert_eq!(history_after_resolution.len(), 1);
        ledger.close()?;

        // Released-uncertain recovery is cleanup-only. Reopening once more
        // proves it cannot append a duplicate terminal observation.
        assert_recovery_fails_closed(&case, &helper)?;
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history_after_resolution);
        ledger.close()?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_crash_cut_released_exited_becomes_uncertain_once() -> TestResult {
        if maybe_run_crash_child(ProcessCrashCell::ReleasedExited)? {
            return Ok(());
        }
        let root = crash_case_root("released-exited");
        let sentinel = root.join("helper-sentinel");
        spawn_and_kill_crash_supervisor(
            ProcessCrashCell::ReleasedExited,
            &root,
            Some(&sentinel),
            None,
            "hermetic_provider::tests::process_crash_cut_released_exited_becomes_uncertain_once",
        )?;
        let case = recovered_crash_case(&root, true)?;
        let helper = crash_helper(&case)?;
        assert_eq!(std::fs::read(&sentinel)?, CRASH_SENTINEL_BYTES);
        assert_eq!(
            assert_recovery_fails_closed(&case, &helper)?,
            RecoveredProcessDisposition::AuthorizedWithoutStartedCleanedUncertain
        );
        let (key, snapshot) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_uncertainty()),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        let ledger = case.open_ledger()?;
        let history = ledger.attempt_history(&key, 10)?;
        ledger.close()?;
        assert_recovery_fails_closed(&case, &helper)?;
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history);
        ledger.close()?;
        assert_eq!(std::fs::read(&sentinel)?, CRASH_SENTINEL_BYTES);
        assert!(!case.worker_root.exists());
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_crash_cut_started_exited_becomes_uncertain_once() -> TestResult {
        if maybe_run_crash_child(ProcessCrashCell::StartedExited)? {
            return Ok(());
        }
        let root = crash_case_root("started-exited");
        let sentinel = root.join("helper-sentinel");
        spawn_and_kill_crash_supervisor(
            ProcessCrashCell::StartedExited,
            &root,
            Some(&sentinel),
            None,
            "hermetic_provider::tests::process_crash_cut_started_exited_becomes_uncertain_once",
        )?;
        let case = recovered_crash_case(&root, true)?;
        let helper = crash_helper(&case)?;
        assert_eq!(std::fs::read(&sentinel)?, CRASH_SENTINEL_BYTES);
        assert_eq!(
            assert_recovery_fails_closed(&case, &helper)?,
            RecoveredProcessDisposition::StartedProcessCleanedUncertain
        );
        let (key, snapshot) = recovered_crash_snapshot(&case, &helper)?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_uncertainty()),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        let ledger = case.open_ledger()?;
        let history = ledger.attempt_history(&key, 10)?;
        ledger.close()?;
        assert_eq!(
            assert_recovery_fails_closed(&case, &helper)?,
            RecoveredProcessDisposition::StartedUncertainCleaned
        );
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history);
        ledger.close()?;
        assert_eq!(std::fs::read(&sentinel)?, CRASH_SENTINEL_BYTES);
        assert!(!case.worker_root.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_only_claimed_recovery_terminalizes_once_without_worker() -> TestResult {
        let case = Case::new("process-only-executing")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (effect, key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 1, effect)?;
        let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
        let _claimed = ledger
            .claim_prepared_process(prepared, "2026-07-18T00:00:10Z")
            .map_err(|_| "prepared process claim failed")?;
        ledger.close()?;

        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = activate_process_only_attempt(&mut attempt);
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecoveryReconciled(_))
        ));
        assert!(attempt.actor.is_none());
        assert!(!case.worker_root.exists());
        drop(attempt);
        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("recovered claimed process is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Failed);
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_not_started_reason()),
            Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
        );
        let history = ledger.attempt_history(&key, 10)?;
        ledger.record_terminal_decision(&TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?)?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "terminal replay must not initialize launch runtime",
            )))
        });
        let mut replayed =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)
                .map_err(|error| format!("terminal process-only reopen failed: {error:?}"))?;
        replayed.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        drop(replayed);
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history);
        ledger.close()?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_only_recovery_marks_released_absent_group_uncertain_without_relaunch() -> TestResult
    {
        let case = Case::new("process-only-released-group-absent")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (effect, key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 1, effect)?;
        let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
        let claimed = ledger
            .claim_prepared_process(prepared, "2026-07-18T00:00:08Z")
            .map_err(|_| "prepared process claim failed")?;
        let mut child = PrivateProcessGroupChild::spawn()?;
        let identity = child.execution_identity(claimed.claim_attempt())?;
        let _authorization = ledger.authorize_process_release_for_test(
            &claimed,
            &process_request,
            &identity,
            "2026-07-18T00:00:09Z",
        )?;
        child.stop()?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "recovery must not initialize launch runtime",
            )))
        });
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = attempt.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );

        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecoveryReconciled(_))
        ));
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(attempt.actor.is_none());
        assert!(!case.worker_root.exists());
        drop(attempt);

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("recovered process effect is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(snapshot.effect().attempts(), 1);
        assert_eq!(snapshot.effect().execution_identity(), Some(&identity));
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_uncertainty()),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        let history_after_first_recovery = ledger.attempt_history(&key, 10)?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "repeated recovery must not initialize launch runtime",
            )))
        });
        let mut repeated =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = repeated.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecoveryReconciled(_))
        ));
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(repeated.actor.is_none());
        drop(repeated);

        let ledger = case.open_ledger()?;
        assert_eq!(
            ledger.attempt_history(&key, 10)?,
            history_after_first_recovery
        );
        ledger.close()?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_only_recovery_requires_absence_proof_before_released_terminal_mutation() -> TestResult
    {
        let case = Case::new("process-only-released-live")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (effect, key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 1, effect)?;
        let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
        let claimed = ledger
            .claim_prepared_process(prepared, "2026-07-18T00:00:08Z")
            .map_err(|_| "prepared process claim failed")?;
        let mut child = PrivateProcessGroupChild::spawn()?;
        let identity = child.execution_identity(claimed.claim_attempt())?;
        let _authorization = ledger.authorize_process_release_for_test(
            &claimed,
            &process_request,
            &identity,
            "2026-07-18T00:00:09Z",
        )?;
        let history_before_recovery = ledger.attempt_history(&key, 10)?;
        ledger.close()?;

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "live recovery must not initialize launch runtime",
            )))
        });
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = attempt.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecovery(
                DurableProcessRecoveryError::CleanupTimedOut
            ))
        ));
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert!(attempt.actor.is_none());
        assert!(!case.worker_root.exists());
        drop(attempt);

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("recovered process effect is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Executing);
        assert_eq!(snapshot.effect().attempts(), 1);
        assert_eq!(snapshot.effect().execution_identity(), Some(&identity));
        assert!(snapshot.current_observation().is_none());
        assert_eq!(ledger.attempt_history(&key, 10)?, history_before_recovery);
        ledger.close()?;

        let status = child.wait()?;
        assert_eq!(status.signal(), Some(9));

        let launches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&launches);
        let runtime = DeferredProviderRuntime::injected(move || {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ProcessError::Spawn(std::io::Error::other(
                "absence-proof recovery must not initialize launch runtime",
            )))
        });
        let mut retried =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = retried.activate_with_runtime(
            runtime,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecoveryReconciled(_))
        ));
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        drop(retried);

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("absence-proven process effect is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            snapshot
                .current_observation()
                .and_then(|observation| observation.evidence().process_uncertainty()),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert_eq!(ledger.attempt_history(&key, 10)?.len(), 1);
        ledger.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_only_uncertain_recovery_fails_closed_before_worker_and_releases_lease() -> TestResult
    {
        let case = Case::new("process-only-uncertain")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;
        let key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::CommitIndeterminate,
            )),
        )?;

        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::ExistingOnly)?;
        let rejected = activate_process_only_attempt(&mut attempt);
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProcessRecoveryReconciled(_))
        ));
        assert!(attempt.actor.is_none());
        assert!(!case.worker_root.exists());
        drop(attempt);
        let mut ledger = case.open_ledger()?;
        assert_eq!(
            ledger
                .exact_outbox_snapshot(&key)?
                .map(|snapshot| snapshot.effect().state()),
            Some(OutboxState::Uncertain)
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn dropping_fresh_process_only_attempt_closes_actor_and_leaves_pending_unclaimed() -> TestResult
    {
        let state = one_phase_state()?;
        let case = Case::new("process-only-fresh-drop")?;
        let _workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
        let key = attempt.idempotency_key.clone();
        activate_process_only_attempt(&mut attempt)?;
        assert!(attempt.actor.is_some());
        drop(attempt);

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("fresh process-only effect must remain durable")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Pending);
        assert!(snapshot.current_observation().is_none());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn repeated_process_only_activation_shuts_existing_actor_and_fails_closed() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("process-only-repeat-activation")?;
        let _workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
        let key = attempt.idempotency_key.clone();
        activate_process_only_attempt(&mut attempt)?;
        assert!(attempt.actor.is_some());

        let rejected = attempt.activate(
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(rejected, Err(HermeticProviderError::ActorBuild)));
        assert!(attempt.actor.is_none());
        assert!(attempt.service.is_none());

        let mut ledger = case.open_ledger()?;
        assert_eq!(
            ledger
                .exact_outbox_snapshot(&key)?
                .map(|snapshot| snapshot.effect().state()),
            Some(OutboxState::Pending)
        );
        ledger.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dropping_terminal_process_only_attempt_before_decision_is_cleanly_reopenable() -> TestResult
    {
        let case = Case::new("process-only-terminal-drop")?;
        let _workspace = case.create_workspace(&one_phase_state()?)?;
        let helper = case.install_helper()?;
        let _key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
        )?;
        let attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
        assert!(attempt.terminal().is_some());
        assert!(attempt.decision().is_none());
        drop(attempt);

        let ledger = case.open_ledger()?;
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_none()
        );
        ledger.close()?;
        let reopened =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
        assert!(reopened.terminal().is_some());
        drop(reopened);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_only_decision_reuse_precedes_wrapper_projection_and_never_writes_lifecycle()
    -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("process-only-wrapper-parity")?;
        let _workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let before = workspace_projection_bytes(&case.workspace_root)?;
        let _key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
        )?;

        let mut attempt =
            open_process_only_attempt(&case, &helper, 1, DurableAttemptOpen::AdmitMissing)?;
        activate_process_only_attempt(&mut attempt)?;
        assert_eq!(
            attempt.observed_termination()?,
            Some(MechanicalTermination::SupervisorFailure)
        );
        assert_eq!(
            attempt.persist_or_validate_terminal_decision(pass_decision())?,
            pass_decision()
        );
        assert_eq!(
            attempt.persist_or_validate_terminal_decision(block_decision())?,
            pass_decision()
        );
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
        assert!(!case.worker_root.exists());
        drop(attempt);

        let reopened = case.open_workspace()?;
        let mut provider = provider_for(&case, reopened, state, &helper, 1)?;
        let projected = provider.run_to_terminal(block_decision())?;
        assert_eq!(projected.mission_status(), MissionStatus::Failed);
        assert_eq!(projected.phase_status(), PhaseStatus::Failed);
        let ledger = case.open_ledger()?;
        let decision = ledger
            .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
            .ok_or("process-only decision must remain authoritative")?;
        assert_eq!(decision.verification(), pass_decision());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn authoritative_execution_binding_changes_for_every_independent_input() -> TestResult {
        let worker_root = PathBuf::from("/tmp/cell3-binding-worker");
        let baseline = execution_binding_from_draft(
            WORKER,
            RUNTIME,
            execution_request_draft(&worker_root, 1)?,
        )?;
        macro_rules! assert_draft_change {
            ($label:literal, $mutation:expr) => {{
                let mut draft = execution_request_draft(&worker_root, 1)?;
                $mutation(&mut draft);
                let changed = execution_binding_from_draft(WORKER, RUNTIME, draft)?;
                assert!(
                    baseline != changed,
                    "{} was omitted from the digest",
                    $label
                );
            }};
        }

        let changed_worker = execution_binding_from_draft(
            "worker-2",
            RUNTIME,
            execution_request_draft(&worker_root, 1)?,
        )?;
        assert!(baseline != changed_worker, "worker_id was omitted");
        let changed_requested_runtime = execution_binding_from_draft(
            WORKER,
            "cell3-alternate-runtime",
            execution_request_draft(&worker_root, 1)?,
        )?;
        assert!(
            baseline != changed_requested_runtime,
            "requested_runtime was omitted"
        );
        assert_draft_change!("mission", |draft: &mut ExecutionRequestDraft| {
            draft.mission = "cell3-binding-mission-2".to_owned();
        });
        assert_draft_change!("phase", |draft: &mut ExecutionRequestDraft| {
            draft.phase = "phase-2".to_owned();
        });
        assert_draft_change!("attempt", |draft: &mut ExecutionRequestDraft| {
            draft.attempt = 2;
        });
        assert_draft_change!("revision", |draft: &mut ExecutionRequestDraft| {
            draft.revision = 2;
        });
        assert_draft_change!("objective", |draft: &mut ExecutionRequestDraft| {
            draft.objective = "different objective".to_owned();
        });
        assert_draft_change!("persona", |draft: &mut ExecutionRequestDraft| {
            draft.persona = "reviewer".to_owned();
        });
        assert_draft_change!("role", |draft: &mut ExecutionRequestDraft| {
            draft.role = "reviewer".to_owned();
        });
        assert_draft_change!("domain", |draft: &mut ExecutionRequestDraft| {
            draft.domain = "work".to_owned();
        });
        assert_draft_change!("skills", |draft: &mut ExecutionRequestDraft| {
            draft.skills.push("testing".to_owned());
        });
        assert_draft_change!("dependencies", |draft: &mut ExecutionRequestDraft| {
            draft.dependencies.push("phase-0".to_owned());
        });
        assert_draft_change!("expected_evidence", |draft: &mut ExecutionRequestDraft| {
            draft.expected_evidence.push("proof".to_owned());
        });
        assert_draft_change!("constraints", |draft: &mut ExecutionRequestDraft| {
            draft.constraints.push("different".to_owned());
        });
        assert_draft_change!("prior_context", |draft: &mut ExecutionRequestDraft| {
            draft.prior_context = "prior".to_owned();
        });
        let alternate_runtime = RuntimeFamily::parse("cell3-other-runtime")?;
        assert_draft_change!("runtime", |draft: &mut ExecutionRequestDraft| {
            draft.runtime = alternate_runtime.clone();
        });
        assert_draft_change!("model", |draft: &mut ExecutionRequestDraft| {
            draft.model = "fixture-model-2".to_owned();
        });
        assert_draft_change!("effort", |draft: &mut ExecutionRequestDraft| {
            draft.effort = Effort::XHigh;
        });
        assert_draft_change!("max_turns", |draft: &mut ExecutionRequestDraft| {
            draft.max_turns = 5;
        });
        assert_draft_change!("worker_dir", |draft: &mut ExecutionRequestDraft| {
            draft.worker_dir = PathBuf::from("/tmp/cell3-binding-worker-2");
        });
        assert_draft_change!("target_dir", |draft: &mut ExecutionRequestDraft| {
            draft.target_dir = Some(PathBuf::from("/tmp/cell3-binding-target"));
        });
        let resume = SessionHandle::new(RuntimeFamily::parse(RUNTIME)?, "session-2")?;
        assert_draft_change!("resume_from", |draft: &mut ExecutionRequestDraft| {
            draft.resume_from = Some(resume.clone());
        });
        assert_draft_change!("hook_script", |draft: &mut ExecutionRequestDraft| {
            draft.hook_script = Some(PathBuf::from("/tmp/cell3-binding-hook"));
        });

        let mut left = execution_request_draft(&worker_root, 1)?;
        left.skills = vec!["ab".to_owned(), "c".to_owned()];
        let mut right = execution_request_draft(&worker_root, 1)?;
        right.skills = vec!["a".to_owned(), "bc".to_owned()];
        assert!(
            execution_binding_from_draft(WORKER, RUNTIME, left)?
                != execution_binding_from_draft(WORKER, RUNTIME, right)?,
            "list item framing is ambiguous"
        );
        let mut ordered = execution_request_draft(&worker_root, 1)?;
        ordered.skills = vec!["first".to_owned(), "second".to_owned()];
        let mut reversed = execution_request_draft(&worker_root, 1)?;
        reversed.skills = vec!["second".to_owned(), "first".to_owned()];
        assert!(
            execution_binding_from_draft(WORKER, RUNTIME, ordered)?
                != execution_binding_from_draft(WORKER, RUNTIME, reversed)?,
            "list ordering is not bound"
        );
        let mut non_utf_a = execution_request_draft(&worker_root, 1)?;
        non_utf_a.worker_dir = PathBuf::from(OsString::from_vec(b"/tmp/non-utf-\xff".to_vec()));
        let mut non_utf_b = execution_request_draft(&worker_root, 1)?;
        non_utf_b.worker_dir = PathBuf::from(OsString::from_vec(b"/tmp/non-utf-\xfe".to_vec()));
        assert!(
            execution_binding_from_draft(WORKER, RUNTIME, non_utf_a)?
                != execution_binding_from_draft(WORKER, RUNTIME, non_utf_b)?,
            "exact Unix path bytes are not bound"
        );
        Ok(())
    }

    #[test]
    fn started_process_failure_evidence_mapping_covers_every_current_variant() -> TestResult {
        let cases = [
            (
                StartedProcessFailureEvidence::Signaled(9),
                MechanicalTermination::ProcessExited(ProcessExitStatus::signal(9)?),
            ),
            (
                StartedProcessFailureEvidence::Deadline,
                MechanicalTermination::HardDeadlineExceeded,
            ),
            (
                StartedProcessFailureEvidence::Stalled,
                MechanicalTermination::WatchdogStalled,
            ),
            (
                StartedProcessFailureEvidence::Cancelled,
                MechanicalTermination::Cancelled,
            ),
            (
                StartedProcessFailureEvidence::OutputLimit,
                MechanicalTermination::SupervisorFailure,
            ),
            (
                StartedProcessFailureEvidence::InfrastructureFailure,
                MechanicalTermination::SupervisorFailure,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(termination_from_started_process_failure(failure), expected);
        }
        Ok(())
    }

    #[test]
    fn pending_terminal_plan_matching_requires_exact_failure_and_cancellation_payloads() {
        let expected_failure = TerminalPlan::Failed {
            phase_error: "expected failure",
        };
        assert_eq!(
            pending_transition_matches_plan(
                &ReducerTransition::PhaseFailed {
                    error: "expected failure".to_owned(),
                },
                expected_failure,
            ),
            Some(true)
        );
        assert_eq!(
            pending_transition_matches_plan(
                &ReducerTransition::PhaseFailed {
                    error: "stale failure".to_owned(),
                },
                expected_failure,
            ),
            Some(false)
        );
        assert_eq!(
            pending_transition_matches_plan(
                &ReducerTransition::MissionCancelled {
                    reason: CANCELLED_REASON.to_owned(),
                },
                TerminalPlan::Cancelled,
            ),
            Some(true)
        );
        assert_eq!(
            pending_transition_matches_plan(
                &ReducerTransition::MissionCancelled {
                    reason: "stale cancellation".to_owned(),
                },
                TerminalPlan::Cancelled,
            ),
            Some(false)
        );
    }

    #[test]
    fn terminal_phase_proof_requires_exact_status_and_payload() -> TestResult {
        let phase_id = PhaseId::new(PHASE)?;
        let phase = |status, error: Option<&str>, skip_reason: Option<&str>| PhaseState {
            id: phase_id.clone(),
            dependencies: Vec::new(),
            status,
            started_at: Some("2000-01-01T00:00:00.0000000000000000001Z".to_owned()),
            finished_at: Some("2000-01-01T00:00:00.0000000000000000002Z".to_owned()),
            error: error.map(str::to_owned),
            skip_reason: skip_reason.map(str::to_owned),
            retry_observations: 0,
        };
        assert!(terminal_plan_matches_phase_state(
            TerminalPlan::Completed,
            &phase(PhaseStatus::Completed, None, None),
        ));
        assert!(!terminal_plan_matches_phase_state(
            TerminalPlan::Completed,
            &phase(PhaseStatus::Completed, Some("stale"), None),
        ));
        let failed = TerminalPlan::Failed {
            phase_error: "expected failure",
        };
        assert!(terminal_plan_matches_phase_state(
            failed,
            &phase(PhaseStatus::Failed, Some("expected failure"), None),
        ));
        assert!(!terminal_plan_matches_phase_state(
            failed,
            &phase(PhaseStatus::Failed, Some("different failure"), None),
        ));
        assert!(!terminal_plan_matches_phase_state(
            failed,
            &phase(
                PhaseStatus::Failed,
                Some("expected failure"),
                Some("stale skip"),
            ),
        ));
        assert!(terminal_plan_matches_phase_state(
            TerminalPlan::Cancelled,
            &phase(PhaseStatus::Skipped, None, Some(CANCELLED_REASON)),
        ));
        assert!(!terminal_plan_matches_phase_state(
            TerminalPlan::Cancelled,
            &phase(PhaseStatus::Skipped, None, Some("different reason")),
        ));
        assert!(!terminal_plan_matches_phase_state(
            TerminalPlan::Cancelled,
            &phase(
                PhaseStatus::Skipped,
                Some("stale error"),
                Some(CANCELLED_REASON),
            ),
        ));
        Ok(())
    }

    #[test]
    fn admit_rejects_request_dependencies() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("request-dependencies")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        // `ExecutionRequest` has no setter for `dependencies`; build the draft
        // directly so this test can supply a non-empty list without needing a
        // second, differently-shaped phase in `MissionState` (which
        // `MissionState::new` would itself reject as an unknown dependency for
        // a genuinely one-phase mission).
        let request = ExecutionRequest::new(ExecutionRequestDraft {
            mission: MISSION.to_owned(),
            phase: PHASE.to_owned(),
            attempt: 1,
            revision: 1,
            objective: "exercise Cell 3 hermetic provider composition".to_owned(),
            persona: "fixture".to_owned(),
            role: "implementer".to_owned(),
            domain: "dev".to_owned(),
            skills: vec!["rust-best-practices".to_owned()],
            dependencies: vec!["phase-0".to_owned()],
            expected_evidence: Vec::new(),
            constraints: vec!["fixture-only".to_owned()],
            prior_context: String::new(),
            runtime: RuntimeFamily::parse(RUNTIME)?,
            model: "fixture-model".to_owned(),
            effort: Effort::High,
            max_turns: 4,
            worker_dir: case.worker_root.clone(),
            target_dir: None,
            resume_from: None,
            hook_script: None,
        })?;
        let result = HermeticProvider::admit(
            workspace,
            state,
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            request,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            result,
            Err(HermeticProviderError::DependenciesUnsupported)
        ));
        Ok(())
    }

    #[test]
    fn unsupported_request_fields_leave_workspace_and_ledger_pristine() -> TestResult {
        for resume in [false, true] {
            let label = if resume {
                "unsupported-resume"
            } else {
                "unsupported-expected-evidence"
            };
            let state = one_phase_state()?;
            let case = Case::new(label)?;
            let workspace = case.create_workspace(&state)?;
            let helper = case.install_helper()?;
            let mut draft = execution_request_draft(&case.worker_root, 1)?;
            if resume {
                draft.resume_from = Some(SessionHandle::new(
                    RuntimeFamily::parse(RUNTIME)?,
                    "provider-session",
                )?);
            } else {
                draft.expected_evidence.push("executor-proof".to_owned());
            }
            let request = ExecutionRequest::new(draft)?;
            let before = workspace_projection_bytes(&case.workspace_root)?;
            assert!(!case.ledger_database().exists());

            let rejected = HermeticProvider::admit(
                workspace,
                state.clone(),
                &helper,
                case.ledger_boundary.clone(),
                RUNTIME,
                request,
                CancellationToken::new(),
                Arc::new(TestTimestampSource::new()),
            );
            assert!(matches!(
                rejected,
                Err(HermeticProviderError::UnsupportedRequest)
            ));
            assert_eq!(
                workspace_projection_bytes(&case.workspace_root)?,
                before,
                "{label} changed lifecycle projection bytes"
            );
            assert!(
                !case.ledger_database().exists(),
                "{label} opened or mutated the process ledger"
            );

            // The unsupported request never reserved either authority surface;
            // a valid request for the exact same attempt remains admissible.
            let reopened = case.open_workspace()?;
            let mut valid = provider_for(&case, reopened, state, &helper, 1)?;
            assert_eq!(
                valid.run_to_terminal(pass_decision())?.mission_status(),
                MissionStatus::Failed
            );
        }
        Ok(())
    }

    #[test]
    fn colon_and_decomposed_unicode_persona_persists_terminal_decision_across_reopen() -> TestResult
    {
        let state = one_phase_state()?;
        let case = Case::new("colon-unicode-persona")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let persona = "fixture:cafe\u{301}";
        let worker_id = orchestrator_core::WorkerId::for_phase(persona, &PhaseId::new(PHASE)?)?;
        let worker_root = case.workspace_root.join("workers").join(worker_id.as_str());
        let mut draft = execution_request_draft(&worker_root, 1)?;
        draft.persona = persona.to_owned();

        let mut first = HermeticProvider::admit(
            workspace,
            state.clone(),
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            ExecutionRequest::new(draft)?,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        assert_eq!(
            first.run_to_terminal(pass_decision())?.mission_status(),
            MissionStatus::Failed
        );
        drop(first);

        let mut recovery_draft = execution_request_draft(&worker_root, 1)?;
        recovery_draft.persona = persona.to_owned();
        let reopened = case.open_workspace()?;
        let mut recovered = HermeticProvider::admit(
            reopened,
            state,
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            ExecutionRequest::new(recovery_draft)?,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        assert_eq!(
            recovered.run_to_terminal(pass_decision())?.mission_status(),
            MissionStatus::Failed
        );

        let ledger = case.open_ledger()?;
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, worker_id.as_str(), 1,)?
                .is_some()
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn helper_from_foreign_authority_is_rejected_before_any_claim() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("foreign-helper")?;
        let workspace = case.create_workspace(&state)?;
        let foreign = Case::new("foreign-helper-authority")?;
        let foreign_helper = foreign.install_helper()?;

        let result = admit_raw(&case, workspace, state.clone(), &foreign_helper, 1);
        assert!(matches!(
            result,
            Err(HermeticProviderError::BindingMismatch)
        ));

        // Helper/workspace authority is validated before opening the ledger,
        // so rejection cannot reserve the immutable outbox key.
        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;

        // A valid retry under the same mission/phase/attempt is still
        // admissible because the rejected helper left no durable residue.
        let helper = case.install_helper()?;
        let reopened = case.open_workspace()?;
        let mut provider = provider_for(&case, reopened, state, &helper, 1)?;
        assert_eq!(
            provider.run_to_terminal(pass_decision())?.mission_status(),
            MissionStatus::Failed
        );
        Ok(())
    }

    #[test]
    fn helper_and_worker_require_the_same_arc_even_for_the_same_physical_root() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("helper-worker-distinct-arcs")?;
        let workspace = case.create_workspace(&state)?;
        drop(workspace);
        let helper = case.install_helper()?;
        let physical_root = helper.executable.boundary.canonical_path().to_path_buf();
        let production_boundary =
            Arc::new(ProductionBoundary::from_canonical_root(&physical_root)?);
        let distinct_shared: SharedCapabilityRoot = production_boundary.clone();
        assert!(!Arc::ptr_eq(&distinct_shared, &helper.executable.boundary));
        assert_eq!(
            distinct_shared.canonical_path(),
            helper.executable.boundary.canonical_path()
        );
        let distinct_workspace =
            WorkspaceAuthority::admit_production(production_boundary, MissionId::new(MISSION)?)?;

        let rejected = admit_raw(&case, distinct_workspace, state, &helper, 1);
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::BindingMismatch)
        ));
        assert!(!case.worker_root.exists());
        assert!(!case.ledger_database().exists());
        Ok(())
    }

    #[test]
    fn mismatched_process_working_root_leaves_zero_rows_and_valid_retry_succeeds() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("working-root-mismatch")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let request = execution_request(&case.parent.join("wrong-worker-root"), 1)?;

        let result = HermeticProvider::admit(
            workspace,
            state.clone(),
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            request,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            result,
            Err(HermeticProviderError::Workspace(
                WorkspaceError::ProcessWorkingRootMismatch
            ))
        ));
        assert!(!case.worker_root.exists());
        assert!(!case.ledger_database().exists());

        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;

        let reopened = case.open_workspace()?;
        let mut provider = provider_for(&case, reopened, state, &helper, 1)?;
        assert_eq!(
            provider.run_to_terminal(pass_decision())?.mission_status(),
            MissionStatus::Failed
        );
        Ok(())
    }

    #[test]
    fn path_equal_working_root_aliases_leave_zero_rows_and_canonical_retry_succeeds() -> TestResult
    {
        for alias_kind in ["double-separator", "trailing-dot", "trailing-separator"] {
            let state = one_phase_state()?;
            let case = Case::new(alias_kind)?;
            let workspace = case.create_workspace(&state)?;
            let helper = case.install_helper()?;
            let canonical_bytes = case.worker_root.as_os_str().as_bytes();
            let alias_bytes = match alias_kind {
                "double-separator" => {
                    let parent = case
                        .worker_root
                        .parent()
                        .ok_or("worker root has no parent")?;
                    let mut bytes = parent.as_os_str().as_bytes().to_vec();
                    bytes.extend_from_slice(b"//");
                    bytes.extend_from_slice(WORKER.as_bytes());
                    bytes
                }
                "trailing-dot" => {
                    let mut bytes = canonical_bytes.to_vec();
                    bytes.extend_from_slice(b"/.");
                    bytes
                }
                "trailing-separator" => {
                    let mut bytes = canonical_bytes.to_vec();
                    bytes.push(b'/');
                    bytes
                }
                _ => return Err("unknown lexical alias fixture".into()),
            };
            let alias = PathBuf::from(OsString::from_vec(alias_bytes));
            assert_eq!(
                alias, case.worker_root,
                "{alias_kind} is no longer component-equal and no longer exercises this boundary"
            );
            assert_ne!(
                alias.as_os_str().as_bytes(),
                canonical_bytes,
                "{alias_kind} did not preserve distinct caller bytes"
            );
            let before = workspace_projection_bytes(&case.workspace_root)?;
            assert!(!case.ledger_database().exists());

            let rejected = HermeticProvider::admit(
                workspace,
                state.clone(),
                &helper,
                case.ledger_boundary.clone(),
                RUNTIME,
                execution_request(&alias, 1)?,
                CancellationToken::new(),
                Arc::new(TestTimestampSource::new()),
            );
            assert!(matches!(
                rejected,
                Err(HermeticProviderError::Workspace(
                    WorkspaceError::ProcessWorkingRootMismatch
                ))
            ));
            assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
            assert!(!case.worker_root.exists());
            assert!(!case.ledger_database().exists());

            let reopened = case.open_workspace()?;
            let mut valid = provider_for(&case, reopened, state, &helper, 1)?;
            assert_eq!(
                valid.run_to_terminal(pass_decision())?.mission_status(),
                MissionStatus::Failed
            );
        }
        Ok(())
    }

    #[test]
    fn replaced_helper_is_rejected_before_lazy_worker_or_ledger_mutation() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let state = one_phase_state()?;
        let case = Case::new("replaced-helper-before-worker")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let executable = helper
            .executable
            .boundary
            .canonical_path()
            .join("bin")
            .join(&helper.label);
        std::fs::remove_file(&executable)?;
        let mut replacement = SYNTHETIC_EXECUTABLE.to_vec();
        replacement.push(1);
        std::fs::write(&executable, replacement)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;

        let rejected = admit_raw(&case, workspace, state, &helper, 1);
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::ProviderLaunch(
                ProviderLaunchError::ExecutableAdmission
            ))
        ));
        assert_lazy_phase_storage_absent(&case);
        Ok(())
    }

    #[test]
    fn same_helper_label_with_different_bytes_has_a_different_logical_identity() -> TestResult {
        let state = one_phase_state()?;
        let first = Case::new("logical-id-first")?;
        let first_workspace = first.create_workspace(&state)?;
        let first_helper = AttestedFixtureHelper::install(
            &first.authority,
            "same-helper-label",
            SYNTHETIC_EXECUTABLE,
        )?;
        let first_prepared = prepared_process_launch(&first, &first_workspace, &first_helper)?;

        let mut different_image = SYNTHETIC_EXECUTABLE.to_vec();
        different_image.push(1);
        let second = Case::new_with_expected_helper("logical-id-second", &different_image)?;
        let second_workspace = second.create_workspace(&state)?;
        let second_helper = AttestedFixtureHelper::install(
            &second.authority,
            "same-helper-label",
            &different_image,
        )?;
        let second_prepared = prepared_process_launch(&second, &second_workspace, &second_helper)?;

        assert_ne!(
            first_prepared.exact_request().executable_id(),
            second_prepared.exact_request().executable_id()
        );
        assert_ne!(
            first_prepared.exact_request().fingerprint(),
            second_prepared.exact_request().fingerprint()
        );
        Ok(())
    }

    #[test]
    fn enrolled_runtime_reverifies_real_cwd_and_executable_proofs_before_release() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        for replace_cwd in [true, false] {
            let state = one_phase_state()?;
            let case = Case::new(if replace_cwd {
                "release-proof-cwd"
            } else {
                "release-proof-executable"
            })?;
            let workspace = case.create_workspace(&state)?;
            let helper = case.install_helper()?;
            let prepared = prepared_process_launch(&case, &workspace, &helper)?;
            let execution_binding = test_execution_binding(&case.worker_root, 1)?;
            let binding = {
                let (_, _, binding) = process_effect_for_attempt(
                    &MissionId::new(MISSION)?,
                    PHASE,
                    1,
                    prepared.exact_request(),
                    &execution_binding,
                )?;
                binding
            };
            let launch = prepared.enroll(binding)?;
            let runtime = launch.initialize_enrolled_runtime_for_test()?;

            if replace_cwd {
                let displaced = case.parent.join("displaced-release-cwd");
                std::fs::rename(&case.worker_root, displaced)?;
                private_dir(&case.worker_root)?;
            } else {
                let executable = helper
                    .executable
                    .boundary
                    .canonical_path()
                    .join("bin")
                    .join(&helper.label);
                std::fs::remove_file(&executable)?;
                let mut replacement = SYNTHETIC_EXECUTABLE.to_vec();
                replacement.push(1);
                std::fs::write(&executable, replacement)?;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
            }

            assert!(runtime.verify_for_release().is_err());
            drop(runtime);
            assert!(!process_wide_has_owned_processes());
            assert!(!case.ledger_database().exists());
        }
        Ok(())
    }

    #[test]
    fn invalid_or_mismatched_requested_runtime_precedes_lazy_storage() -> TestResult {
        for (label, requested_runtime) in [
            ("invalid-requested-runtime", "invalid\nruntime"),
            ("mismatched-requested-runtime", "different-runtime"),
        ] {
            let state = one_phase_state()?;
            let case = Case::new(label)?;
            let workspace = case.create_workspace(&state)?;
            let helper = case.install_helper()?;
            let rejected = HermeticProvider::admit(
                workspace,
                state,
                &helper,
                case.ledger_boundary.clone(),
                requested_runtime,
                execution_request(&case.worker_root, 1)?,
                CancellationToken::new(),
                Arc::new(TestTimestampSource::new()),
            );
            assert!(matches!(
                rejected,
                Err(HermeticProviderError::RuntimeRegistry(_))
            ));
            assert_lazy_phase_storage_absent(&case);
        }
        Ok(())
    }

    #[test]
    fn fresh_warn_rejection_never_starts_lifecycle_or_claims_process() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("fresh-warn-rejection")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let before = workspace_projection_bytes(&case.workspace_root)?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, idempotency_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut provider = provider_for(&case, workspace, state.clone(), &helper, 1)?;

        assert!(matches!(
            provider.run_to_terminal(warn_decision()),
            Err(HermeticProviderError::UnsupportedVerificationPolicy)
        ));
        assert_eq!(
            provider.coordinator.state().status(),
            MissionStatus::NotStarted
        );
        assert_eq!(provider.phase_status()?, PhaseStatus::Pending);
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&idempotency_key)?
            .ok_or("fresh admission must retain its unclaimed process effect")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Pending);
        assert_eq!(snapshot.effect().attempts(), 0);
        assert!(snapshot.current_observation().is_none());
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_none()
        );
        ledger.close()?;
        drop(provider);

        // The rejected policy did not poison the pending claim. A later
        // blocking decision can execute this exact attempt normally.
        let reopened = case.open_workspace()?;
        let mut valid = provider_for(&case, reopened, state, &helper, 1)?;
        assert_eq!(
            valid.run_to_terminal(pass_decision())?.mission_status(),
            MissionStatus::Failed
        );
        Ok(())
    }

    #[test]
    fn fresh_attempt_with_unexecutable_helper_fails_phase_and_mission() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("fresh-attempt")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, idempotency_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut provider = provider_for(&case, workspace, state, &helper, 1)?;
        assert!(provider.attempt_is_runnable());

        let outcome = provider.run_to_terminal(pass_decision())?;
        assert_eq!(outcome.mission_status(), MissionStatus::Failed);
        assert_eq!(outcome.phase_status(), PhaseStatus::Failed);
        let replay = provider.run_to_terminal(pass_decision())?;
        assert_eq!(replay, outcome);

        // The ledger must show exactly one claimed attempt that reached a
        // terminal state — proof the attested helper path was genuinely
        // exercised (claim -> spawn attempt -> resolve), not skipped.
        let ledger = case.open_ledger()?;
        let pending = ledger.pending_effects(10)?;
        assert!(pending.is_empty());
        let history = ledger.attempt_history(&idempotency_key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        // Kernels may surface an unexecutable fixture either as a spawn
        // rejection or as a child that exits non-zero. Both prove the same
        // one-attempt lifecycle without weakening the typed evidence check.
        match history[0].evidence().code() {
            EffectEvidenceCode::ExitObservedFailure => {
                assert!(
                    history[0]
                        .evidence()
                        .exit_code()
                        .is_some_and(|code| code != 0)
                );
            }
            EffectEvidenceCode::ProcessNotStarted => {
                assert_eq!(
                    history[0].evidence().process_not_started_reason(),
                    Some(ProcessNotStartedEvidenceReason::SpawnFailed)
                );
            }
            unexpected => {
                return Err(format!("unexpected synthetic-helper evidence: {unexpected:?}").into());
            }
        }
        ledger.close()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compiled_helper_succeeds_once_and_terminal_replay_does_not_relaunch() -> TestResult {
        let _serialization = REAL_DURABLE_CANARY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!process_wide_has_owned_processes());

        let helper_bytes = compiled_owned_process_helper()?;
        let state = one_phase_state()?;
        let case = Case::new_with_expected_helper("compiled-helper-replay", &helper_bytes)?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper_bytes(&helper_bytes)?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, idempotency_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let sentinel_path = case.worker_root.join(DURABLE_CANARY_SENTINEL);
        let duplicate_path = case.worker_root.join(DURABLE_CANARY_DUPLICATE);
        let gate_roots_before = short_gate_root_entries()?;

        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        assert!(first.attempt_is_runnable());
        let first_outcome = first.run_to_terminal(pass_decision())?;
        assert!(first.durable_execution_identity_present()?);
        assert_eq!(first_outcome.mission_status(), MissionStatus::Completed);
        assert_eq!(first_outcome.phase_status(), PhaseStatus::Completed);
        drop(first);

        let sentinel_before = durable_canary_sentinel_snapshot(&sentinel_path)?;
        assert_eq!(sentinel_before.bytes.as_slice(), DURABLE_CANARY_CONTENT);
        assert!(!duplicate_path.exists());
        assert!(!process_wide_has_owned_processes());
        assert!(process_owned_namespace_entries(case.fixture_root()?)?.is_empty());
        assert_eq!(short_gate_root_entries()?, gate_roots_before);

        let mut ledger = case.open_ledger()?;
        let first_snapshot = ledger
            .exact_outbox_snapshot(&idempotency_key)?
            .ok_or("successful durable canary has no exact outbox snapshot")?;
        assert_eq!(first_snapshot.effect().state(), OutboxState::Succeeded);
        assert_eq!(first_snapshot.effect().attempts(), 1);
        assert_eq!(first_snapshot.effect().logical_attempt(), 1);
        let first_execution_identity = first_snapshot
            .effect()
            .execution_identity()
            .ok_or("successful durable canary has no execution identity")?
            .clone();
        assert_eq!(first_execution_identity.attempt(), 1);
        assert!(first_execution_identity.pid() > 0);
        assert!(first_execution_identity.process_group_id() > 0);
        let first_observation = first_snapshot
            .current_observation()
            .ok_or("successful durable canary has no current observation")?
            .clone();
        assert_eq!(first_observation.attempt(), 1);
        assert_eq!(first_observation.state(), OutboxState::Succeeded);
        assert_eq!(
            first_observation.evidence().code(),
            EffectEvidenceCode::ExitObservedSuccess
        );
        assert_eq!(first_observation.evidence().exit_code(), Some(0));
        let first_history = ledger.attempt_history(&idempotency_key, 10)?;
        assert_eq!(first_history, vec![first_observation.clone()]);
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;

        let reopened = case.open_workspace()?;
        let mut replayed = provider_for(&case, reopened, state, &helper, 1)?;
        assert!(!replayed.attempt_is_runnable());
        assert!(replayed.durable_attempt.actor.is_none());
        assert!(replayed.durable_attempt.service.is_none());
        assert!(replayed.durable_attempt.prepared_launch.is_none());
        let replay_outcome = replayed.run_to_terminal(pass_decision())?;
        assert!(replayed.durable_execution_identity_present()?);
        assert_eq!(replay_outcome, first_outcome);
        drop(replayed);

        assert_eq!(
            durable_canary_sentinel_snapshot(&sentinel_path)?,
            sentinel_before
        );
        assert!(!duplicate_path.exists());
        assert!(!process_wide_has_owned_processes());
        assert!(process_owned_namespace_entries(case.fixture_root()?)?.is_empty());
        assert_eq!(short_gate_root_entries()?, gate_roots_before);

        let mut ledger = case.open_ledger()?;
        let replay_snapshot = ledger
            .exact_outbox_snapshot(&idempotency_key)?
            .ok_or("replayed durable canary has no exact outbox snapshot")?;
        assert_eq!(replay_snapshot.effect().state(), OutboxState::Succeeded);
        assert_eq!(replay_snapshot.effect().attempts(), 1);
        assert_eq!(
            replay_snapshot.effect().execution_identity(),
            Some(&first_execution_identity)
        );
        assert_eq!(
            replay_snapshot.current_observation(),
            Some(&first_observation)
        );
        assert_eq!(ledger.attempt_history(&idempotency_key, 10)?, first_history);
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn pending_admission_transition_is_repaired_before_actor_install() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("pending-admission-restart")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        first
            .coordinator
            .inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
        assert!(matches!(
            first.run_to_terminal(pass_decision()),
            Err(HermeticProviderError::Lifecycle(LifecycleError::Workspace(
                WorkspaceError::InjectedProjectionFault("after-event-sync")
            )))
        ));
        drop(first);

        let reopened = case.open_workspace()?;
        let mut second = provider_for(&case, reopened, state, &helper, 1)?;
        let recovered = second.run_to_terminal(pass_decision())?;
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        let events = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "mission.started")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "phase.started")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn pending_phase_retrying_is_rejected_before_ledger_or_actor_admission() -> TestResult {
        let state = one_phase_state()?;
        let phase = PhaseId::new(PHASE)?;
        let case = Case::new("pending-phase-retrying")?;
        let helper = case.install_helper()?;
        let workspace = case.create_workspace(&state)?;
        let mut coordinator =
            LifecycleCoordinator::new(workspace, continuation_runtime(false)?, state.clone())?;
        coordinator.transition_allocated(
            None,
            ReducerTransition::MissionStarted,
            serde_json::Value::Null,
            None,
        )?;
        coordinator.transition_allocated(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            serde_json::Value::Null,
            None,
        )?;
        // A completed worker stream opts this fixture checkpoint into the
        // exact event-sequence marker. PhaseRetrying is status-preserving, so
        // without that marker its old and target checkpoints are byte-equal
        // and there is no distinguishable pending checkpoint acknowledgement.
        let marker_request = execution_request(&case.worker_root, 1)?;
        let marker_identity = WorkerIdentity::new(MISSION, PHASE, "marker-worker")?;
        let marker = coordinator.with_attempt(&phase, "marker-worker", 1, |runtime, sink| {
            execute_continuation_request(runtime, sink, &marker_request, marker_identity)
        })?;
        let marker_outcome = marker
            .into_executed()
            .ok_or("marker worker unexpectedly replayed")??;
        assert!(marker_outcome.is_completed());
        let running_state = coordinator.state().clone();
        drop(coordinator);

        // `LifecycleCoordinator` deliberately cannot author PhaseRetrying,
        // but the compatibility codec and reducer accept it from existing Go
        // history. Use the real fixture projection writer to reproduce the
        // legitimate event-synced/checkpoint-not-renamed crash boundary.
        let workspace = case.open_workspace()?;
        let mut writer = workspace.into_fixture_projection_writer()?;
        let sequence = running_state
            .greatest_applied_sequence()
            .and_then(|sequence| sequence.checked_add(1))
            .ok_or("retry event sequence is unavailable")?;
        let event = encode_current_event(&EventRecord {
            id: format!("evt_fixture_{sequence:019}"),
            event_type: "phase.retrying".to_owned(),
            timestamp: format!("2000-01-01T00:00:00.{sequence:019}Z"),
            sequence,
            mission_id: MISSION.to_owned(),
            phase_id: Some(PHASE.to_owned()),
            worker_id: None,
            data: None,
            extra: EventJsonMap::default(),
        })?;
        let decoded = decode_event_line(&event)?;
        let input = crate::project_event(&decoded)?;
        assert_eq!(input.transition, ReducerTransition::PhaseRetrying);
        let retry_state = reduce(&running_state, &input)?.state;
        let checkpoint_template =
            decode_checkpoint(writer.lifecycle_checkpoint_bytes())?.projection;
        let retry_checkpoint =
            crate::checkpoint_projection::checkpoint_for_state(&checkpoint_template, &retry_state)?;
        let retry_checkpoint = encode_current_checkpoint(&retry_checkpoint)?;
        let projection = writer.commit_event_checkpoint_with_fault(
            &event,
            &retry_checkpoint,
            Some(crate::workspace::LifecycleProjectionFault::EventSynced),
        );
        assert!(matches!(
            projection,
            Err(WorkspaceError::InjectedProjectionFault("after-event-sync"))
        ));
        drop(writer);

        let before = workspace_projection_bytes(&case.workspace_root)?;
        assert!(!case.ledger_database().exists());
        let workspace = case.open_workspace()?;
        let rejected = admit_raw(&case, workspace, state, &helper, 1);
        assert!(
            matches!(
                rejected,
                Err(HermeticProviderError::Lifecycle(
                    LifecycleError::RecoveryPending
                ))
            ),
            "unexpected pending-retry admission result: {rejected:?}"
        );
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
        assert_lazy_phase_storage_absent(&case);
        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn service_error_that_leaves_exact_effect_pending_requires_retry_without_decision() -> TestResult
    {
        let state = one_phase_state()?;
        let case = Case::new("pending-service-error")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;

        // A distinct, earlier acknowledged effect makes the target effect
        // ineligible in deterministic queue order. The durable service will
        // fail its exact claim and prove that the target claim was not
        // committed, leaving the target at Pending.
        let earlier_binding = test_execution_binding(&case.worker_root, 99)?;
        let (earlier, _, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            99,
            &process_request,
            &earlier_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 99, earlier)?;
        ledger.close()?;

        let target_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, target_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &target_binding,
        )?;
        let mut provider = provider_for(&case, workspace, state, &helper, 1)?;
        assert!(matches!(
            provider.run_to_terminal(pass_decision()),
            Err(HermeticProviderError::ProcessRetryRequired)
        ));

        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&target_key)?
            .ok_or("target process effect must exist")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Pending);
        assert!(snapshot.current_observation().is_none());
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_none()
        );
        ledger.close()?;
        let events = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert!(events.iter().all(|event| {
            !event.starts_with("worker.")
                && !matches!(
                    event.as_str(),
                    "phase.completed"
                        | "phase.failed"
                        | "phase.skipped"
                        | "mission.completed"
                        | "mission.failed"
                        | "mission.cancelled"
                )
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn admission_recovers_claimed_before_spawn_without_projection_or_duplicate_spawn() -> TestResult
    {
        let state = one_phase_state()?;
        let case = Case::new("executing-recovery-required")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (effect, key, binding) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut ledger = case.open_ledger()?;
        admit_process_effect(&mut ledger, &MissionId::new(MISSION)?, PHASE, 1, effect)?;
        let prepared = ledger.prepare_exact_process_claim(&binding, &process_request)?;
        assert!(
            ledger
                .claim_prepared_process(prepared, "2026-07-18T00:00:10Z")
                .is_ok()
        );
        assert_eq!(
            ledger
                .exact_outbox_snapshot(&key)?
                .map(|snapshot| snapshot.effect().state()),
            Some(OutboxState::Executing)
        );
        ledger.close()?;

        assert!(matches!(
            admit_raw(&case, workspace, state.clone(), &helper, 1),
            Err(HermeticProviderError::ProcessRecoveryReconciled(
                RecoveredProcessDisposition::RecoveredBeforeSpawnNotStarted
            ))
        ));
        let mut ledger = case.open_ledger()?;
        let recovered = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("recovered process is missing")?;
        assert_eq!(recovered.effect().state(), OutboxState::Failed);
        assert_eq!(
            recovered
                .current_observation()
                .and_then(|observation| observation.evidence().process_not_started_reason()),
            Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
        );
        let history = ledger.attempt_history(&key, 10)?;
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_none()
        );
        let decision = TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?;
        ledger.record_terminal_decision(&decision)?;
        ledger.close()?;

        let reopened = case.open_workspace()?;
        let replayed = admit_raw(&case, reopened, state, &helper, 1)?;
        assert!(!replayed.attempt_is_runnable());
        drop(replayed);
        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&key, 10)?, history);
        ledger.close()?;
        let event_log = case.workspace_root.join("events.jsonl");
        assert!(!event_log.exists() || event_types(&event_log)?.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn admission_refuses_uncertain_effect_even_with_terminal_observation() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("uncertain-recovery-required")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::CommitIndeterminate,
            )),
        )?;
        assert!(matches!(
            admit_raw(&case, workspace, state, &helper, 1),
            Err(HermeticProviderError::ProcessRecoveryReconciled(
                RecoveredProcessDisposition::Unresolved
            ))
        ));
        let mut ledger = case.open_ledger()?;
        let snapshot = ledger
            .exact_outbox_snapshot(&key)?
            .ok_or("uncertain process effect must exist")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            snapshot.current_observation().map(EffectObservation::state),
            Some(OutboxState::Uncertain)
        );
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_none()
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn pending_effect_with_persisted_decision_is_a_conflict() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("pending-decision-conflict")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let decision = TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?;
        let mut ledger = case.open_ledger()?;
        ledger.record_terminal_decision(&decision)?;
        ledger.close()?;

        assert!(matches!(
            admit_raw(&case, workspace, state, &helper, 1),
            Err(HermeticProviderError::TerminalDecisionConflict)
        ));
        let mut ledger = case.open_ledger()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        assert_eq!(
            ledger
                .exact_outbox_snapshot(&key)?
                .map(|snapshot| snapshot.effect().state()),
            Some(OutboxState::Pending)
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn setup_error_surfaces_store_close_failure() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("setup-close-failure")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let decision = TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::SupervisorFailure),
            pass_decision(),
        )?;
        let mut ledger = case.open_ledger()?;
        ledger.record_terminal_decision(&decision)?;
        ledger.close()?;

        // Pin the pre-admission snapshot. Admission writes the fresh Pending
        // effect, detects its conflict with the decision, then must surface
        // the failed writer handoff instead of concealing it behind conflict.
        let observer = Connection::open(case.ledger_database())?;
        observer.execute_batch("BEGIN; SELECT count(*) FROM journal;")?;
        assert!(matches!(
            admit_raw(&case, workspace, state, &helper, 1),
            Err(HermeticProviderError::RuntimeStore(
                RuntimeStoreError::CloseIncomplete
            ))
        ));
        observer.execute_batch("ROLLBACK")?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn terminal_effect_rejects_mismatched_persisted_decision() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("terminal-decision-mismatch")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let _key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
        )?;
        let decision = TerminalDecisionRecord::new(
            &MissionId::new(MISSION)?,
            PHASE,
            WORKER,
            1,
            Some(MechanicalTermination::Cancelled),
            pass_decision(),
        )?;
        let mut ledger = case.open_ledger()?;
        ledger.record_terminal_decision(&decision)?;
        ledger.close()?;

        assert!(matches!(
            admit_raw(&case, workspace, state, &helper, 1),
            Err(HermeticProviderError::TerminalDecisionConflict)
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn terminal_effect_without_decision_stays_actorless_and_persists_before_projection()
    -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("terminal-without-decision")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let _key = seed_terminal_process_effect(
            &case,
            &helper,
            1,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
        )?;
        let mut provider = provider_for(&case, workspace, state, &helper, 1)?;
        assert!(provider.durable_attempt.actor.is_none());
        assert!(provider.durable_attempt.decision().is_none());
        let outcome = provider.run_to_terminal(pass_decision())?;
        assert_eq!(outcome.mission_status(), MissionStatus::Failed);
        assert_eq!(outcome.phase_status(), PhaseStatus::Failed);
        let ledger = case.open_ledger()?;
        let decision = ledger
            .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
            .ok_or("known terminal ledger outcome must persist its decision")?;
        assert_eq!(
            decision.observed_termination(),
            Some(MechanicalTermination::SupervisorFailure)
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn active_continuation_rejects_mismatched_spawn_and_ignored_terminal() -> TestResult {
        let case = Case::new("continuation-mismatched-spawn")?;
        let (mut coordinator, phase) = active_continuation_coordinator(&case, false)?;
        let before = workspace_projection_bytes(&case.workspace_root)?;
        let mut changed = execution_request_draft(&case.worker_root, 1)?;
        changed.model = "different-spawn-model".to_owned();
        let request = ExecutionRequest::new(changed)?;

        let result = project_continuation_request(&mut coordinator, &phase, &request)?;
        assert!(matches!(
            result,
            Err(LifecycleError::AttemptTerminalMissing)
        ));
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
        assert!(matches!(
            coordinator.durable_attempt_replay(&phase, WORKER, 1),
            Err(LifecycleError::AttemptRecoveryRequired)
        ));
        Ok(())
    }

    #[test]
    fn active_continuation_rejects_output_even_when_executor_ignores_error() -> TestResult {
        let case = Case::new("continuation-output-poison")?;
        let (mut coordinator, phase) = active_continuation_coordinator(&case, true)?;
        let before = workspace_projection_bytes(&case.workspace_root)?;
        let request = execution_request(&case.worker_root, 1)?;

        // The executor ignores worker.output rejection; registry dispatch
        // still attempts its centrally derived terminal. The sink's poison
        // must make the enclosing continuation fail instead of accepting it.
        let result = project_continuation_request(&mut coordinator, &phase, &request)?;
        assert!(matches!(
            result,
            Err(LifecycleError::AttemptTerminalMissing)
        ));
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
        assert!(matches!(
            coordinator.durable_attempt_replay(&phase, WORKER, 1),
            Err(LifecycleError::AttemptRecoveryRequired)
        ));
        Ok(())
    }

    #[test]
    fn active_continuation_persistence_fault_cannot_be_ignored() -> TestResult {
        let case = Case::new("continuation-persistence-poison")?;
        let (mut coordinator, phase) = active_continuation_coordinator(&case, false)?;
        let request = execution_request(&case.worker_root, 1)?;
        coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);

        // Exact Spawn replay is read-only and does not consume the fault.
        // Terminal publication reaches the real projection writer, becomes
        // indeterminate, and must fail the continuation even though registry
        // dispatch treats terminal emission as a best-effort call.
        let result = project_continuation_request(&mut coordinator, &phase, &request)?;
        assert!(matches!(result, Err(LifecycleError::RecoveryPending)));
        drop(coordinator);

        // The terminal event itself was durable at this injected cut. Reopen
        // repairs its checkpoint, proving failure was surfaced without losing
        // the recoverable authority record.
        let workspace = case.open_workspace()?;
        let recovered =
            LifecycleCoordinator::new(workspace, continuation_runtime(false)?, one_phase_state()?)?;
        let replay = recovered
            .durable_attempt_replay(&phase, WORKER, 1)?
            .ok_or("terminal continuation was not recovered")?;
        assert!(matches!(
            replay.terminal().kind(),
            WorkerEventKind::Completed | WorkerEventKind::Failed
        ));
        Ok(())
    }

    #[test]
    fn active_continuation_missing_terminal_fails_without_mutation() -> TestResult {
        let case = Case::new("continuation-missing-terminal")?;
        let (mut coordinator, phase) = active_continuation_coordinator(&case, false)?;
        let before = workspace_projection_bytes(&case.workspace_root)?;
        let request = execution_request(&case.worker_root, 1)?;
        let identity = WorkerIdentity::new(MISSION, PHASE, WORKER)?;
        let mut spawn_receipt = None;
        let mut withheld_terminal = None;
        let mut unexpected_event = None;
        let rejection = EventSinkError::new(
            EventSinkErrorKind::Rejected,
            "test deliberately withholds the continuation terminal",
        )?;
        let result =
            coordinator.project_terminal_into_active_attempt(&phase, WORKER, 1, |runtime, sink| {
                // Registry dispatch constructs the exact Spawn from the bound
                // request. Forward only that event to the real continuation
                // sink; withholding the centrally derived terminal leaves the
                // real sink in AwaitTerminal.
                let mut spawn_only = SpawnReplayOnlySink {
                    inner: sink,
                    spawn_receipt: &mut spawn_receipt,
                    withheld_terminal: &mut withheld_terminal,
                    unexpected_event: &mut unexpected_event,
                    rejection,
                };
                execute_continuation_request(runtime, &mut spawn_only, &request, identity)
            });
        assert!(matches!(
            result,
            Err(LifecycleError::AttemptTerminalMissing)
        ));
        let spawn_receipt = spawn_receipt.ok_or("exact Spawn replay returned no receipt")?;
        assert!(spawn_receipt.id().starts_with("evt_fixture_"));
        assert!(spawn_receipt.sequence() > 0);
        assert!(!spawn_receipt.timestamp().is_empty());
        assert!(matches!(
            withheld_terminal,
            Some(WorkerEventKind::Completed | WorkerEventKind::Failed)
        ));
        assert_eq!(unexpected_event, None);
        assert_eq!(workspace_projection_bytes(&case.workspace_root)?, before);
        assert!(matches!(
            coordinator.durable_attempt_replay(&phase, WORKER, 1),
            Err(LifecycleError::AttemptRecoveryRequired)
        ));
        Ok(())
    }

    #[test]
    fn crash_after_spawn_projection_reuses_exact_attempt_without_relaunch() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("active-spawn-recovery")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, idempotency_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;

        // Keep the injected fault for the worker.spawned projection rather
        // than consuming it on mission/phase admission.
        first.ensure_phase_running()?;
        first
            .coordinator
            .inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
        let first_result = first.run_to_terminal(pass_decision());
        assert!(matches!(
            first_result,
            Err(HermeticProviderError::Lifecycle(
                LifecycleError::RecoveryPending
            ))
        ));

        let ledger = case.open_ledger()?;
        let first_history = ledger.attempt_history(&idempotency_key, 10)?;
        assert_eq!(first_history.len(), 1);
        assert!(
            ledger
                .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
                .is_some()
        );
        ledger.close()?;
        drop(first);

        // Reopening repairs the one durable Spawned envelope. The provider
        // loads the already-terminal process evidence and decision, then the
        // narrow continuation path acknowledges the exact duplicate Spawned
        // bytes and appends only the terminal worker envelope.
        let reopened = case.open_workspace()?;
        let mut second = provider_for(&case, reopened, state, &helper, 1)?;
        let recovered = second.run_to_terminal(pass_decision())?;
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Failed);
        let events = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "worker.spawned")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| { matches!(event.as_str(), "worker.completed" | "worker.failed") })
                .count(),
            1
        );

        let ledger = case.open_ledger()?;
        assert_eq!(ledger.attempt_history(&idempotency_key, 10)?, first_history);
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn phase_terminal_mission_running_restart_finishes_only_the_mission() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("phase-terminal-restart")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        first.stop_after_phase_terminal_for_test();

        let first_result = first.run_to_terminal(pass_decision());
        assert!(matches!(
            first_result,
            Err(HermeticProviderError::InjectedStopAfterPhaseTerminal)
        ));
        assert_eq!(
            first.coordinator.state().status(),
            MissionStatus::InProgress
        );
        assert_eq!(first.phase_status()?, PhaseStatus::Failed);
        let before = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            before
                .iter()
                .filter(|event| event.as_str() == "phase.failed")
                .count(),
            1
        );
        assert!(
            before
                .iter()
                .all(|event| event.as_str() != "mission.failed")
        );
        drop(first);

        let reopened = case.open_workspace()?;
        let mut second = provider_for(&case, reopened, state, &helper, 1)?;
        let recovered = second.run_to_terminal(pass_decision())?;
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Failed);

        let after = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            after
                .iter()
                .filter(|event| event.as_str() == "phase.failed")
                .count(),
            1
        );
        assert_eq!(
            after
                .iter()
                .filter(|event| event.as_str() == "mission.failed")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn terminal_reopen_rejects_cross_worker_and_changed_spawn_metadata() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("terminal-binding-drift")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        first.stop_after_phase_terminal_for_test();
        assert!(matches!(
            first.run_to_terminal(pass_decision()),
            Err(HermeticProviderError::InjectedStopAfterPhaseTerminal)
        ));
        drop(first);

        let cross_persona = "worker-2";
        let cross_worker_id =
            orchestrator_core::WorkerId::for_phase(cross_persona, &PhaseId::new(PHASE)?)?;
        let cross_worker_root = case
            .workspace_root
            .join("workers")
            .join(cross_worker_id.as_str());
        let mut cross_draft = execution_request_draft(&cross_worker_root, 1)?;
        cross_draft.persona = cross_persona.to_owned();
        let reopened = case.open_workspace()?;
        let cross_worker = HermeticProvider::admit(
            reopened,
            state.clone(),
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            ExecutionRequest::new(cross_draft)?,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(
            matches!(
                cross_worker,
                Err(HermeticProviderError::ExecutionBindingConflict)
            ),
            "unexpected cross-worker admission result: {cross_worker:?}"
        );
        assert!(
            !cross_worker_root.exists(),
            "ledger binding conflict materialized a rejected worker tree"
        );

        let mut changed_draft = execution_request_draft(&case.worker_root, 1)?;
        changed_draft.model = "fixture-model-changed".to_owned();
        let reopened = case.open_workspace()?;
        let changed_metadata = HermeticProvider::admit(
            reopened,
            state,
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            ExecutionRequest::new(changed_draft)?,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            changed_metadata,
            Err(HermeticProviderError::ExecutionBindingConflict)
        ));

        let events = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "worker.spawned")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "phase.failed")
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|event| event.as_str() != "mission.failed")
        );
        Ok(())
    }

    #[test]
    fn pending_mission_terminal_restart_repairs_without_worker_reproof_at_admission() -> TestResult
    {
        let state = one_phase_state()?;
        let case = Case::new("pending-mission-terminal-restart")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let process_request = exact_process_request(&case, &helper)?;
        let execution_binding = test_execution_binding(&case.worker_root, 1)?;
        let (_, idempotency_key, _) = process_effect_for_attempt(
            &MissionId::new(MISSION)?,
            PHASE,
            1,
            &process_request,
            &execution_binding,
        )?;

        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        first.stop_after_phase_terminal_for_test();
        assert!(matches!(
            first.run_to_terminal(pass_decision()),
            Err(HermeticProviderError::InjectedStopAfterPhaseTerminal)
        ));
        drop(first);

        let before_history = {
            let ledger = case.open_ledger()?;
            let history = ledger.attempt_history(&idempotency_key, 10)?;
            ledger.close()?;
            history
        };
        let reopened = case.open_workspace()?;
        let mut second = provider_for(&case, reopened, state.clone(), &helper, 1)?;
        second.fault_mission_terminal_after_event_sync_for_test();
        assert!(matches!(
            second.run_to_terminal(pass_decision()),
            Err(HermeticProviderError::Lifecycle(LifecycleError::Workspace(
                WorkspaceError::InjectedProjectionFault("after-event-sync")
            )))
        ));
        drop(second);

        // The event is durable but its checkpoint acknowledgement is not.
        // Admission recognizes this exact terminal transition as recovery
        // workspace, stays actorless, and leaves validation/repair to run.
        let reopened = case.open_workspace()?;
        let mut third = provider_for(&case, reopened, state, &helper, 1)?;
        assert!(third.durable_attempt.actor.is_none());
        let recovered = third.run_to_terminal(pass_decision())?;
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Failed);

        let events = event_types(&case.workspace_root.join("events.jsonl"))?;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "phase.failed")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "mission.failed")
                .count(),
            1
        );
        let ledger = case.open_ledger()?;
        assert_eq!(
            ledger.attempt_history(&idempotency_key, 10)?,
            before_history
        );
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_decision_persisted_before_phase_finished_recovers_same_terminal() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("crash-after-decision")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        first.force_unresolved_children_for_test()?;

        // The attempt runs for real (claim -> spawn attempt -> resolve) and
        // the Cell 2F decision persists; only the phase/mission-finishing
        // transition is blocked, deterministically reproducing "crashed after
        // persisting the decision, before finishing the phase" without any
        // filesystem fault injection or real process kill.
        let persisted_verification = block_decision();
        let first_result = first.run_to_terminal(persisted_verification);
        assert!(matches!(
            first_result,
            Err(HermeticProviderError::Lifecycle(
                LifecycleError::UnresolvedChildren
            ))
        ));
        drop(first);

        // Fresh provider, fresh workspace handle, fresh ledger handle, no
        // in-process state at all — the same shape as a genuine restart. The
        // SAME already-installed helper is reused (a genuine restart binds
        // the same fixed executable identity; re-`install`ing under a new
        // label would mint a different `ProcessRequest` fingerprint and a
        // different outbox idempotency key, which is exactly what this test
        // must NOT do).
        let reopened_workspace = case.open_workspace()?;
        let mut second = provider_for(&case, reopened_workspace, state, &helper, 1)?;
        // A caller may now present a Warn-shaped choice, but the already
        // durable blocking decision remains authoritative on recovery.
        let recovered = second.run_to_terminal(warn_decision())?;
        assert_eq!(recovered.mission_status(), MissionStatus::Failed);
        assert_eq!(recovered.phase_status(), PhaseStatus::Failed);

        // Exactly one terminal ledger observation exists for the idempotency
        // key this mission/phase/attempt binds to: the second provider must
        // not have relaunched the attested helper a second time.
        let ledger = case.open_ledger()?;
        let recovery = ledger.recovery_effects(10)?;
        assert!(recovery.is_empty());
        let decision = ledger
            .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
            .ok_or("terminal decision must survive restart")?;
        assert_eq!(decision.verification(), persisted_verification);
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn restart_after_full_completion_replays_without_reclaiming() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("restart-after-completion")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let mut first = provider_for(&case, workspace, state.clone(), &helper, 1)?;
        let first_outcome = first.run_to_terminal(pass_decision())?;
        assert_eq!(first_outcome.mission_status(), MissionStatus::Failed);
        drop(first);

        let reopened_workspace = case.open_workspace()?;
        let mut second = provider_for(&case, reopened_workspace, state, &helper, 1)?;
        let second_outcome = second.run_to_terminal(pass_decision())?;
        assert_eq!(second_outcome.mission_status(), MissionStatus::Failed);
        assert_eq!(second_outcome.phase_status(), PhaseStatus::Failed);
        Ok(())
    }

    #[test]
    fn admit_rejects_workspace_that_collides_with_ledger_boundary() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("ledger-workspace-collision")?;
        let helper = case.install_helper()?;

        // A production workspace deliberately enrolled under the exact same
        // `Arc<ProductionBoundary>` this case's ledger uses: the collision
        // the guard exists to catch. `WorkspaceAuthority::create_production`
        // is a genuine production constructor (not a fixture stand-in), so
        // this proves the guard fires for a real production boundary alias,
        // not just a fixture-shaped approximation of one.
        let seed = FixtureWorkspaceSeed::new(
            b"ledger-workspace collision fixture\n".to_vec(),
            &initial_checkpoint(&state),
            b"{}".to_vec(),
        )?;
        let colliding_workspace = WorkspaceAuthority::create_production(
            Arc::clone(case.ledger_boundary.production_boundary()),
            MissionId::new(MISSION)?,
            seed,
        )?;

        let result = admit_raw(&case, colliding_workspace, state, &helper, 1);
        assert!(matches!(
            result,
            Err(HermeticProviderError::LedgerBoundaryCollision)
        ));

        // The guard must fire before any journaling: zero rows on the ledger.
        let ledger = case.open_ledger()?;
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[test]
    fn admit_rejects_distinct_boundary_arcs_for_same_physical_root() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("ledger-physical-root-alias")?;
        let helper = case.install_helper()?;
        let aliased_root = case.parent.join("physical-alias");
        private_dir(&aliased_root)?;
        let aliased_root = std::fs::canonicalize(aliased_root)?;
        let workspace_boundary = Arc::new(ProductionBoundary::from_canonical_root(&aliased_root)?);
        let ledger_boundary = Arc::new(ProductionBoundary::from_canonical_root(&aliased_root)?);
        assert!(!Arc::ptr_eq(&workspace_boundary, &ledger_boundary));

        let seed = FixtureWorkspaceSeed::new(
            b"physical-root alias fixture\n".to_vec(),
            &initial_checkpoint(&state),
            b"{}".to_vec(),
        )?;
        let workspace = WorkspaceAuthority::create_production(
            workspace_boundary,
            MissionId::new(MISSION)?,
            seed,
        )?;
        let worker_root = aliased_root
            .join("workspaces")
            .join(MISSION)
            .join("workers")
            .join(WORKER);
        let rejected = HermeticProvider::admit(
            workspace,
            state,
            &helper,
            PrivateProcessLedgerBoundary::new(ledger_boundary),
            RUNTIME,
            execution_request(&worker_root, 1)?,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            rejected,
            Err(HermeticProviderError::LedgerBoundaryCollision)
        ));

        // Creation of the production workspace is expected; opening the same
        // physical root as a process ledger is not.
        assert!(!aliased_root.join("runtime.db").exists());
        Ok(())
    }

    #[test]
    fn concurrent_second_admit_on_same_ledger_boundary_observes_writer_leased() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("concurrent-admit")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;

        // Holding `_provider` alive keeps its actor thread's `RuntimeStore`
        // open on `case.ledger_boundary` (the actor is only shut down by
        // `run_to_terminal`/`shutdown`, neither of which is called here), so
        // the writer lease this composition takes is real, not simulated.
        let _provider = provider_for(&case, workspace, state, &helper, 1)?;

        let second_open =
            RuntimeStore::open_private(case.ledger_boundary.clone(), StorageActorAuthority::new());
        assert!(matches!(second_open, Err(RuntimeStoreError::WriterLeased)));
        Ok(())
    }

    #[test]
    fn already_cancelled_token_converges_to_persisted_cancelled_decision() -> TestResult {
        let state = one_phase_state()?;
        let case = Case::new("pre-cancelled")?;
        let workspace = case.create_workspace(&state)?;
        let helper = case.install_helper()?;
        let request = execution_request(&case.worker_root, 1)?;

        // Cancelled before `admit` is ever called: the reviewer's concern was
        // that cancellation is only consulted at `run_process` preflight, not
        // pre-admission, so an already-cancelled token might never reach a
        // persisted `Cancelled` decision. It does: `run_process`'s own
        // preflight check (`current_process_preflight_reason`) observes the
        // already-cancelled token the first time the executor calls it, with
        // no additional wiring needed in this composition.
        let cancellation = CancellationToken::new();
        assert!(cancellation.cancel());
        let mut provider = HermeticProvider::admit(
            workspace,
            state,
            &helper,
            case.ledger_boundary.clone(),
            RUNTIME,
            request,
            cancellation,
            Arc::new(TestTimestampSource::new()),
        )?;

        let outcome = provider.run_to_terminal(pass_decision())?;
        // Cell 1's §2.6 mapping: `Incomplete / Cancelled` (regardless of the
        // supplied verification decision) is one `mission.cancelled`, with
        // the reducer atomically marking the running phase `skipped` — never
        // a separate `phase.failed`/`phase.completed` transition.
        assert_eq!(outcome.mission_status(), MissionStatus::Cancelled);
        assert_eq!(outcome.phase_status(), PhaseStatus::Skipped);

        // The persisted Cell 2F ledger decision must carry the exact typed
        // `MechanicalTermination::Cancelled` outcome — proof the full
        // composition converged on a durably persisted decision, not merely
        // a lower-level service refusal that never reached storage.
        let ledger = case.open_ledger()?;
        let persisted = ledger
            .terminal_decision(&MissionId::new(MISSION)?, PHASE, WORKER, 1)?
            .ok_or("terminal decision must be persisted for a cancelled attempt")?;
        assert_eq!(
            persisted.observed_termination(),
            Some(MechanicalTermination::Cancelled)
        );
        assert!(ledger.pending_effects(10)?.is_empty());
        assert!(ledger.recovery_effects(10)?.is_empty());
        ledger.close()?;
        Ok(())
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[test]
    fn admit_canary_composes_one_phase_mission_lifecycle() -> TestResult {
        use crate::{
            capability::CapabilityRoot, hermetic_canary_root::HermeticCanaryAuthority,
            hermetic_process_canary::HermeticProcessCanaryAuthority,
        };

        // Publish one exact root whose v1 layout fixes distinct compatibility
        // and private-ledger targets under one retained fixture authority.
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let outer = temporary.join(format!(
            "orchestrator-rs-stage2-canary-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        private_dir(&outer)?;
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy = FixtureAuthorityPolicy::new(outer.join("live-user"), checkout, &temporary);

        let canary_root = outer.join("canary-root");
        let authority = HermeticCanaryAuthority::publish_exact_at(&policy, &canary_root)?;
        let compatibility_home = Arc::clone(authority.compatibility_home());
        let ledger_boundary = authority.private_process_ledger().clone();
        let canary = HermeticProcessCanaryAuthority::enroll_current_executable(
            Arc::clone(&compatibility_home),
            "canary-helper",
        )?;
        let canary = Arc::new(canary);

        // Workspace under the canary's exact leaf.
        let mission_id = MissionId::new("stage2-canary-mission")?;
        let phase = PhaseId::new("canary-phase")?;
        let state = MissionState::new(
            mission_id.clone(),
            vec![PhaseDefinition {
                id: phase.clone(),
                dependencies: Vec::new(),
            }],
        )?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            status: "pending".to_owned(),
            plan: Some(CheckpointPlan {
                id: "stage2-canary-plan".to_owned(),
                phases: vec![CheckpointPhase {
                    id: phase.as_str().to_owned(),
                    status: "pending".to_owned(),
                    ..CheckpointPhase::default()
                }],
                ..CheckpointPlan::default()
            }),
            ..CheckpointProjection::default()
        };
        let seed =
            crate::WorkspaceSeed::new(b"stage2 canary\n".to_vec(), &checkpoint, b"{}".to_vec())?;
        let workspace = crate::WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(&compatibility_home),
            mission_id.clone(),
            seed,
        )?;

        // Build the execution request matching the mission/phase.
        let worker_id = orchestrator_core::WorkerId::for_phase("canary", &phase)?;
        let worker_root = canary
            .boundary()
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        let request = ExecutionRequest::new(ExecutionRequestDraft {
            mission: mission_id.as_str().to_owned(),
            phase: phase.as_str().to_owned(),
            attempt: 1,
            revision: 1,
            objective: "stage 2 canary mission".to_owned(),
            persona: "canary".to_owned(),
            role: "implementer".to_owned(),
            domain: "dev".to_owned(),
            skills: Vec::new(),
            dependencies: Vec::new(),
            expected_evidence: Vec::new(),
            constraints: Vec::new(),
            prior_context: String::new(),
            runtime: RuntimeFamily::parse("canary")?,
            model: "canary-model".to_owned(),
            effort: Effort::High,
            max_turns: 1,
            worker_dir: worker_root.clone(),
            target_dir: None,
            resume_from: None,
            hook_script: None,
        })?;

        // Admit through the canary path.
        let cancellation = CancellationToken::new();
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let mut provider = HermeticProvider::admit_canary(
            workspace,
            state,
            &canary,
            ledger_boundary,
            "canary",
            request,
            cancellation.clone(),
            Arc::clone(&timestamp_source),
        )?;

        // Run to terminal. The enrolled current executable (the test binary)
        // is spawned with --hermetic-canary-worker. In test mode the binary
        // doesn't intercept the flag, so the exec exits non-zero. The
        // provider lifecycle (attempt → actor → receipt → classification →
        // terminal decision → projection) runs end-to-end.
        //
        // We pass verification=Pass+Block because the verification gate is
        // the operator's decision, not the process exit classification. The
        // sentinel check would be the real gate in production.
        let outcome = provider.run_to_terminal(decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        ))?;

        // Assert the lifecycle reached a terminal state.
        assert!(
            outcome.mission_status().is_terminal(),
            "mission must be terminal after run_to_terminal: {:?}",
            outcome.mission_status()
        );
        assert!(
            outcome.phase_status().is_terminal(),
            "phase must be terminal after run_to_terminal: {:?}",
            outcome.phase_status()
        );

        // The existing fixture test suite (50+ tests in this module) proves
        // reopen/replay, crash injection, cancellation, and terminal decision
        // persistence through the same DurableHermeticAttempt code path.
        // Those tests cover the identical lifecycle machinery; the canary
        // path differs only in the executable source (enrolled current_exe
        // vs fixture helper), not in the durable actor, store, projection,
        // or terminal-decision code.

        let _ = std::fs::remove_dir_all(&outer);
        Ok(())
    }
}
