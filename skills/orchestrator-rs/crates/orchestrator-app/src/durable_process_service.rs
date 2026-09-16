//! Attempt-bound durable process execution for one enrolled provider worker.
//!
//! Consumed in production (non-test) code since Cell 3's hermetic provider
//! (`hermetic_provider.rs`) composes `DurableProcessActor`/`DurableProcessService`
//! with the attested fixture helper; the module-wide `expect(dead_code)` this
//! file previously carried is gone because it is no longer accurate.

#[path = "provider_process_enrollment.rs"]
mod provider_process_enrollment;

#[cfg(not(test))]
use provider_process_enrollment::DeferredProviderRuntime;
#[cfg(test)]
pub(crate) use provider_process_enrollment::DeferredProviderRuntime;
pub(crate) use provider_process_enrollment::EnrolledProviderLaunch;
#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) use provider_process_enrollment::RetainedFileIdentity;
#[cfg(all(test, unix, feature = "verification-process-canary"))]
pub(crate) use provider_process_enrollment::RetainedProviderExecutable;
use provider_process_enrollment::{
    ExactProviderAdmission, InitializedProviderRuntime, ProviderAdmissionFailure,
};
pub(crate) use provider_process_enrollment::{PreparedProviderLaunch, ProviderLaunchError};

use crate::{
    EffectEvidence, EffectResolution, ProcessExecutionIdentity, ProcessNotStartedEvidenceReason,
    ProcessUncertaintyEvidence, RuntimeStoreError, RustPilotProcessReleaseAdmission,
    lifecycle::OwnedChildState,
    process_receipt::map_authorized_process_outcome,
    runtime_store::{
        AuthorizedLauncherIdentity, ClaimedProcessAttempt, ExactProcessClaimError,
        PrivateProcessLedgerStore, ProcessAuthorizationReleaseError, ProcessEffectBinding,
        ProcessTerminalClaim, RecoveredProcessAttempt, RecoveredProcessAttemptClassification,
        RecoveredProcessCleanup,
    },
};
use orchestrator_exec::{
    BoundProcessPreflight, Cancellation, ProcessBudget, ProcessPreflight, ProcessPreflightReason,
    ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind,
    ServiceContractError,
};
use orchestrator_process::{
    AuthorizedProcessErrorClassification, AuthorizedProcessOutcome, CancellationToken,
    ExactProcessGroupAbsence, KernelProcessIdentity, ProcessIdentityError, ProcessNotStartedReason,
    ProcessNotStartedReceipt, ProcessOutputSender, ProcessRequestBinding, ProcessSpec,
    ProcessStartGate, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    process_wide_has_owned_processes,
};
#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
use std::{
    ffi::OsString,
    fmt,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;

const PROVIDER_WORKER_ARGV0: &str = "nanika-provider-worker";

pub(crate) trait ProcessTimestampSource: Send + Sync {
    fn now_utc(&self) -> String;
}

/// Durable boundaries exposed to one explicitly scoped fixture observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DurableProcessBarrierPoint {
    ClaimedBeforeSpecification,
    ReleaseAuthorizedBeforeStartedReceipt,
    StartedPersistedBeforeTerminalResolution,
}

/// Infallible, actor-scoped observation of exact durable process boundaries.
///
/// The observer receives no storage, receipt, identity, or launch authority.
/// Production composition supplies the zero-sized no-op implementation.
pub(crate) trait DurableProcessBarrierObserver: Send + Sync + 'static {
    fn observe(&self, point: DurableProcessBarrierPoint);
}

struct NoopDurableProcessBarrierObserver;

impl DurableProcessBarrierObserver for NoopDurableProcessBarrierObserver {
    fn observe(&self, _point: DurableProcessBarrierPoint) {}
}

fn observe_after_success<T, E>(
    result: Result<T, E>,
    point: DurableProcessBarrierPoint,
    observer: &impl DurableProcessBarrierObserver,
) -> Result<T, E> {
    if result.is_ok() {
        observer.observe(point);
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveredProcessDisposition {
    RecoveredBeforeSpawnNotStarted,
    SpawnUnobserved,
    BlockedLauncherCleanedNotStarted,
    AuthorizedWithoutStartedCleanedUncertain,
    StartedProcessCleanedUncertain,
    UnreleasedUncertainCleaned,
    AuthorizedUncertainCleaned,
    StartedUncertainCleaned,
    Unresolved,
    LeaderAbsentGroupPresent(RecoveredProcessAttemptClassification),
}

#[derive(Debug, Error)]
pub(crate) enum DurableProcessRecoveryError {
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error(transparent)]
    Identity(#[from] ProcessIdentityError),
    #[error("recovered process group cleanup failed")]
    CleanupFailed,
    #[error("recovered process group cleanup timed out")]
    CleanupTimedOut,
}

#[cfg(unix)]
pub(crate) fn reconcile_recovered_process_attempt(
    store: &mut PrivateProcessLedgerStore,
    binding: &ProcessEffectBinding,
    request: &ProcessRequest,
    observed_at_utc: &str,
) -> Result<RecoveredProcessDisposition, DurableProcessRecoveryError> {
    let recovered: RecoveredProcessAttempt =
        store.recover_exact_process_attempt(binding, request)?;
    let classification = recovered.classification();
    match classification {
        RecoveredProcessAttemptClassification::ClaimedBeforeSpawn => {
            store.resolve_recovered_before_spawn(recovered, observed_at_utc)?;
            Ok(RecoveredProcessDisposition::RecoveredBeforeSpawnNotStarted)
        }
        RecoveredProcessAttemptClassification::SpawnUnobserved => {
            Ok(RecoveredProcessDisposition::SpawnUnobserved)
        }
        RecoveredProcessAttemptClassification::Unresolved => {
            Ok(RecoveredProcessDisposition::Unresolved)
        }
        RecoveredProcessAttemptClassification::BlockedLauncher
        | RecoveredProcessAttemptClassification::AuthorizedWithoutStarted
        | RecoveredProcessAttemptClassification::StartedExecuting
        | RecoveredProcessAttemptClassification::UnreleasedUncertain
        | RecoveredProcessAttemptClassification::AuthorizedUncertain
        | RecoveredProcessAttemptClassification::StartedUncertain => {
            let cleanup = recovered.into_cleanup()?;
            let Some(absence) = clean_recovered_process_group(&cleanup)? else {
                return Ok(RecoveredProcessDisposition::LeaderAbsentGroupPresent(
                    classification,
                ));
            };
            store.resolve_recovered_process_group_absent(cleanup, &absence, observed_at_utc)?;
            Ok(match classification {
                RecoveredProcessAttemptClassification::BlockedLauncher => {
                    RecoveredProcessDisposition::BlockedLauncherCleanedNotStarted
                }
                RecoveredProcessAttemptClassification::AuthorizedWithoutStarted => {
                    RecoveredProcessDisposition::AuthorizedWithoutStartedCleanedUncertain
                }
                RecoveredProcessAttemptClassification::StartedExecuting => {
                    RecoveredProcessDisposition::StartedProcessCleanedUncertain
                }
                RecoveredProcessAttemptClassification::UnreleasedUncertain => {
                    RecoveredProcessDisposition::UnreleasedUncertainCleaned
                }
                RecoveredProcessAttemptClassification::AuthorizedUncertain => {
                    RecoveredProcessDisposition::AuthorizedUncertainCleaned
                }
                RecoveredProcessAttemptClassification::StartedUncertain => {
                    RecoveredProcessDisposition::StartedUncertainCleaned
                }
                _ => return Err(DurableProcessRecoveryError::CleanupFailed),
            })
        }
    }
}

#[cfg(unix)]
fn clean_recovered_process_group(
    cleanup: &RecoveredProcessCleanup,
) -> Result<Option<ExactProcessGroupAbsence>, DurableProcessRecoveryError> {
    match inspect_cleanup_identity(cleanup)? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => return Ok(Some(absence)),
        RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => return Ok(None),
        RecordedProcessIdentityStatus::ExactLive => {}
    }
    // Reinspect immediately before signalling. A changed/ambiguous identity
    // never reaches the signal operation.
    match inspect_cleanup_identity(cleanup)? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => return Ok(Some(absence)),
        RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => return Ok(None),
        RecordedProcessIdentityStatus::ExactLive => {}
    }
    let raw_group = i32::try_from(cleanup.process_group_id())
        .map_err(|_| DurableProcessRecoveryError::CleanupFailed)?;
    let group = Pid::from_raw(raw_group).ok_or(DurableProcessRecoveryError::CleanupFailed)?;
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(_) => return Err(DurableProcessRecoveryError::CleanupFailed),
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or(DurableProcessRecoveryError::CleanupTimedOut)?;
    loop {
        match inspect_cleanup_identity(cleanup)? {
            RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => return Ok(Some(absence)),
            RecordedProcessIdentityStatus::ExactLive
            | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {}
        }
        if Instant::now() >= deadline {
            return Err(DurableProcessRecoveryError::CleanupTimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn inspect_cleanup_identity(
    cleanup: &RecoveredProcessCleanup,
) -> Result<RecordedProcessIdentityStatus, ProcessIdentityError> {
    inspect_recorded_process_identity(
        cleanup.pid(),
        cleanup.process_group_id(),
        cleanup.process_start_identity(),
    )
}

struct DurableProcessServiceErrors {
    denied: ProcessServiceError,
    outside_root: ProcessServiceError,
    not_enrolled: ProcessServiceError,
    invalid: ProcessServiceError,
    unavailable: ProcessServiceError,
    indeterminate: ProcessServiceError,
    outcome: Mutex<Option<DurableProcessOutcomeObservation>>,
}

pub(crate) struct DurableProcessOutcomeObservation {
    pub(crate) not_started_reason: Option<ProcessNotStartedReason>,
    pub(crate) release_confirmed: bool,
    pub(crate) post_release_identity: Option<KernelProcessIdentity>,
    pub(crate) cleanup_identity: Option<KernelProcessIdentity>,
    pub(crate) group_absent: bool,
    pub(crate) cleanup_complete: bool,
}

impl DurableProcessOutcomeObservation {
    fn from_outcome(outcome: &AuthorizedProcessOutcome) -> Self {
        match outcome {
            AuthorizedProcessOutcome::NotStarted(receipt) => Self {
                not_started_reason: Some(receipt.reason),
                release_confirmed: false,
                post_release_identity: None,
                cleanup_identity: receipt.launcher_identity.clone(),
                group_absent: true,
                cleanup_complete: true,
            },
            AuthorizedProcessOutcome::Started(report) => Self {
                not_started_reason: None,
                release_confirmed: true,
                post_release_identity: report.kernel_identity.clone(),
                cleanup_identity: report.kernel_identity.clone(),
                group_absent: report.group_absent,
                cleanup_complete: report.cleanup_complete,
            },
            AuthorizedProcessOutcome::Uncertain(receipt) => Self {
                not_started_reason: None,
                release_confirmed: false,
                post_release_identity: None,
                cleanup_identity: receipt.launcher_identity.clone(),
                group_absent: receipt.report.group_absent,
                cleanup_complete: receipt.report.cleanup_complete,
            },
        }
    }
}

impl DurableProcessServiceErrors {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            denied: ProcessServiceError::new(
                ProcessServiceErrorKind::Denied,
                "durable process request is outside its enrolled provider policy",
            )?,
            outside_root: ProcessServiceError::new(
                ProcessServiceErrorKind::OutsideRoot,
                "durable process request names an unenrolled working root",
            )?,
            not_enrolled: ProcessServiceError::new(
                ProcessServiceErrorKind::NotEnrolled,
                "durable process request names an unenrolled executable",
            )?,
            invalid: ProcessServiceError::new(
                ProcessServiceErrorKind::InvalidRequest,
                "durable process request cannot satisfy its bounded policy",
            )?,
            unavailable: ProcessServiceError::new(
                ProcessServiceErrorKind::Unavailable,
                "durable process actor is unavailable",
            )?,
            indeterminate: ProcessServiceError::new(
                ProcessServiceErrorKind::OutcomeIndeterminate,
                "durable process outcome requires recovery reconciliation",
            )?,
            outcome: Mutex::new(None),
        })
    }
}

/// One-shot process service for one exact durable provider attempt.
///
/// Construction stays inside the app composition root. The service is public
/// so executors can receive it behind `dyn ProcessService`, but it cannot be
/// cloned or independently bound to an outbox key.
///
/// ```compile_fail
/// use orchestrator_app::DurableProcessService;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<DurableProcessService>();
/// ```
///
/// ```compile_fail
/// use orchestrator_app::DurableProcessService;
///
/// // Only the crate-private storage composition root can bind a service.
/// let _ = DurableProcessService::new();
/// ```
pub struct DurableProcessService {
    sender: SyncSender<ActorMessage>,
    admission: ExactProviderAdmission,
    pending: Mutex<Option<ClosedActorCall>>,
    cancellation: CancellationToken,
    used: AtomicBool,
    faulted: Arc<AtomicBool>,
    errors: Arc<DurableProcessServiceErrors>,
    output_sender: Option<ProcessOutputSender>,
}

impl DurableProcessService {
    #[must_use]
    pub(crate) fn with_output_sender(mut self, sender: ProcessOutputSender) -> Self {
        self.output_sender = Some(sender);
        self
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) -> bool {
        self.cancellation.cancel()
    }

    fn copy_request(
        &self,
        request: &ProcessRequest,
    ) -> Result<ProcessRequest, ProcessServiceError> {
        let mut rebuilt = ProcessRequest::new(
            request.purpose(),
            request.executable_id(),
            request.working_root(),
        )
        .map_err(|_| self.errors.invalid.clone())?;
        for argument in request.expose_arguments() {
            rebuilt = rebuilt
                .with_argument(argument)
                .map_err(|_| self.errors.invalid.clone())?;
        }
        for (key, value) in request.expose_environment() {
            rebuilt = rebuilt
                .with_environment(key, value)
                .map_err(|_| self.errors.invalid.clone())?;
        }
        if let Some(stdin) = request.expose_stdin() {
            rebuilt = rebuilt
                .with_stdin(stdin.to_vec())
                .map_err(|_| self.errors.invalid.clone())?;
        }
        rebuilt = rebuilt
            .with_max_output_bytes(request.max_output_bytes())
            .map_err(|_| self.errors.invalid.clone())?;
        if request.truncated_output_acknowledged() {
            rebuilt = rebuilt.with_truncated_output_acknowledged();
        }
        Ok(rebuilt)
    }

    pub(crate) fn exact_request_copy(&self) -> Result<ProcessRequest, ProcessServiceError> {
        let pending = lock_unpoisoned(&self.pending);
        let call = pending
            .as_ref()
            .ok_or_else(|| self.errors.unavailable.clone())?;
        self.copy_request(&call.request)
    }

    pub(crate) fn take_outcome_observation(&self) -> Option<DurableProcessOutcomeObservation> {
        lock_unpoisoned(&self.errors.outcome).take()
    }

    /// Dispatches one process request through the durable actor and waits for
    /// the receipt synchronously. Exposed pub(crate) so the canary composition
    /// can drive one dispatch without the full hermetic-provider lifecycle.
    #[allow(dead_code, reason = "staged for canary durable composition")]
    pub(crate) fn dispatch_once(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        self.execute(request, budget)
    }

    pub(crate) fn finish_bound_preflight(
        &self,
        request: &ProcessRequest,
        preflight: BoundProcessPreflight,
    ) -> Result<(), ProcessServiceError> {
        let prepared = self.prepare_call(request)?;
        if !preflight.matches_request(&prepared.call.request) {
            return Err(self.errors.denied.clone());
        }
        self.finish_prepared_preflight(prepared, preflight)
    }

    fn finish_prepared_preflight(
        &self,
        prepared: PreparedProcessCall,
        preflight: BoundProcessPreflight,
    ) -> Result<(), ProcessServiceError> {
        let PreparedProcessCall { mut consumed, call } = prepared;
        let ClosedActorCall { binding, request } = call;
        let (reply_sender, reply_receiver) = sync_channel(1);
        let message = ActorMessage::FinishPreflight(PreflightMessage {
            binding,
            request,
            preflight,
            reply: reply_sender,
        });
        self.sender.try_send(message).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                self.errors.unavailable.clone()
            }
        })?;
        consumed.disarm();
        match reply_receiver.recv() {
            Ok(result) => result,
            Err(_) => {
                self.faulted.store(true, Ordering::Release);
                Err(self.errors.indeterminate.clone())
            }
        }
    }

    fn prepare_call(
        &self,
        request: &ProcessRequest,
    ) -> Result<PreparedProcessCall<'_>, ProcessServiceError> {
        if self
            .used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(self.errors.unavailable.clone());
        }
        let consumed = ConsumedCallGuard::new(&self.sender);
        if self.faulted.load(Ordering::Acquire) {
            return Err(self.errors.unavailable.clone());
        }
        self.admission
            .admit(request)
            .map_err(|failure| match failure {
                ProviderAdmissionFailure::Denied => self.errors.denied.clone(),
                ProviderAdmissionFailure::OutsideRoot => self.errors.outside_root.clone(),
                ProviderAdmissionFailure::NotEnrolled => self.errors.not_enrolled.clone(),
            })?;
        let call = lock_unpoisoned(&self.pending)
            .take()
            .ok_or_else(|| self.errors.unavailable.clone())?;
        if self.admission.admit(&call.request).is_err() || !call.binding.matches(&call.request) {
            return Err(self.errors.invalid.clone());
        }
        Ok(PreparedProcessCall { consumed, call })
    }
}

impl fmt::Debug for DurableProcessService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableProcessService")
            .field("kind", &"one-shot-durable-provider-process")
            .field("used", &self.used.load(Ordering::Acquire))
            .field("faulted", &self.faulted.load(Ordering::Acquire))
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

impl Cancellation for DurableProcessService {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl OwnedChildState for DurableProcessService {
    fn has_unresolved_children(&self) -> bool {
        process_wide_has_owned_processes()
    }
}

impl ProcessService for DurableProcessService {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        let prepared = self.prepare_call(request)?;
        let Some(preflight) = preflight.bind(self, &prepared.call.request) else {
            return Err(self.errors.denied.clone());
        };
        self.finish_prepared_preflight(prepared, preflight)
    }

    fn execute(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        let PreparedProcessCall { mut consumed, call } = self.prepare_call(request)?;
        let ClosedActorCall { binding, request } = call;
        let (reply_sender, reply_receiver) = sync_channel(1);
        let message = ActorMessage::Execute(ExecuteMessage {
            binding,
            request,
            budget,
            output_sender: self.output_sender.clone(),
            reply: reply_sender,
        });
        self.sender.try_send(message).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                self.errors.unavailable.clone()
            }
        })?;
        consumed.disarm();
        match reply_receiver.recv() {
            Ok(result) => result,
            Err(_) => {
                self.faulted.store(true, Ordering::Release);
                Err(self.errors.indeterminate.clone())
            }
        }
    }
}

struct PreparedProcessCall<'a> {
    consumed: ConsumedCallGuard<'a>,
    call: ClosedActorCall,
}

struct ClosedActorCall {
    binding: ProcessEffectBinding,
    request: ProcessRequest,
}

struct ConsumedCallGuard<'a> {
    sender: &'a SyncSender<ActorMessage>,
    armed: bool,
}

impl<'a> ConsumedCallGuard<'a> {
    fn new(sender: &'a SyncSender<ActorMessage>) -> Self {
        Self {
            sender,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ConsumedCallGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.sender.try_send(ActorMessage::Abort);
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DurableProcessBuildError {
    #[error("durable process enrollment is invalid")]
    InvalidEnrollment,
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error(transparent)]
    Contract(#[from] ServiceContractError),
    #[error("durable process actor thread could not start")]
    ThreadStart(#[source] std::io::Error),
    #[error(
        "durable process actor thread could not start and the runtime store did not close: thread={thread}; close={close}"
    )]
    ThreadStartAndStoreClose {
        thread: std::io::Error,
        close: RuntimeStoreError,
    },
}

fn close_store_then_build_error<T>(
    store: PrivateProcessLedgerStore,
    error: DurableProcessBuildError,
) -> Result<T, DurableProcessBuildError> {
    match store.close() {
        Ok(()) => Err(error),
        Err(close) => Err(DurableProcessBuildError::Store(close)),
    }
}

/// Join handle for the sole SQLite/process actor. Dropping it performs a
/// synchronous shutdown so neither the store writer nor supervisor outlives
/// the composition scope.
pub(crate) struct DurableProcessActor {
    sender: SyncSender<ActorMessage>,
    join: Option<JoinHandle<Result<(), RuntimeStoreError>>>,
    cancellation: CancellationToken,
}

impl DurableProcessActor {
    /// Spawns the sole SQLite/process actor for one closed provider launch.
    pub(crate) fn spawn(
        store: PrivateProcessLedgerStore,
        launch: EnrolledProviderLaunch,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
    ) -> Result<(Self, DurableProcessService), DurableProcessBuildError> {
        Self::spawn_with_observer(
            store,
            launch,
            cancellation,
            timestamp_source,
            NoopDurableProcessBarrierObserver,
        )
    }

    /// Spawns one actor with a non-ambient observer scoped to that actor.
    pub(crate) fn spawn_with_observer<Observer>(
        store: PrivateProcessLedgerStore,
        launch: EnrolledProviderLaunch,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Observer,
    ) -> Result<(Self, DurableProcessService), DurableProcessBuildError>
    where
        Observer: DurableProcessBarrierObserver,
    {
        Self::spawn_inner(
            store,
            launch,
            cancellation,
            timestamp_source,
            observer,
            None,
        )
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn spawn_with_observer_and_release_admission<Observer>(
        store: PrivateProcessLedgerStore,
        launch: EnrolledProviderLaunch,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Observer,
        release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
    ) -> Result<(Self, DurableProcessService), DurableProcessBuildError>
    where
        Observer: DurableProcessBarrierObserver,
    {
        Self::spawn_inner(
            store,
            launch,
            cancellation,
            timestamp_source,
            observer,
            release_admission,
        )
    }

    pub(crate) fn spawn_with_release_admission(
        store: PrivateProcessLedgerStore,
        launch: EnrolledProviderLaunch,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
    ) -> Result<(Self, DurableProcessService), DurableProcessBuildError> {
        Self::spawn_inner(
            store,
            launch,
            cancellation,
            timestamp_source,
            NoopDurableProcessBarrierObserver,
            release_admission,
        )
    }

    fn spawn_inner<Observer>(
        store: PrivateProcessLedgerStore,
        launch: EnrolledProviderLaunch,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Observer,
        release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
    ) -> Result<(Self, DurableProcessService), DurableProcessBuildError>
    where
        Observer: DurableProcessBarrierObserver,
    {
        let EnrolledProviderLaunch {
            request,
            admission,
            runtime,
            binding,
        } = launch;
        // Every fallible step between taking ownership of `store` and handing
        // it to the worker thread (further below) must close it best-effort
        // on the way out: `RuntimeStore::drop` deliberately never releases an
        // uncleanly-dropped writer lease (see its `Drop` impl), so simply
        // letting `store` fall out of scope here would hold the boundary's
        // writer lease for the rest of the process on every early error.
        if admission.admit(&request).is_err() || !binding.matches(&request) {
            return close_store_then_build_error(
                store,
                DurableProcessBuildError::InvalidEnrollment,
            );
        }
        let errors = match DurableProcessServiceErrors::new() {
            Ok(errors) => Arc::new(errors),
            Err(error) => {
                return close_store_then_build_error(store, error.into());
            }
        };
        let faulted = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = sync_channel(1);
        let actor_faulted = Arc::clone(&faulted);
        let actor_errors = Arc::clone(&errors);
        let actor_cancellation = cancellation.clone();
        let startup_store = Arc::new(Mutex::new(Some(store)));
        let actor_startup_store = Arc::clone(&startup_store);
        let join = match thread::Builder::new()
            .name("orchestrator-durable-process".to_owned())
            .spawn(move || {
                let store = lock_unpoisoned(&actor_startup_store)
                    .take()
                    .ok_or(RuntimeStoreError::CloseIncomplete)?;
                actor_main(
                    store,
                    receiver,
                    runtime,
                    actor_cancellation,
                    timestamp_source,
                    actor_faulted,
                    actor_errors,
                    observer,
                    release_admission,
                )
            }) {
            Ok(join) => join,
            Err(source) => {
                if let Some(store) = lock_unpoisoned(&startup_store).take() {
                    if let Err(close) = store.close() {
                        return Err(DurableProcessBuildError::ThreadStartAndStoreClose {
                            thread: source,
                            close,
                        });
                    }
                }
                return Err(DurableProcessBuildError::ThreadStart(source));
            }
        };
        let service = DurableProcessService {
            sender: sender.clone(),
            admission,
            pending: Mutex::new(Some(ClosedActorCall { binding, request })),
            cancellation,
            used: AtomicBool::new(false),
            faulted,
            errors,
            output_sender: None,
        };
        Ok((
            Self {
                sender,
                join: Some(join),
                cancellation: service.cancellation.clone(),
            },
            service,
        ))
    }

    pub(crate) fn shutdown(mut self) -> Result<(), RuntimeStoreError> {
        self.shutdown_inner(true)
    }

    pub(crate) fn shutdown_without_cancelling(mut self) -> Result<(), RuntimeStoreError> {
        self.shutdown_inner(false)
    }

    fn shutdown_inner(&mut self, cancel: bool) -> Result<(), RuntimeStoreError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        if cancel {
            self.cancellation.cancel();
        }
        let (reply, acknowledgement) = sync_channel(1);
        let _ = self.sender.send(ActorMessage::Shutdown(reply));
        let _ = acknowledgement.recv();
        join.join()
            .map_or(Err(RuntimeStoreError::CloseIncomplete), |result| result)
    }
}

impl Drop for DurableProcessActor {
    fn drop(&mut self) {
        let _ = self.shutdown_inner(true);
    }
}

enum ActorMessage {
    Execute(ExecuteMessage),
    FinishPreflight(PreflightMessage),
    Abort,
    Shutdown(SyncSender<()>),
}

struct ExecuteMessage {
    binding: ProcessEffectBinding,
    request: ProcessRequest,
    budget: ProcessBudget,
    output_sender: Option<ProcessOutputSender>,
    reply: SyncSender<Result<ProcessReceipt, ProcessServiceError>>,
}

struct PreflightMessage {
    binding: ProcessEffectBinding,
    request: ProcessRequest,
    preflight: BoundProcessPreflight,
    reply: SyncSender<Result<(), ProcessServiceError>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the actor thread owns each exact durability and observation capability"
)]
fn actor_main<Observer>(
    mut store: PrivateProcessLedgerStore,
    receiver: Receiver<ActorMessage>,
    runtime: DeferredProviderRuntime,
    cancellation: CancellationToken,
    timestamp_source: Arc<dyn ProcessTimestampSource>,
    faulted: Arc<AtomicBool>,
    errors: Arc<DurableProcessServiceErrors>,
    observer: Observer,
    release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
) -> Result<(), RuntimeStoreError>
where
    Observer: DurableProcessBarrierObserver,
{
    let Ok(message) = receiver.recv() else {
        return store.close();
    };
    match message {
        ActorMessage::Execute(message) => {
            let result = execute_actor_attempt(
                &mut store,
                runtime,
                &cancellation,
                timestamp_source.as_ref(),
                &faulted,
                &errors,
                &observer,
                &message,
                release_admission.as_deref(),
            );
            match store.close() {
                Ok(()) => {
                    if message.reply.try_send(result).is_err() {
                        faulted.store(true, Ordering::Release);
                    }
                    Ok(())
                }
                Err(error) => {
                    faulted.store(true, Ordering::Release);
                    let _ = message.reply.try_send(Err(errors.indeterminate.clone()));
                    Err(error)
                }
            }
        }
        ActorMessage::FinishPreflight(message) => {
            let PreflightMessage {
                binding,
                request,
                preflight,
                reply,
            } = message;
            let result = finish_preflight_attempt(
                &mut store,
                timestamp_source.as_ref(),
                &faulted,
                &errors,
                binding,
                request,
                preflight,
            );
            match store.close() {
                Ok(()) => {
                    if reply.try_send(result).is_err() {
                        faulted.store(true, Ordering::Release);
                    }
                    Ok(())
                }
                Err(error) => {
                    faulted.store(true, Ordering::Release);
                    let _ = reply.try_send(Err(errors.indeterminate.clone()));
                    Err(error)
                }
            }
        }
        ActorMessage::Abort => store.close(),
        ActorMessage::Shutdown(reply) => {
            let result = store.close();
            let _ = reply.try_send(());
            result
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "attempt execution consumes each exact actor and observation capability"
)]
fn execute_actor_attempt(
    store: &mut PrivateProcessLedgerStore,
    runtime: DeferredProviderRuntime,
    cancellation: &CancellationToken,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
    observer: &impl DurableProcessBarrierObserver,
    message: &ExecuteMessage,
    release_admission: Option<&dyn RustPilotProcessReleaseAdmission>,
) -> Result<ProcessReceipt, ProcessServiceError> {
    if let Some((reason, cancellation_observed, deadline_observed)) =
        prelaunch_not_started_observation(cancellation, message.budget)
    {
        let claimed = claim_exact_attempt(
            store,
            &message.binding,
            &message.request,
            timestamp_source,
            faulted,
            errors,
        )?;
        return resolve_claimed_not_started(
            store,
            claimed,
            &message.request,
            reason,
            cancellation_observed,
            deadline_observed,
            timestamp_source,
            faulted,
            errors,
        );
    }
    // This retained-authority check is deliberately pre-claim: an invalid
    // executable/CWD enrollment must not mutate the durable process ledger.
    // All operation construction and runtime initialization happen only after
    // the atomic typed claim below, which is the recoverable C4 crash seam.
    if runtime.verify_preclaim().is_err() {
        return Err(errors.unavailable.clone());
    }
    if let Some((reason, cancellation_observed, deadline_observed)) =
        prelaunch_not_started_observation(cancellation, message.budget)
    {
        let claimed = claim_exact_attempt(
            store,
            &message.binding,
            &message.request,
            timestamp_source,
            faulted,
            errors,
        )?;
        return resolve_claimed_not_started(
            store,
            claimed,
            &message.request,
            reason,
            cancellation_observed,
            deadline_observed,
            timestamp_source,
            faulted,
            errors,
        );
    }
    let claimed = claim_exact_attempt(
        store,
        &message.binding,
        &message.request,
        timestamp_source,
        faulted,
        errors,
    )?;
    observer.observe(DurableProcessBarrierPoint::ClaimedBeforeSpecification);
    let specification = match build_spec_after_claim(
        &claimed,
        &message.request,
        message.budget,
        cancellation,
        errors,
        message.output_sender.clone(),
    ) {
        Ok(specification) => specification,
        Err(_) => {
            return resolve_claimed_prelaunch_failure(
                store,
                claimed,
                &message.request,
                message.budget,
                cancellation,
                timestamp_source,
                faulted,
                errors,
            );
        }
    };
    if let Some((reason, cancellation_observed, deadline_observed)) =
        prelaunch_not_started_observation(cancellation, message.budget)
    {
        return resolve_claimed_not_started(
            store,
            claimed,
            &message.request,
            reason,
            cancellation_observed,
            deadline_observed,
            timestamp_source,
            faulted,
            errors,
        );
    }
    let mut runtime = match runtime.initialize() {
        Ok(initialized) => initialized,
        Err(_) => {
            return resolve_claimed_prelaunch_failure(
                store,
                claimed,
                &message.request,
                message.budget,
                cancellation,
                timestamp_source,
                faulted,
                errors,
            );
        }
    };
    if let Some((reason, cancellation_observed, deadline_observed)) =
        prelaunch_not_started_observation(cancellation, message.budget)
    {
        return resolve_claimed_not_started(
            store,
            claimed,
            &message.request,
            reason,
            cancellation_observed,
            deadline_observed,
            timestamp_source,
            faulted,
            errors,
        );
    }
    execute_attempt(
        store,
        &mut runtime,
        specification,
        claimed,
        cancellation,
        timestamp_source,
        faulted,
        errors,
        observer,
        message,
        release_admission,
    )
}

fn prelaunch_not_started_observation(
    cancellation: &CancellationToken,
    budget: ProcessBudget,
) -> Option<(ProcessNotStartedReason, bool, bool)> {
    let cancellation_observed = cancellation.is_cancelled();
    let deadline_observed =
        budget.remaining().is_zero() || Instant::now() >= budget.hard_deadline();
    if cancellation_observed {
        Some((
            ProcessNotStartedReason::Cancelled,
            cancellation_observed,
            deadline_observed,
        ))
    } else if deadline_observed {
        Some((
            ProcessNotStartedReason::Deadline,
            cancellation_observed,
            deadline_observed,
        ))
    } else {
        None
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "terminal resolution consumes the claimed attempt and its retained pre-launch context"
)]
fn resolve_claimed_prelaunch_failure(
    store: &mut PrivateProcessLedgerStore,
    claimed: ClaimedProcessAttempt,
    request: &ProcessRequest,
    budget: ProcessBudget,
    cancellation: &CancellationToken,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
) -> Result<ProcessReceipt, ProcessServiceError> {
    let (reason, cancellation_observed, deadline_observed) = prelaunch_not_started_observation(
        cancellation,
        budget,
    )
    .unwrap_or((ProcessNotStartedReason::SpawnFailed, false, false));
    resolve_claimed_not_started(
        store,
        claimed,
        request,
        reason,
        cancellation_observed,
        deadline_observed,
        timestamp_source,
        faulted,
        errors,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "terminal resolution consumes every retained pre-launch observation"
)]
fn resolve_claimed_not_started(
    store: &mut PrivateProcessLedgerStore,
    claimed: ClaimedProcessAttempt,
    request: &ProcessRequest,
    reason: ProcessNotStartedReason,
    cancellation_observed: bool,
    deadline_observed: bool,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
) -> Result<ProcessReceipt, ProcessServiceError> {
    resolve_outcome(
        store,
        claimed,
        AuthorizedProcessOutcome::NotStarted(ProcessNotStartedReceipt {
            reason,
            launcher_spawned: false,
            launcher_identity: None,
            elapsed: Duration::ZERO,
            cancellation_observed,
            deadline_observed,
        }),
        request,
        None,
        timestamp_source,
        faulted,
        errors,
    )
}

fn finish_preflight_attempt(
    store: &mut PrivateProcessLedgerStore,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
    binding: ProcessEffectBinding,
    request: ProcessRequest,
    preflight: BoundProcessPreflight,
) -> Result<(), ProcessServiceError> {
    if !preflight.matches_request(&request) {
        return Err(errors.denied.clone());
    }
    let claimed =
        claim_exact_attempt(store, &binding, &request, timestamp_source, faulted, errors)?;
    let reason = match preflight.reason() {
        ProcessPreflightReason::Cancelled => ProcessNotStartedEvidenceReason::Cancelled,
        ProcessPreflightReason::DeadlineExceeded => ProcessNotStartedEvidenceReason::Deadline,
    };
    let observed_at = timestamp_source.now_utc();
    if resolve_exact(
        store,
        claimed.into_terminal(),
        EffectResolution::NotStarted(EffectEvidence::process_not_started(reason)),
        &observed_at,
    )
    .is_ok()
    {
        return Ok(());
    }
    faulted.store(true, Ordering::Release);
    Err(errors.indeterminate.clone())
}

fn claim_exact_attempt(
    store: &mut PrivateProcessLedgerStore,
    binding: &ProcessEffectBinding,
    request: &ProcessRequest,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
) -> Result<ClaimedProcessAttempt, ProcessServiceError> {
    let prepared_claim = match store.prepare_exact_process_claim(binding, request) {
        Ok(prepared) => prepared,
        Err(RuntimeStoreError::ExpectedEffectNotClaimable) => {
            return Err(errors.unavailable.clone());
        }
        Err(_) => {
            faulted.store(true, Ordering::Release);
            return Err(errors.indeterminate.clone());
        }
    };
    let claimed_at = timestamp_source.now_utc();
    match store.claim_prepared_process(prepared_claim, &claimed_at) {
        Ok(claimed) => Ok(claimed),
        Err(ExactProcessClaimError::ProvenNotCommitted) => Err(errors.unavailable.clone()),
        Err(ExactProcessClaimError::Indeterminate) => {
            faulted.store(true, Ordering::Release);
            Err(errors.indeterminate.clone())
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "authorized execution consumes every exact launch and durability capability"
)]
fn execute_attempt(
    store: &mut PrivateProcessLedgerStore,
    runtime: &mut InitializedProviderRuntime,
    specification: ProcessSpec,
    claimed: ClaimedProcessAttempt,
    cancellation: &CancellationToken,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
    observer: &impl DurableProcessBarrierObserver,
    message: &ExecuteMessage,
    release_admission: Option<&dyn RustPilotProcessReleaseAdmission>,
) -> Result<ProcessReceipt, ProcessServiceError> {
    let permitted_at_utc = timestamp_source.now_utc();
    if store
        .record_process_spawn_permit(&claimed, &message.request, &permitted_at_utc)
        .is_err()
    {
        faulted.store(true, Ordering::Release);
        return Err(errors.indeterminate.clone());
    }
    let request_binding =
        ProcessRequestBinding::from_bytes(message.request.fingerprint().into_bytes());
    let (start_gate, gate_authority) = ProcessStartGate::channel(request_binding);
    #[cfg(all(unix, feature = "verification-process-canary"))]
    let provider_completion = match runtime.take_provider_completion() {
        Ok(completion) => completion,
        Err(_) => {
            return resolve_uncertainty_and_fault(
                store,
                claimed,
                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                timestamp_source,
                faulted,
                errors,
            );
        }
    };

    thread::scope(|scope| {
        let supervisor = runtime.supervisor();
        let launch = runtime.launch();
        let worker = match thread::Builder::new()
            .name("orchestrator-durable-process-worker".to_owned())
            .spawn_scoped(scope, move || {
                let mut start_gate = start_gate;
                supervisor.run_authorized(&specification, launch, &mut start_gate)
            }) {
            Ok(worker) => worker,
            Err(_) => {
                return resolve_outcome(
                    store,
                    claimed,
                    AuthorizedProcessOutcome::NotStarted(ProcessNotStartedReceipt {
                        reason: ProcessNotStartedReason::SpawnFailed,
                        launcher_spawned: false,
                        launcher_identity: None,
                        elapsed: Duration::ZERO,
                        cancellation_observed: cancellation.is_cancelled(),
                        deadline_observed: false,
                    }),
                    &message.request,
                    None,
                    timestamp_source,
                    faulted,
                    errors,
                );
            }
        };
        let mut gate_observed = false;
        let mut authorized_identity: Option<AuthorizedLauncherIdentity> = None;
        let mut started_observed = false;
        let mut started_authority = None;
        if let Ok((gate_request, authority)) = gate_authority.receive() {
            started_authority = Some(authority);
            {
                gate_observed = true;
                if runtime.verify_for_release().is_err() {
                    let rejected = gate_request.reject().is_ok();
                    return match worker.join() {
                        Ok(Ok(outcome))
                            if rejected
                                && matches!(
                                    &outcome,
                                    AuthorizedProcessOutcome::NotStarted(receipt)
                                        if receipt.reason == ProcessNotStartedReason::GateRejected
                                ) =>
                        {
                            resolve_outcome(
                                store,
                                claimed,
                                outcome,
                                &message.request,
                                None,
                                timestamp_source,
                                faulted,
                                errors,
                            )
                        }
                        Ok(Ok(outcome)) if !rejected => resolve_outcome(
                            store,
                            claimed,
                            outcome,
                            &message.request,
                            None,
                            timestamp_source,
                            faulted,
                            errors,
                        ),
                        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => resolve_uncertainty_and_fault(
                            store,
                            claimed,
                            ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                            timestamp_source,
                            faulted,
                            errors,
                        ),
                    };
                }
                let release_permit = if let Some(admission) = release_admission {
                    let Some(permit) = admission.admit_release() else {
                        let _ = gate_request.reject();
                        return match worker.join() {
                            Ok(Ok(outcome)) => resolve_outcome(
                                store,
                                claimed,
                                outcome,
                                &message.request,
                                None,
                                timestamp_source,
                                faulted,
                                errors,
                            ),
                            Ok(Err(_)) | Err(_) => resolve_uncertainty_and_fault(
                                store,
                                claimed,
                                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                                timestamp_source,
                                faulted,
                                errors,
                            ),
                        };
                    };
                    Some(permit)
                } else {
                    None
                };
                // The owner decision precedes the gate's existing atomic
                // release-versus-cancellation arbitration. Once admitted,
                // any later request cancels through that same token.
                let recorded_at = timestamp_source.now_utc();
                let kernel = gate_request.identity();
                let identity = ProcessExecutionIdentity::new(
                    claimed.claim_attempt(),
                    kernel.pid(),
                    kernel.process_group_id(),
                    kernel.process_start_identity(),
                    recorded_at.clone(),
                );
                match identity.and_then(|identity| {
                    store.record_blocked_process_identity(
                        &claimed,
                        &message.request,
                        &gate_request,
                        &identity,
                    )?;
                    store.authorize_process_release(
                        &claimed,
                        &message.request,
                        &gate_request,
                        &identity,
                        &recorded_at,
                    )
                }) {
                    Ok(authorization) => {
                        let released = authorization.release(gate_request);
                        // Admission covers the release decision, not waits for
                        // receipts or observers that cancellation must interrupt.
                        drop(release_permit);
                        match observe_after_success(
                            released,
                            DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
                            observer,
                        ) {
                            Ok(identity) => authorized_identity = Some(identity),
                            Err(ProcessAuthorizationReleaseError::GateClosed(identity)) => {
                                authorized_identity = Some(identity);
                            }
                            Err(ProcessAuthorizationReleaseError::BindingMismatch) => {
                                let _ = worker.join();
                                return resolve_uncertainty_and_fault(
                                    store,
                                    claimed,
                                    ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                                    timestamp_source,
                                    faulted,
                                    errors,
                                );
                            }
                        }
                    }
                    Err(_) => {
                        drop(release_permit);
                        match store.process_release_was_authorized(
                            claimed.idempotency_key(),
                            claimed.claim_attempt(),
                        ) {
                            Ok(false) => {
                                let _ = gate_request.reject();
                            }
                            Ok(true) | Err(_) => {
                                let _ = gate_request.indeterminate();
                                let _ = worker.join();
                                return resolve_uncertainty_and_fault(
                                    store,
                                    claimed,
                                    ProcessUncertaintyEvidence::CommitIndeterminate,
                                    timestamp_source,
                                    faulted,
                                    errors,
                                );
                            }
                        }
                    }
                }
            }
        }
        if let (Some(authorized), Some(authority)) =
            (authorized_identity.as_ref(), started_authority.take())
        {
            if let Ok(started) = authority.receive_started() {
                let observed_at = timestamp_source.now_utc();
                match store.record_process_started_observed(
                    &claimed,
                    &message.request,
                    authorized,
                    started.receipt(),
                    &observed_at,
                ) {
                    Ok(()) => {
                        started_observed = true;
                        if observe_after_success(
                            started.persisted(),
                            DurableProcessBarrierPoint::StartedPersistedBeforeTerminalResolution,
                            observer,
                        )
                        .is_err()
                        {
                            let _ = worker.join();
                            return resolve_uncertainty_and_fault(
                                store,
                                claimed,
                                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                                timestamp_source,
                                faulted,
                                errors,
                            );
                        }
                    }
                    Err(_) => {
                        let _ = started.indeterminate();
                        let _ = worker.join();
                        return resolve_uncertainty_and_fault(
                            store,
                            claimed,
                            ProcessUncertaintyEvidence::CommitIndeterminate,
                            timestamp_source,
                            faulted,
                            errors,
                        );
                    }
                }
            }
        }

        match worker.join() {
            Ok(Ok(outcome)) => {
                if matches!(
                    (&outcome, started_observed),
                    (AuthorizedProcessOutcome::Started(_), false)
                        | (AuthorizedProcessOutcome::NotStarted(_), true)
                ) {
                    return resolve_uncertainty_and_fault(
                        store,
                        claimed,
                        ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                        timestamp_source,
                        faulted,
                        errors,
                    );
                }
                #[cfg(all(unix, feature = "verification-process-canary"))]
                if runtime
                    .complete_canary_if_successful(
                        provider_completion,
                        &outcome,
                        authorized_identity.as_ref(),
                        &claimed.outcome_binding(),
                    )
                    .is_err()
                {
                    return resolve_uncertainty_and_fault(
                        store,
                        claimed,
                        ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                        timestamp_source,
                        faulted,
                        errors,
                    );
                }
                resolve_outcome(
                    store,
                    claimed,
                    outcome,
                    &message.request,
                    authorized_identity,
                    timestamp_source,
                    faulted,
                    errors,
                )
            }
            Ok(Err(error))
                if error.classification()
                    == AuthorizedProcessErrorClassification::ProvenNotStarted =>
            {
                if gate_observed || authorized_identity.is_some() {
                    resolve_uncertainty_and_fault(
                        store,
                        claimed,
                        ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                        timestamp_source,
                        faulted,
                        errors,
                    )
                } else {
                    resolve_outcome(
                        store,
                        claimed,
                        AuthorizedProcessOutcome::NotStarted(ProcessNotStartedReceipt {
                            reason: ProcessNotStartedReason::SpawnFailed,
                            launcher_spawned: false,
                            launcher_identity: None,
                            elapsed: Duration::ZERO,
                            cancellation_observed: cancellation.is_cancelled(),
                            deadline_observed: false,
                        }),
                        &message.request,
                        None,
                        timestamp_source,
                        faulted,
                        errors,
                    )
                }
            }
            Ok(Err(_)) | Err(_) => resolve_uncertainty_and_fault(
                store,
                claimed,
                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                timestamp_source,
                faulted,
                errors,
            ),
        }
    })
}

fn build_spec_after_claim(
    _claimed: &ClaimedProcessAttempt,
    request: &ProcessRequest,
    budget: ProcessBudget,
    cancellation: &CancellationToken,
    errors: &DurableProcessServiceErrors,
    output_sender: Option<ProcessOutputSender>,
) -> Result<ProcessSpec, ProcessServiceError> {
    let mut argv = Vec::with_capacity(request.expose_arguments().len().saturating_add(1));
    argv.push(OsString::from(match request.purpose() {
        orchestrator_exec::ProcessPurpose::Verification => "nanika-verifier",
        _ => PROVIDER_WORKER_ARGV0,
    }));
    argv.extend(request.expose_arguments().iter().map(OsString::from));
    let mut specification = ProcessSpec::new(argv, budget.remaining())
        .map_err(|_| errors.invalid.clone())?
        .with_hard_deadline_at(budget.hard_deadline())
        .with_max_output_bytes(request.max_output_bytes())
        .with_cancellation(cancellation.clone());
    if !budget.stall_window().is_zero() {
        specification = specification.with_stall_timeout(budget.stall_window());
    }
    if let Some(output_sender) = output_sender {
        specification = specification.with_output_sender(output_sender);
    }
    for (key, value) in request.expose_environment() {
        specification = specification.with_env(key, value);
    }
    if let Some(stdin) = request.expose_stdin() {
        specification = specification.with_stdin(stdin.to_vec());
    }
    Ok(specification)
}

#[expect(
    clippy::too_many_arguments,
    reason = "terminal resolution consumes every retained attempt observation"
)]
fn resolve_outcome(
    store: &mut PrivateProcessLedgerStore,
    claimed: ClaimedProcessAttempt,
    outcome: AuthorizedProcessOutcome,
    request: &ProcessRequest,
    authorized_identity: Option<AuthorizedLauncherIdentity>,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
) -> Result<ProcessReceipt, ProcessServiceError> {
    let observation = DurableProcessOutcomeObservation::from_outcome(&outcome);
    let pending =
        match map_authorized_process_outcome(outcome, request, authorized_identity, &claimed) {
            Ok(mapped) => match mapped.bind(claimed) {
                Ok(pending) => pending,
                Err(_) => {
                    faulted.store(true, Ordering::Release);
                    return Err(errors.indeterminate.clone());
                }
            },
            Err(_) => {
                return resolve_uncertainty_and_fault(
                    store,
                    claimed,
                    ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                    timestamp_source,
                    faulted,
                    errors,
                );
            }
        };
    let observed_at = timestamp_source.now_utc();
    if let Ok(receipt) = pending.commit(store, &observed_at) {
        *lock_unpoisoned(&errors.outcome) = Some(observation);
        return Ok(receipt);
    }
    faulted.store(true, Ordering::Release);
    Err(errors.indeterminate.clone())
}

fn resolve_uncertainty_and_fault(
    store: &mut PrivateProcessLedgerStore,
    claimed: ClaimedProcessAttempt,
    uncertainty: ProcessUncertaintyEvidence,
    timestamp_source: &dyn ProcessTimestampSource,
    faulted: &AtomicBool,
    errors: &DurableProcessServiceErrors,
) -> Result<ProcessReceipt, ProcessServiceError> {
    let observed_at = timestamp_source.now_utc();
    let terminal = claimed.into_terminal();
    let _ = resolve_exact(
        store,
        terminal,
        EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(uncertainty)),
        &observed_at,
    );
    faulted.store(true, Ordering::Release);
    Err(errors.indeterminate.clone())
}

fn resolve_exact(
    store: &mut PrivateProcessLedgerStore,
    terminal: ProcessTerminalClaim,
    resolution: EffectResolution,
    observed_at: &str,
) -> Result<(), RuntimeStoreError> {
    let expected = resolution.clone();
    if store
        .resolve_claimed_process(&terminal, resolution, observed_at)
        .is_ok()
        || store
            .resolve_claimed_process(&terminal, expected.clone(), observed_at)
            .is_ok()
    {
        return Ok(());
    }
    if store.claimed_process_resolution_is_durable(&terminal, &expected, observed_at)? {
        Ok(())
    } else {
        Err(RuntimeStoreError::InvalidOutboxTransition)
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::owned_process_serialization::OwnedProcessTurn;
    use crate::runtime_store::PrivateProcessLedgerBoundary;
    use crate::{
        EffectOperationSlot, JournalIntent, OutboxEffectKind, OutboxIntent, OutboxState,
        ProductionBoundary, RuntimeStore, StorageActorAuthority,
    };
    use orchestrator_core::MissionId;
    use orchestrator_exec::{
        Clock, EffectBudget, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
        EffectServiceErrorKind, EventReceipt, EventSink, EventSinkError, EventSinkErrorKind,
        ExecutionContext, ProcessPurpose, ProcessTerminationReceipt, WatchdogDecision,
        WatchdogPolicy, WorkerEventDraft, WorkerIdentity,
    };
    use orchestrator_process::{ProcessError, ProcessSupervisor, ProductionProcessLaunchAuthority};
    use rusqlite::{Connection, params};
    use std::{
        error::Error,
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::{Barrier, atomic::AtomicUsize},
        time::Instant,
    };

    type TestResult<T = ()> = Result<T, Box<dyn Error>>;

    static NEXT_HOME: AtomicUsize = AtomicUsize::new(1);

    struct TestHome {
        path: PathBuf,
        /// Held for the whole test so no other test observes this one's owned
        /// children. Declared last so it is released after every other field.
        _serialization: OwnedProcessTurn,
    }

    impl TestHome {
        fn new() -> TestResult<Self> {
            let path = std::env::temp_dir().join(format!(
                "orchestrator-durable-preflight-{}-{}",
                std::process::id(),
                NEXT_HOME.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            Ok(Self {
                path: fs::canonicalize(path)?,
                _serialization: OwnedProcessTurn::take(),
            })
        }

        fn boundary(&self) -> TestResult<Arc<ProductionBoundary>> {
            Ok(Arc::new(ProductionBoundary::from_canonical_root(
                &self.path,
            )?))
        }

        fn database(&self) -> PathBuf {
            self.path.join("runtime.db")
        }

        fn open_store(&self) -> TestResult<PrivateProcessLedgerStore> {
            Ok(RuntimeStore::open_private(
                PrivateProcessLedgerBoundary::new(self.boundary()?),
                StorageActorAuthority::new(),
            )?)
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
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
            format!("2026-07-17T00:00:{step:02}Z")
        }
    }

    fn current_kernel_identity() -> TestResult<KernelProcessIdentity> {
        let pid = std::process::id();
        let process_group_id = u32::try_from(rustix::process::getpgid(None)?.as_raw_pid())?;
        Ok(KernelProcessIdentity::observe(pid, process_group_id)?)
    }

    fn outcome_report(
        identity: Option<KernelProcessIdentity>,
    ) -> orchestrator_process::ProcessReport {
        orchestrator_process::ProcessReport {
            termination: orchestrator_process::ProcessTermination::Exited(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            truncated: false,
            stdout_discarded_bytes: 0,
            stderr_discarded_bytes: 0,
            elapsed: Duration::ZERO,
            spawned: true,
            pid: identity.as_ref().map(KernelProcessIdentity::pid),
            pgid: identity
                .as_ref()
                .map(KernelProcessIdentity::process_group_id),
            kernel_identity: identity,
            cancellation_observed: false,
            deadline_observed: false,
            stall_observed: false,
            term_sent: false,
            kill_sent: false,
            escalated_to_kill: false,
            direct_child_reaped: true,
            group_absent: true,
            cleanup_complete: true,
            infrastructure_failures: Vec::new(),
        }
    }

    #[test]
    fn uncertain_release_delivery_does_not_fabricate_post_release_identity() -> TestResult {
        let launcher_identity = current_kernel_identity()?;
        let outcome =
            AuthorizedProcessOutcome::Uncertain(orchestrator_process::ProcessUncertainReceipt {
                reason: orchestrator_process::ProcessUncertainReason::ReleaseDelivery,
                launcher_identity: Some(launcher_identity.clone()),
                report: outcome_report(None),
            });
        let observation = DurableProcessOutcomeObservation::from_outcome(&outcome);

        assert!(!observation.release_confirmed);
        assert!(observation.post_release_identity.is_none());
        let cleanup_identity = observation
            .cleanup_identity
            .ok_or("missing known launcher cleanup identity")?;
        assert_eq!(cleanup_identity.pid(), launcher_identity.pid());
        assert_eq!(
            cleanup_identity.process_group_id(),
            launcher_identity.process_group_id()
        );
        assert_eq!(
            cleanup_identity.process_start_identity(),
            launcher_identity.process_start_identity()
        );
        Ok(())
    }

    #[test]
    fn confirmed_started_outcome_retains_report_kernel_identity() -> TestResult {
        let report_identity = current_kernel_identity()?;
        let outcome =
            AuthorizedProcessOutcome::Started(outcome_report(Some(report_identity.clone())));
        let observation = DurableProcessOutcomeObservation::from_outcome(&outcome);

        assert!(observation.release_confirmed);
        let post_release_identity = observation
            .post_release_identity
            .ok_or("missing confirmed post-release identity")?;
        assert_eq!(post_release_identity.pid(), report_identity.pid());
        assert_eq!(
            post_release_identity.process_group_id(),
            report_identity.process_group_id()
        );
        assert_eq!(
            post_release_identity.process_start_identity(),
            report_identity.process_start_identity()
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    struct InvalidStartedMarkerTimestampSource {
        calls: AtomicUsize,
    }

    #[cfg(target_os = "macos")]
    impl InvalidStartedMarkerTimestampSource {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl ProcessTimestampSource for InvalidStartedMarkerTimestampSource {
        fn now_utc(&self) -> String {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 3 {
                "invalid-started-marker-timestamp".to_owned()
            } else {
                format!("2026-07-17T00:01:{call:02}Z")
            }
        }
    }

    #[cfg(target_os = "macos")]
    struct CancelBeforeStartedAcknowledgementTimestampSource {
        calls: AtomicUsize,
        cancellation: CancellationToken,
    }

    #[cfg(target_os = "macos")]
    impl CancelBeforeStartedAcknowledgementTimestampSource {
        fn new(cancellation: CancellationToken) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                cancellation,
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl ProcessTimestampSource for CancelBeforeStartedAcknowledgementTimestampSource {
        fn now_utc(&self) -> String {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 3 {
                self.cancellation.cancel();
            }
            format!("2026-07-17T00:02:{call:02}Z")
        }
    }

    #[cfg(target_os = "macos")]
    #[derive(Clone, Debug, Eq, PartialEq)]
    struct DurableBoundarySnapshot {
        point: DurableProcessBarrierPoint,
        state: String,
        attempts: i64,
        claim_markers: i64,
        spawn_permits: i64,
        release_authorizations: i64,
        started_markers: i64,
        terminal_observations: i64,
        target_started: bool,
    }

    #[cfg(target_os = "macos")]
    type DurableBoundaryLog = Arc<Mutex<Vec<Result<DurableBoundarySnapshot, String>>>>;

    #[cfg(target_os = "macos")]
    struct RecordingDurableProcessObserver {
        database: PathBuf,
        idempotency_key: String,
        sentinel: PathBuf,
        observations: DurableBoundaryLog,
    }

    #[cfg(target_os = "macos")]
    impl DurableProcessBarrierObserver for RecordingDurableProcessObserver {
        fn observe(&self, point: DurableProcessBarrierPoint) {
            let snapshot = self
                .snapshot(point)
                .map_err(|error| format!("{point:?}: {error}"));
            lock_unpoisoned(&self.observations).push(snapshot);
        }
    }

    #[cfg(target_os = "macos")]
    impl RecordingDurableProcessObserver {
        fn snapshot(
            &self,
            point: DurableProcessBarrierPoint,
        ) -> TestResult<DurableBoundarySnapshot> {
            let connection = Connection::open(&self.database)?;
            let (state, attempts) = connection.query_row(
                "SELECT state, attempts FROM outbox WHERE idempotency_key = ?1",
                params![self.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let count_journal = |transition_kind: &str| -> rusqlite::Result<i64> {
                connection.query_row(
                    "SELECT count(*) FROM journal WHERE transition_kind = ?1",
                    params![transition_kind],
                    |row| row.get(0),
                )
            };
            let release_authorizations = connection.query_row(
                "SELECT count(*) FROM outbox_process_release_authorization
                 WHERE idempotency_key = ?1",
                params![self.idempotency_key],
                |row| row.get(0),
            )?;
            let terminal_observations = connection.query_row(
                "SELECT count(*) FROM outbox_attempt_observation
                 WHERE idempotency_key = ?1",
                params![self.idempotency_key],
                |row| row.get(0),
            )?;
            Ok(DurableBoundarySnapshot {
                point,
                state,
                attempts,
                claim_markers: count_journal("cell3.process_attempt_claimed")?,
                spawn_permits: count_journal("cell3.process_spawn_permitted")?,
                release_authorizations,
                started_markers: count_journal("cell3.process_started_observed")?,
                terminal_observations,
                target_started: self.sentinel.try_exists()?,
            })
        }
    }

    struct FixedClock(Instant);

    impl Clock for FixedClock {
        fn now(&self) -> Instant {
            self.0
        }
    }

    struct ExpiringClock {
        before_deadline: Instant,
        deadline: Instant,
        calls: AtomicUsize,
    }

    impl ExpiringClock {
        fn new(before_deadline: Instant, deadline: Instant) -> Self {
            Self {
                before_deadline,
                deadline,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Clock for ExpiringClock {
        fn now(&self) -> Instant {
            if self.calls.fetch_add(1, Ordering::SeqCst) < 2 {
                self.before_deadline
            } else {
                self.deadline
            }
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

    struct RejectingEffectService {
        error: EffectServiceError,
    }

    impl RejectingEffectService {
        fn new() -> TestResult<Self> {
            Ok(Self {
                error: EffectServiceError::new(
                    EffectServiceErrorKind::Unavailable,
                    "durable preflight test does not admit effects",
                )?,
            })
        }
    }

    impl EffectService for RejectingEffectService {
        fn execute(
            &self,
            _request: &EffectRequest,
            _budget: EffectBudget<'_>,
        ) -> Result<EffectReceipt, EffectServiceError> {
            Err(self.error.clone())
        }
    }

    struct RejectingEventSink {
        error: EventSinkError,
    }

    impl RejectingEventSink {
        fn new() -> TestResult<Self> {
            Ok(Self {
                error: EventSinkError::new(
                    EventSinkErrorKind::Unavailable,
                    "durable preflight test does not emit events",
                )?,
            })
        }
    }

    impl EventSink for RejectingEventSink {
        fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
            Err(self.error.clone())
        }
    }

    fn timestamp(step: u8) -> String {
        format!("2026-07-17T00:00:{step:02}Z")
    }

    fn process_request(argument: &str) -> TestResult<ProcessRequest> {
        Ok(ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-worker",
            "/fixture/worker",
        )?
        .with_argument(argument)?
        .with_environment("FIXTURE_TOKEN", "sensitive-value")?
        .with_max_output_bytes(32_768)?)
    }

    #[derive(Clone, Copy, Debug)]
    enum ExactRequestMutation {
        Purpose,
        Executable,
        WorkingRoot,
        Arguments,
        EnvironmentOrder,
        EnvironmentLoader,
        StdinEmpty,
        StdinBytes,
        MaxOutput,
        Truncation,
    }

    fn exact_admission_request(
        mutation: Option<ExactRequestMutation>,
    ) -> TestResult<ProcessRequest> {
        let purpose = if matches!(mutation, Some(ExactRequestMutation::Purpose)) {
            ProcessPurpose::Git
        } else {
            ProcessPurpose::ProviderWorker
        };
        let executable = if matches!(mutation, Some(ExactRequestMutation::Executable)) {
            "other-enrolled-worker"
        } else {
            "enrolled-worker"
        };
        let working_root = if matches!(mutation, Some(ExactRequestMutation::WorkingRoot)) {
            "/fixture/other-worker"
        } else {
            "/fixture/worker"
        };
        let arguments = if matches!(mutation, Some(ExactRequestMutation::Arguments)) {
            ["duplicate", "duplicate", "middle"]
        } else {
            ["duplicate", "middle", "duplicate"]
        };
        let environment = if matches!(mutation, Some(ExactRequestMutation::EnvironmentOrder)) {
            [
                ("DUPLICATE", "same"),
                ("DUPLICATE", "same"),
                ("ORDER", "middle"),
            ]
        } else {
            [
                ("DUPLICATE", "same"),
                ("ORDER", "middle"),
                ("DUPLICATE", "same"),
            ]
        };
        let mut request = ProcessRequest::new(purpose, executable, working_root)?;
        for argument in arguments {
            request = request.with_argument(argument)?;
        }
        for (key, value) in environment {
            request = request.with_environment(key, value)?;
        }
        if matches!(mutation, Some(ExactRequestMutation::EnvironmentLoader)) {
            request = request.with_environment("DYLD_INSERT_LIBRARIES", "/tmp/not-loaded")?;
        }
        if matches!(mutation, Some(ExactRequestMutation::StdinEmpty)) {
            request = request.with_stdin(Vec::new())?;
        } else if matches!(mutation, Some(ExactRequestMutation::StdinBytes)) {
            request = request.with_stdin(vec![0x00, 0x7f, 0xff])?;
        }
        request = request.with_max_output_bytes(
            if matches!(mutation, Some(ExactRequestMutation::MaxOutput)) {
                32_767
            } else {
                32_768
            },
        )?;
        if matches!(mutation, Some(ExactRequestMutation::Truncation)) {
            request = request.with_truncated_output_acknowledged();
        }
        Ok(request)
    }

    fn append_pending_process(
        store: &mut PrivateProcessLedgerStore,
        request: &ProcessRequest,
        transition_id: &str,
    ) -> TestResult<(String, ProcessEffectBinding)> {
        let effect = OutboxIntent::for_process(
            MissionId::new("mission-1")?,
            Some("phase-1".to_owned()),
            OutboxEffectKind::ProviderProcess,
            EffectOperationSlot::new(transition_id)?,
            1,
            serde_json::json!({"provider":"fixture"}),
            request,
        )?;
        let key = effect.idempotency_key().to_owned();
        let binding = effect.bind_process(request)?;
        let intent = JournalIntent::new(
            transition_id,
            Some(MissionId::new("mission-1")?),
            "phase.started",
            serde_json::json!({"phase_id":"phase-1"}),
            timestamp(1),
        )?
        .with_outbox(effect)?;
        store.append(&intent)?;
        Ok((key, binding))
    }

    fn spawn_preflight_test_actor(
        store: PrivateProcessLedgerStore,
        binding: ProcessEffectBinding,
        request: &ProcessRequest,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        runtime_initializations: Arc<AtomicUsize>,
    ) -> TestResult<(
        DurableProcessActor,
        DurableProcessService,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    )> {
        let preclaim_verifications = Arc::new(AtomicUsize::new(0));
        let release_verifications = Arc::new(AtomicUsize::new(0));
        let runtime = DeferredProviderRuntime::injected_with_verifiers(
            {
                let preclaim_verifications = Arc::clone(&preclaim_verifications);
                move || {
                    preclaim_verifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            move || {
                runtime_initializations.fetch_add(1, Ordering::SeqCst);
                Err(ProcessError::InvalidSpec)
            },
            {
                let release_verifications = Arc::clone(&release_verifications);
                move || {
                    release_verifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );
        let launch = EnrolledProviderLaunch::injected_for_test(request, binding, runtime)?;
        let (actor, service) =
            DurableProcessActor::spawn(store, launch, cancellation, timestamp_source)?;
        Ok((
            actor,
            service,
            preclaim_verifications,
            release_verifications,
        ))
    }

    fn assert_preflight_terminalizes(
        reason: ProcessPreflightReason,
        expected_termination: ProcessTerminationReceipt,
        expected_evidence: ProcessNotStartedEvidenceReason,
    ) -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("preflight")?;
        let mut store = home.open_store()?;
        let (key, binding) = append_pending_process(&mut store, &request, "durable-preflight")?;
        let cancellation = CancellationToken::new();
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, service, preclaim_verifications, release_verifications) =
            spawn_preflight_test_actor(
                store,
                binding,
                &request,
                cancellation,
                timestamp_source,
                Arc::clone(&runtime_initializations),
            )?;
        let now = Instant::now();
        let deadline = match reason {
            ProcessPreflightReason::Cancelled => {
                assert!(service.cancel());
                now + Duration::from_secs(60)
            }
            ProcessPreflightReason::DeadlineExceeded => now,
        };
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            deadline,
        );

        let receipt = context.run_process(&request)?;
        assert_eq!(receipt.termination(), expected_termination);
        drop(context);
        assert_eq!(preclaim_verifications.load(Ordering::SeqCst), 0);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        assert_eq!(release_verifications.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        drop(service);

        let reopened = home.open_store()?;
        assert!(reopened.pending_effects(10)?.is_empty());
        assert!(reopened.recovery_effects(10)?.is_empty());
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().code(),
            crate::EffectEvidenceCode::ProcessNotStarted
        );
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(expected_evidence)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn cancelled_execution_context_terminalizes_exact_process_without_launch() -> TestResult {
        assert_preflight_terminalizes(
            ProcessPreflightReason::Cancelled,
            ProcessTerminationReceipt::Cancelled,
            ProcessNotStartedEvidenceReason::Cancelled,
        )
    }

    #[test]
    fn expired_execution_context_terminalizes_exact_process_without_launch() -> TestResult {
        assert_preflight_terminalizes(
            ProcessPreflightReason::DeadlineExceeded,
            ProcessTerminationReceipt::DeadlineExceeded,
            ProcessNotStartedEvidenceReason::Deadline,
        )
    }

    fn assert_post_claim_prelaunch_failure_terminalizes_once(
        transition_id: &str,
        argument: &str,
        remaining: Duration,
        expected_preclaim_verifications: usize,
        expected_preclaim_observed_unclaimed: bool,
        expected_runtime_initializations: usize,
        expected_claim_observed_by_initializer: bool,
    ) -> TestResult {
        let home = TestHome::new()?;
        let request = process_request(argument)?;
        let mut store = home.open_store()?;
        let (key, binding) = append_pending_process(&mut store, &request, transition_id)?;
        let preclaim_verifications = Arc::new(AtomicUsize::new(0));
        let preclaim_observed_unclaimed = Arc::new(AtomicBool::new(false));
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let claim_observed_by_initializer = Arc::new(AtomicBool::new(false));
        let release_verifications = Arc::new(AtomicUsize::new(0));
        let database = home.database();
        let preclaim_database = database.clone();
        let preclaim_key = key.clone();
        let observed_key = key.clone();
        let runtime = DeferredProviderRuntime::injected_with_verifiers(
            {
                let preclaim_verifications = Arc::clone(&preclaim_verifications);
                let preclaim_observed_unclaimed = Arc::clone(&preclaim_observed_unclaimed);
                move || {
                    preclaim_verifications.fetch_add(1, Ordering::SeqCst);
                    let unclaimed = Connection::open(&preclaim_database)
                        .and_then(|connection| {
                            connection.query_row(
                                "SELECT state, attempts FROM outbox WHERE idempotency_key = ?1",
                                params![preclaim_key],
                                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                            )
                        })
                        .is_ok_and(|(state, attempts)| state == "pending" && attempts == 0);
                    preclaim_observed_unclaimed.store(unclaimed, Ordering::SeqCst);
                    Ok(())
                }
            },
            {
                let runtime_initializations = Arc::clone(&runtime_initializations);
                let claim_observed_by_initializer = Arc::clone(&claim_observed_by_initializer);
                move || {
                    runtime_initializations.fetch_add(1, Ordering::SeqCst);
                    let claimed = Connection::open(&database)
                        .and_then(|connection| {
                            connection.query_row(
                                "SELECT state, attempts FROM outbox WHERE idempotency_key = ?1",
                                params![observed_key],
                                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                            )
                        })
                        .is_ok_and(|(state, attempts)| state == "executing" && attempts == 1);
                    claim_observed_by_initializer.store(claimed, Ordering::SeqCst);
                    Err(ProcessError::InvalidSpec)
                }
            },
            {
                let release_verifications = Arc::clone(&release_verifications);
                move || {
                    release_verifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );
        let launch = EnrolledProviderLaunch::injected_for_test(&request, binding, runtime)?;
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, service) =
            DurableProcessActor::spawn(store, launch, CancellationToken::new(), timestamp_source)?;
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + remaining,
        );

        let receipt = context.run_process(&request)?;
        assert_eq!(
            receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        drop(context);
        assert_eq!(
            preclaim_verifications.load(Ordering::SeqCst),
            expected_preclaim_verifications
        );
        assert_eq!(
            preclaim_observed_unclaimed.load(Ordering::SeqCst),
            expected_preclaim_observed_unclaimed
        );
        assert_eq!(
            runtime_initializations.load(Ordering::SeqCst),
            expected_runtime_initializations
        );
        assert_eq!(
            claim_observed_by_initializer.load(Ordering::SeqCst),
            expected_claim_observed_by_initializer
        );
        assert_eq!(release_verifications.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        drop(service);

        let reopened = home.open_store()?;
        assert!(reopened.pending_effects(10)?.is_empty());
        assert!(reopened.recovery_effects(10)?.is_empty());
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::SpawnFailed)
        );
        reopened.close()?;

        let connection = Connection::open(home.database())?;
        let claim_count: i64 = connection.query_row(
            "SELECT count(*) FROM outbox_attempt_claim WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        let claim_marker_count: i64 = connection.query_row(
            "SELECT count(*) FROM journal
             WHERE transition_kind = 'cell3.process_attempt_claimed'",
            [],
            |row| row.get(0),
        )?;
        let spawn_permit_count: i64 = connection.query_row(
            "SELECT count(*) FROM journal
             WHERE transition_kind = 'cell3.process_spawn_permitted'",
            [],
            |row| row.get(0),
        )?;
        let identity_count: i64 = connection.query_row(
            "SELECT count(*) FROM outbox_execution_identity WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        let release_count: i64 = connection.query_row(
            "SELECT count(*) FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        assert_eq!(claim_count, 1);
        assert_eq!(claim_marker_count, 1);
        assert_eq!(spawn_permit_count, 0);
        assert_eq!(identity_count, 0);
        assert_eq!(release_count, 0);
        Ok(())
    }

    #[test]
    fn runtime_initializer_failure_terminalizes_the_previously_claimed_attempt() -> TestResult {
        assert_post_claim_prelaunch_failure_terminalizes_once(
            "runtime-initializer-failure",
            "runtime-initializer-failure",
            Duration::from_secs(60),
            1,
            true,
            1,
            true,
        )
    }

    #[test]
    fn invalid_process_spec_terminalizes_the_claim_without_initializing_runtime() -> TestResult {
        assert_post_claim_prelaunch_failure_terminalizes_once(
            "invalid-process-spec",
            "invalid-process-spec",
            Duration::from_secs(24 * 60 * 60 + 1),
            1,
            true,
            0,
            false,
        )
    }

    #[cfg(target_os = "macos")]
    fn release_gate_runtime(
        home: &TestHome,
        preclaim_verifications: Arc<AtomicUsize>,
        runtime_initializations: Arc<AtomicUsize>,
        release_verifications: Arc<AtomicUsize>,
        verify_release: impl Fn() -> Result<(), ProcessError> + Send + Sync + 'static,
    ) -> TestResult<DeferredProviderRuntime> {
        let root = home.path.join("release-gate-root");
        let bin = root.join("bin");
        let workspaces = root.join("workspaces");
        let executable = bin.join("helper");
        let cwd = workspaces.join("run");
        fs::create_dir_all(&cwd)?;
        fs::create_dir_all(&bin)?;
        for directory in [&root, &bin, &workspaces, &cwd] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        let image = b"#!/bin/sh\nprintf released > release-sentinel\n";
        fs::write(&executable, image)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        let launch = ProductionProcessLaunchAuthority::new_disposable_canary(
            fs::File::open(&root)?,
            fs::File::open(&executable)?,
            PathBuf::from("bin/helper"),
            fs::File::open(&cwd)?,
            PathBuf::from("workspaces/run"),
            image,
        )?;
        let supervisor = ProcessSupervisor::process_wide()?;
        Ok(DeferredProviderRuntime::injected_with_verifiers(
            move || {
                preclaim_verifications.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            move || {
                runtime_initializations.fetch_add(1, Ordering::SeqCst);
                Ok((launch, supervisor))
            },
            move || {
                release_verifications.fetch_add(1, Ordering::SeqCst);
                verify_release()
            },
        ))
    }

    #[cfg(target_os = "macos")]
    type SpawnedBarrierTestActor = (
        DurableProcessActor,
        DurableProcessService,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    );

    #[cfg(target_os = "macos")]
    fn spawn_barrier_test_actor<Observer>(
        home: &TestHome,
        request: &ProcessRequest,
        binding: ProcessEffectBinding,
        cancellation: CancellationToken,
        timestamp_source: Arc<dyn ProcessTimestampSource>,
        observer: Observer,
        verify_release: impl Fn() -> Result<(), ProcessError> + Send + Sync + 'static,
    ) -> TestResult<SpawnedBarrierTestActor>
    where
        Observer: DurableProcessBarrierObserver,
    {
        let preclaim_verifications = Arc::new(AtomicUsize::new(0));
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let release_verifications = Arc::new(AtomicUsize::new(0));
        let runtime = release_gate_runtime(
            home,
            Arc::clone(&preclaim_verifications),
            Arc::clone(&runtime_initializations),
            Arc::clone(&release_verifications),
            verify_release,
        )?;
        let launch = EnrolledProviderLaunch::injected_for_test(request, binding, runtime)?;
        let store = home.open_store()?;
        let (actor, service) = DurableProcessActor::spawn_with_observer(
            store,
            launch,
            cancellation,
            timestamp_source,
            observer,
        )?;
        Ok((
            actor,
            service,
            preclaim_verifications,
            runtime_initializations,
            release_verifications,
        ))
    }

    #[cfg(target_os = "macos")]
    fn execute_test_process(
        service: &DurableProcessService,
        request: &ProcessRequest,
    ) -> TestResult<Result<ProcessReceipt, ProcessServiceError>> {
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );
        Ok(context.run_process(request))
    }

    #[cfg(target_os = "macos")]
    fn recording_observer(
        home: &TestHome,
        idempotency_key: &str,
    ) -> (RecordingDurableProcessObserver, DurableBoundaryLog) {
        let observations = Arc::new(Mutex::new(Vec::new()));
        (
            RecordingDurableProcessObserver {
                database: home.database(),
                idempotency_key: idempotency_key.to_owned(),
                sentinel: home
                    .path
                    .join("release-gate-root/workspaces/run/release-sentinel"),
                observations: Arc::clone(&observations),
            },
            observations,
        )
    }

    #[cfg(target_os = "macos")]
    fn completed_boundary_snapshots(
        observations: &DurableBoundaryLog,
    ) -> TestResult<Vec<DurableBoundarySnapshot>> {
        lock_unpoisoned(observations)
            .clone()
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| std::io::Error::other(error).into())
    }

    #[cfg(target_os = "macos")]
    fn count_started_markers(home: &TestHome) -> TestResult<i64> {
        Ok(Connection::open(home.database())?.query_row(
            "SELECT count(*) FROM journal
             WHERE transition_kind = 'cell3.process_started_observed'",
            [],
            |row| row.get(0),
        )?)
    }

    #[derive(Clone)]
    struct InMemoryDurableProcessObserver {
        observations: Arc<Mutex<Vec<DurableProcessBarrierPoint>>>,
    }

    impl DurableProcessBarrierObserver for InMemoryDurableProcessObserver {
        fn observe(&self, point: DurableProcessBarrierPoint) {
            lock_unpoisoned(&self.observations).push(point);
        }
    }

    #[test]
    fn barrier_observer_is_not_called_for_failed_release_or_ack_results() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observer = InMemoryDurableProcessObserver {
            observations: Arc::clone(&observations),
        };
        let mismatched_release: Result<(), ProcessAuthorizationReleaseError> =
            Err(ProcessAuthorizationReleaseError::BindingMismatch);
        assert!(
            observe_after_success(
                mismatched_release,
                DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
                &observer,
            )
            .is_err()
        );
        assert!(
            observe_after_success(
                Err::<(), ()>(()),
                DurableProcessBarrierPoint::StartedPersistedBeforeTerminalResolution,
                &observer,
            )
            .is_err()
        );
        assert!(lock_unpoisoned(&observations).is_empty());
        assert_eq!(std::mem::size_of::<NoopDurableProcessBarrierObserver>(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn barrier_observer_reports_exact_committed_c4_c5_c6_order_once() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("barrier-success")?;
        let mut store = home.open_store()?;
        let (key, binding) = append_pending_process(&mut store, &request, "barrier-success")?;
        store.close()?;
        let (observer, observations) = recording_observer(&home, &key);
        let (actor, service, preclaims, initializations, releases) = spawn_barrier_test_actor(
            &home,
            &request,
            binding,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
            observer,
            || Ok(()),
        )?;

        let receipt = execute_test_process(&service, &request)?.map_err(std::io::Error::other)?;
        assert!(matches!(
            receipt.termination(),
            ProcessTerminationReceipt::Exited(status) if status.as_code() == Some(0)
        ));
        actor.shutdown()?;
        drop(service);
        assert!(!process_wide_has_owned_processes());
        assert_eq!(preclaims.load(Ordering::SeqCst), 1);
        assert_eq!(initializations.load(Ordering::SeqCst), 1);
        assert_eq!(releases.load(Ordering::SeqCst), 1);

        let snapshots = completed_boundary_snapshots(&observations)?;
        assert_eq!(snapshots.len(), 3);
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.point)
                .collect::<Vec<_>>(),
            [
                DurableProcessBarrierPoint::ClaimedBeforeSpecification,
                DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
                DurableProcessBarrierPoint::StartedPersistedBeforeTerminalResolution,
            ]
        );
        for snapshot in &snapshots {
            assert_eq!(snapshot.state, "executing");
            assert_eq!(snapshot.attempts, 1);
            assert_eq!(snapshot.claim_markers, 1);
            assert_eq!(snapshot.terminal_observations, 0);
        }
        assert_eq!(snapshots[0].spawn_permits, 0);
        assert_eq!(snapshots[0].release_authorizations, 0);
        assert_eq!(snapshots[0].started_markers, 0);
        assert!(!snapshots[0].target_started);
        assert_eq!(snapshots[1].spawn_permits, 1);
        assert_eq!(snapshots[1].release_authorizations, 1);
        assert_eq!(snapshots[1].started_markers, 0);
        assert!(
            !snapshots[1].target_started,
            "the target cannot execute before the durable started ACK"
        );
        assert_eq!(snapshots[2].spawn_permits, 1);
        assert_eq!(snapshots[2].release_authorizations, 1);
        assert_eq!(snapshots[2].started_markers, 1);
        assert!(
            home.path
                .join("release-gate-root/workspaces/run/release-sentinel")
                .try_exists()?
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn barrier_observer_omits_c5_and_c6_when_release_gate_is_closed() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("barrier-gate-closed")?;
        let mut store = home.open_store()?;
        let (key, binding) = append_pending_process(&mut store, &request, "barrier-gate-closed")?;
        store.close()?;
        let cancellation = CancellationToken::new();
        let cancellation_at_release = cancellation.clone();
        let (observer, observations) = recording_observer(&home, &key);
        let (actor, service, _, _, _) = spawn_barrier_test_actor(
            &home,
            &request,
            binding,
            cancellation,
            Arc::new(TestTimestampSource::new()),
            observer,
            move || {
                cancellation_at_release.cancel();
                Ok(())
            },
        )?;

        let receipt = execute_test_process(&service, &request)?.map_err(std::io::Error::other)?;
        assert_eq!(receipt.termination(), ProcessTerminationReceipt::Cancelled);
        actor.shutdown()?;
        drop(service);
        assert!(!process_wide_has_owned_processes());
        let snapshots = completed_boundary_snapshots(&observations)?;
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.point)
                .collect::<Vec<_>>(),
            [DurableProcessBarrierPoint::ClaimedBeforeSpecification]
        );
        assert_eq!(count_started_markers(&home)?, 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn barrier_observer_omits_c6_when_started_marker_persistence_fails() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("barrier-persist-failure")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "barrier-persist-failure")?;
        store.close()?;
        let (observer, observations) = recording_observer(&home, &key);
        let (actor, service, _, _, _) = spawn_barrier_test_actor(
            &home,
            &request,
            binding,
            CancellationToken::new(),
            Arc::new(InvalidStartedMarkerTimestampSource::new()),
            observer,
            || Ok(()),
        )?;

        assert!(execute_test_process(&service, &request)?.is_err());
        actor.shutdown()?;
        drop(service);
        assert!(!process_wide_has_owned_processes());
        let snapshots = completed_boundary_snapshots(&observations)?;
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.point)
                .collect::<Vec<_>>(),
            [
                DurableProcessBarrierPoint::ClaimedBeforeSpecification,
                DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
            ]
        );
        assert_eq!(count_started_markers(&home)?, 0);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn barrier_observer_omits_c6_when_started_persistence_ack_fails() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("barrier-ack-failure")?;
        let mut store = home.open_store()?;
        let (key, binding) = append_pending_process(&mut store, &request, "barrier-ack-failure")?;
        store.close()?;
        let cancellation = CancellationToken::new();
        let timestamp_source: Arc<dyn ProcessTimestampSource> = Arc::new(
            CancelBeforeStartedAcknowledgementTimestampSource::new(cancellation.clone()),
        );
        let (observer, observations) = recording_observer(&home, &key);
        let (actor, service, _, _, _) = spawn_barrier_test_actor(
            &home,
            &request,
            binding,
            cancellation,
            timestamp_source,
            observer,
            || Ok(()),
        )?;

        assert!(execute_test_process(&service, &request)?.is_err());
        actor.shutdown()?;
        drop(service);
        assert!(!process_wide_has_owned_processes());
        let snapshots = completed_boundary_snapshots(&observations)?;
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.point)
                .collect::<Vec<_>>(),
            [
                DurableProcessBarrierPoint::ClaimedBeforeSpecification,
                DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
            ]
        );
        assert_eq!(
            count_started_markers(&home)?,
            1,
            "the marker committed before the cancellation defeated its ACK"
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn release_reverification_failure_rejects_gate_without_authorization_marker() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("release-reverification")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "release-reverification")?;
        let preclaim_verifications = Arc::new(AtomicUsize::new(0));
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let release_verifications = Arc::new(AtomicUsize::new(0));
        let runtime = release_gate_runtime(
            &home,
            Arc::clone(&preclaim_verifications),
            Arc::clone(&runtime_initializations),
            Arc::clone(&release_verifications),
            || Err(ProcessError::InvalidSpec),
        )?;
        let launch = EnrolledProviderLaunch::injected_for_test(&request, binding, runtime)?;
        let (actor, service) = DurableProcessActor::spawn(
            store,
            launch,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        )?;
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        let receipt = context.run_process(&request)?;
        assert_eq!(
            receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        drop(context);
        assert_eq!(preclaim_verifications.load(Ordering::SeqCst), 1);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 1);
        assert_eq!(release_verifications.load(Ordering::SeqCst), 1);
        actor.shutdown()?;
        drop(service);
        assert!(!process_wide_has_owned_processes());
        assert!(
            !home
                .path
                .join("release-gate-root/workspaces/run/release-sentinel")
                .exists()
        );

        let reopened = home.open_store()?;
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::GateRejected)
        );
        reopened.close()?;
        let connection = Connection::open(home.database())?;
        let authorizations: i64 = connection.query_row(
            "SELECT count(*) FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        assert_eq!(authorizations, 0);
        Ok(())
    }

    #[test]
    fn closed_service_rejects_every_mutated_request_field_before_runtime_or_claim() -> TestResult {
        for (case_index, (mutation, expected_kind)) in [
            (
                ExactRequestMutation::Purpose,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::Executable,
                ProcessServiceErrorKind::NotEnrolled,
            ),
            (
                ExactRequestMutation::WorkingRoot,
                ProcessServiceErrorKind::OutsideRoot,
            ),
            (
                ExactRequestMutation::Arguments,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::EnvironmentOrder,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::EnvironmentLoader,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::StdinEmpty,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::StdinBytes,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::MaxOutput,
                ProcessServiceErrorKind::Denied,
            ),
            (
                ExactRequestMutation::Truncation,
                ProcessServiceErrorKind::Denied,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let home = TestHome::new()?;
            let admitted_request = exact_admission_request(None)?;
            let mut store = home.open_store()?;
            let (key, binding) = append_pending_process(
                &mut store,
                &admitted_request,
                &format!("exact-admission-{case_index}"),
            )?;
            let preclaim_verifications = Arc::new(AtomicUsize::new(0));
            let runtime_initializations = Arc::new(AtomicUsize::new(0));
            let release_verifications = Arc::new(AtomicUsize::new(0));
            let runtime = DeferredProviderRuntime::injected_with_verifiers(
                {
                    let preclaim_verifications = Arc::clone(&preclaim_verifications);
                    move || {
                        preclaim_verifications.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
                {
                    let runtime_initializations = Arc::clone(&runtime_initializations);
                    move || {
                        runtime_initializations.fetch_add(1, Ordering::SeqCst);
                        Err(ProcessError::InvalidSpec)
                    }
                },
                {
                    let release_verifications = Arc::clone(&release_verifications);
                    move || {
                        release_verifications.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            );
            let launch =
                EnrolledProviderLaunch::injected_for_test(&admitted_request, binding, runtime)?;
            let (actor, service) = DurableProcessActor::spawn(
                store,
                launch,
                CancellationToken::new(),
                Arc::new(TestTimestampSource::new()),
            )?;
            let mutated_request = exact_admission_request(Some(mutation))?;
            let now = Instant::now();
            let clock = FixedClock(now);
            let watchdog = FixedWatchdog;
            let effects = RejectingEffectService::new()?;
            let mut events = RejectingEventSink::new()?;
            let mut context = ExecutionContext::new(
                &service,
                &clock,
                &watchdog,
                &effects,
                &mut events,
                WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
                now + Duration::from_secs(60),
            );

            let observed_kind = match context.run_process(&mutated_request) {
                Ok(_) => {
                    return Err(std::io::Error::other(format!(
                        "{mutation:?} mutation was admitted"
                    ))
                    .into());
                }
                Err(error) => error.kind(),
            };
            assert_eq!(observed_kind, expected_kind, "{mutation:?}");
            drop(context);
            assert_eq!(preclaim_verifications.load(Ordering::SeqCst), 0);
            assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
            assert_eq!(release_verifications.load(Ordering::SeqCst), 0);
            actor.shutdown()?;
            drop(service);

            let reopened = home.open_store()?;
            let pending = reopened.pending_effects(10)?;
            assert_eq!(pending.len(), 1, "{mutation:?}");
            assert_eq!(pending[0].idempotency_key(), key, "{mutation:?}");
            assert_eq!(pending[0].attempts(), 0, "{mutation:?}");
            assert!(pending[0].execution_identity().is_none(), "{mutation:?}");
            assert!(
                reopened.attempt_history(&key, 10)?.is_empty(),
                "{mutation:?}"
            );
            reopened.close()?;
        }
        Ok(())
    }

    /// Proves the spawn-lease-leak fix: a binding mismatch caught before the
    /// worker thread is ever started must still close the store, releasing
    /// the boundary's writer lease. Without the fix, `store` is dropped
    /// uncleanly on this early-return path and `RuntimeStore::drop` forgets
    /// the boundary reference forever (see its `Drop` impl), so a subsequent
    /// `RuntimeStore::open` on the same boundary would fail with
    /// `WriterLeased` instead of succeeding.
    #[test]
    fn spawn_releases_writer_lease_when_binding_mismatches_request() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("preflight")?;
        let mismatched_request = process_request("mismatched-argument")?;
        let mut store = home.open_store()?;
        let (_key, binding) = append_pending_process(&mut store, &request, "durable-preflight")?;
        let cancellation = CancellationToken::new();
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let runtime = DeferredProviderRuntime::injected({
            let runtime_initializations = Arc::clone(&runtime_initializations);
            move || {
                runtime_initializations.fetch_add(1, Ordering::SeqCst);
                Err(ProcessError::InvalidSpec)
            }
        });
        let launch = EnrolledProviderLaunch::mismatched_for_actor_test(
            &request,
            &mismatched_request,
            binding,
            runtime,
        )?;

        let result = DurableProcessActor::spawn(store, launch, cancellation, timestamp_source);
        assert!(matches!(
            result,
            Err(DurableProcessBuildError::InvalidEnrollment)
        ));
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);

        // The writer lease must have been released by the best-effort close
        // on the mismatch path: reopening the same boundary must succeed
        // rather than observe `WriterLeased`.
        let reopened = home.open_store()?;
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn spawn_surfaces_close_failure_before_binding_mismatch() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("preflight")?;
        let mismatched_request = process_request("mismatched-argument")?;
        let mut store = home.open_store()?;
        let (_key, binding) =
            append_pending_process(&mut store, &request, "durable-preflight-close")?;

        // Pin an older WAL snapshot, then create frames it cannot checkpoint.
        // The early binding error must not conceal that writer handoff failed.
        let observer = Connection::open(home.database())?;
        observer.execute_batch("BEGIN; SELECT count(*) FROM journal;")?;
        let later_request = process_request("later-effect")?;
        let _ =
            append_pending_process(&mut store, &later_request, "durable-preflight-close-later")?;
        let runtime = DeferredProviderRuntime::injected(|| Err(ProcessError::InvalidSpec));
        let launch = EnrolledProviderLaunch::mismatched_for_actor_test(
            &request,
            &mismatched_request,
            binding,
            runtime,
        )?;
        let result = DurableProcessActor::spawn(
            store,
            launch,
            CancellationToken::new(),
            Arc::new(TestTimestampSource::new()),
        );
        assert!(matches!(
            result,
            Err(DurableProcessBuildError::Store(
                RuntimeStoreError::CloseIncomplete
            ))
        ));
        observer.execute_batch("ROLLBACK")?;
        Ok(())
    }

    struct CancelBeforeDelegatingExecute {
        durable: Arc<DurableProcessService>,
        execute_calls: AtomicUsize,
    }

    impl Cancellation for CancelBeforeDelegatingExecute {
        fn is_cancelled(&self) -> bool {
            self.durable.is_cancelled()
        }
    }

    impl ProcessService for CancelBeforeDelegatingExecute {
        fn finish_preflight(
            &self,
            request: &ProcessRequest,
            preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            let _ = preflight.bind(self, request);
            Err(self.durable.errors.unavailable.clone())
        }

        fn execute(
            &self,
            request: &ProcessRequest,
            budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            self.durable.cancel();
            self.durable.execute(request, budget)
        }
    }

    #[test]
    fn execute_preflight_prefers_cancellation_when_cancelled_and_expired() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("execute-preflight")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "execute-preflight-precedence")?;
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, durable, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &request,
            CancellationToken::new(),
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        let durable = Arc::new(durable);
        let service = CancelBeforeDelegatingExecute {
            durable: Arc::clone(&durable),
            execute_calls: AtomicUsize::new(0),
        };
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let clock = ExpiringClock::new(now, deadline);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            deadline,
        );

        let receipt = context.run_process(&request)?;
        assert_eq!(receipt.termination(), ProcessTerminationReceipt::Cancelled);
        drop(context);
        assert_eq!(service.execute_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        drop(service);
        drop(durable);

        let reopened = home.open_store()?;
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::Cancelled)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn post_commit_resolution_error_is_proven_by_exact_readback() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("post-commit-readback")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "post-commit-resolution-readback")?;
        store.inject_process_resolution_acknowledgement_loss();
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, service, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &request,
            CancellationToken::new(),
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        assert!(service.cancel());
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        let receipt = context.run_process(&request)?;
        assert_eq!(receipt.termination(), ProcessTerminationReceipt::Cancelled);
        drop(context);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        drop(service);

        let reopened = home.open_store()?;
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::Cancelled)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn committed_preflight_with_close_failure_is_outcome_indeterminate() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("close-failure")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "preflight-close-failure")?;
        let observer = Connection::open(home.database())?;
        observer.execute_batch("BEGIN; SELECT count(*) FROM journal;")?;
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, service, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &request,
            CancellationToken::new(),
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        assert!(service.cancel());
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        assert!(
            context.run_process(&request).is_err_and(|error| {
                error.kind() == ProcessServiceErrorKind::OutcomeIndeterminate
            })
        );
        drop(context);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        assert!(matches!(
            actor.shutdown(),
            Err(RuntimeStoreError::CloseIncomplete)
        ));
        drop(service);

        observer.execute_batch("ROLLBACK")?;
        let (state, attempts): (String, i64) = observer.query_row(
            "SELECT state, attempts FROM outbox WHERE idempotency_key = ?1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(state, "failed");
        assert_eq!(attempts, 1);
        let observations: i64 = observer.query_row(
            "SELECT count(*) FROM outbox_attempt_observation WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        assert_eq!(observations, 1);
        Ok(())
    }

    struct DroppingReplyProcessService {
        durable: Arc<DurableProcessService>,
        execute_calls: AtomicUsize,
    }

    impl Cancellation for DroppingReplyProcessService {
        fn is_cancelled(&self) -> bool {
            self.durable.is_cancelled()
        }
    }

    impl ProcessService for DroppingReplyProcessService {
        fn finish_preflight(
            &self,
            request: &ProcessRequest,
            preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            let PreparedProcessCall { mut consumed, call } = self.durable.prepare_call(request)?;
            let ClosedActorCall { binding, request } = call;
            let Some(preflight) = preflight.bind(self, &request) else {
                return Err(self.durable.errors.denied.clone());
            };
            let (reply, abandoned_reply) = sync_channel(1);
            drop(abandoned_reply);
            self.durable
                .sender
                .try_send(ActorMessage::FinishPreflight(PreflightMessage {
                    binding,
                    request,
                    preflight,
                    reply,
                }))
                .map_err(|_| self.durable.errors.unavailable.clone())?;
            consumed.disarm();
            Err(self.durable.errors.indeterminate.clone())
        }

        fn execute(
            &self,
            _request: &ProcessRequest,
            _budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            Err(self.durable.errors.unavailable.clone())
        }
    }

    #[test]
    fn dropped_preflight_reply_is_never_acknowledged_as_success() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("dropped-reply")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "dropped-preflight-reply")?;
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, durable, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &request,
            CancellationToken::new(),
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        let durable = Arc::new(durable);
        assert!(durable.cancel());
        let service = DroppingReplyProcessService {
            durable: Arc::clone(&durable),
            execute_calls: AtomicUsize::new(0),
        };
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &service,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        assert!(
            context.run_process(&request).is_err_and(|error| {
                error.kind() == ProcessServiceErrorKind::OutcomeIndeterminate
            })
        );
        drop(context);
        assert_eq!(service.execute_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        assert!(durable.faulted.load(Ordering::Acquire));
        drop(service);
        drop(durable);

        let reopened = home.open_store()?;
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempt(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        reopened.close()?;
        Ok(())
    }

    struct MisdirectingProcessService {
        durable: Arc<DurableProcessService>,
        forwarded_request: ProcessRequest,
        execute_calls: AtomicUsize,
    }

    impl Cancellation for MisdirectingProcessService {
        fn is_cancelled(&self) -> bool {
            self.durable.is_cancelled()
        }
    }

    impl ProcessService for MisdirectingProcessService {
        fn finish_preflight(
            &self,
            _request: &ProcessRequest,
            preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            self.durable
                .finish_preflight(&self.forwarded_request, preflight)
        }

        fn execute(
            &self,
            request: &ProcessRequest,
            budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            self.durable.execute(request, budget)
        }
    }

    #[repr(transparent)]
    struct TransparentForwardingProcessService(DurableProcessService);

    impl Cancellation for TransparentForwardingProcessService {
        fn is_cancelled(&self) -> bool {
            self.0.is_cancelled()
        }
    }

    impl ProcessService for TransparentForwardingProcessService {
        fn finish_preflight(
            &self,
            request: &ProcessRequest,
            preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            self.0.finish_preflight(request, preflight)
        }

        fn execute(
            &self,
            request: &ProcessRequest,
            budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            self.0.execute(request, budget)
        }
    }

    #[test]
    fn transparent_wrapper_cannot_forward_preflight_to_inner_durable_service() -> TestResult {
        let home = TestHome::new()?;
        let request = process_request("transparent-forwarding")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &request, "transparent-forwarding")?;
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, durable, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &request,
            CancellationToken::new(),
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        assert!(durable.cancel());
        let forwarding = TransparentForwardingProcessService(durable);
        assert_eq!(
            std::ptr::from_ref(&forwarding).cast::<()>(),
            std::ptr::from_ref(&forwarding.0).cast::<()>(),
            "the regression must exercise an identical data address"
        );
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &forwarding,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        assert!(
            context
                .run_process(&request)
                .is_err_and(|error| error.kind() == ProcessServiceErrorKind::Denied)
        );
        drop(context);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);
        actor.shutdown()?;
        drop(forwarding);

        let reopened = home.open_store()?;
        let pending = reopened.pending_effects(10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].idempotency_key(), key);
        assert_eq!(pending[0].attempts(), 0);
        assert!(pending[0].execution_identity().is_none());
        assert!(reopened.attempt_history(&key, 10)?.is_empty());
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn mismatched_preflight_token_is_rejected_before_claim_or_launch() -> TestResult {
        let home = TestHome::new()?;
        let source_request = process_request("source-request")?;
        let durable_request = process_request("durable-request")?;
        let mut store = home.open_store()?;
        let (key, binding) =
            append_pending_process(&mut store, &durable_request, "mismatched-preflight")?;
        let cancellation = CancellationToken::new();
        let runtime_initializations = Arc::new(AtomicUsize::new(0));
        let timestamp_source: Arc<dyn ProcessTimestampSource> =
            Arc::new(TestTimestampSource::new());
        let (actor, durable, _, _) = spawn_preflight_test_actor(
            store,
            binding,
            &durable_request,
            cancellation,
            timestamp_source,
            Arc::clone(&runtime_initializations),
        )?;
        let durable = Arc::new(durable);
        assert!(durable.cancel());
        let forwarding = MisdirectingProcessService {
            durable: Arc::clone(&durable),
            forwarded_request: durable_request,
            execute_calls: AtomicUsize::new(0),
        };
        let now = Instant::now();
        let clock = FixedClock(now);
        let watchdog = FixedWatchdog;
        let effects = RejectingEffectService::new()?;
        let mut events = RejectingEventSink::new()?;
        let mut context = ExecutionContext::new(
            &forwarding,
            &clock,
            &watchdog,
            &effects,
            &mut events,
            WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
            now + Duration::from_secs(60),
        );

        assert!(
            context
                .run_process(&source_request)
                .is_err_and(|error| { error.kind() == ProcessServiceErrorKind::Denied })
        );
        drop(context);
        assert_eq!(forwarding.execute_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime_initializations.load(Ordering::SeqCst), 0);

        type RetryResult<T> = Result<T, String>;
        let retry_request = process_request("durable-request")?;
        let barrier = Barrier::new(3);
        let retry_worker = |worker: &'static str| -> RetryResult<ProcessServiceErrorKind> {
            barrier.wait();
            let now = Instant::now();
            let clock = FixedClock(now);
            let watchdog = FixedWatchdog;
            let effects = RejectingEffectService::new().map_err(|error| error.to_string())?;
            let mut events = RejectingEventSink::new().map_err(|error| error.to_string())?;
            let mut context = ExecutionContext::new(
                durable.as_ref(),
                &clock,
                &watchdog,
                &effects,
                &mut events,
                WorkerIdentity::new("mission-1", "phase-1", worker)
                    .map_err(|error| error.to_string())?,
                now + Duration::from_secs(60),
            );
            match context.run_process(&retry_request) {
                Ok(_) => Err("a consumed durable service acknowledged a retry".to_owned()),
                Err(error) => Ok(error.kind()),
            }
        };
        let retry_kinds = thread::scope(|scope| -> RetryResult<[ProcessServiceErrorKind; 2]> {
            let first = scope.spawn(|| retry_worker("retry-worker-1"));
            let second = scope.spawn(|| retry_worker("retry-worker-2"));
            barrier.wait();
            Ok([
                first
                    .join()
                    .map_err(|_| "first retry worker panicked".to_owned())??,
                second
                    .join()
                    .map_err(|_| "second retry worker panicked".to_owned())??,
            ])
        })?;
        assert_eq!(
            retry_kinds,
            [
                ProcessServiceErrorKind::Unavailable,
                ProcessServiceErrorKind::Unavailable,
            ]
        );
        actor.shutdown()?;
        drop(forwarding);
        drop(durable);

        let reopened = home.open_store()?;
        let pending = reopened.pending_effects(10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].idempotency_key(), key);
        assert_eq!(pending[0].attempts(), 0);
        assert!(pending[0].execution_identity().is_none());
        assert!(reopened.attempt_history(&key, 10)?.is_empty());
        reopened.close()?;
        Ok(())
    }
}
