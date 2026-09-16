//! Application-owned durable process composition for the private Rust pilot.

use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

/// Owner-controlled final admission for a durable pilot process release.
///
/// Implementations must serialize this decision with durable cancellation
/// acceptance. Refusing a permit leaves the blocked launcher unstarted; an
/// issued permit keeps that serialization held through the release decision.
pub trait RustPilotProcessReleaseAdmission: Send + Sync {
    fn admit_release(&self) -> Option<Box<dyn RustPilotProcessReleasePermit + '_>>;
}

/// Linear permit retained until the blocked launcher is released or rejected.
pub trait RustPilotProcessReleasePermit {}

#[cfg(feature = "test-support")]
use std::time::Duration;

use orchestrator_core::MissionId;
use orchestrator_exec::{
    BoundProcessPreflight, ProcessBudget, ProcessPurpose, ProcessReceipt, ProcessRequest,
    ServiceContractError,
};
use orchestrator_process::{
    CancellationToken, ExactProcessGroupAbsence, ExecutableFileAttestation, KernelProcessIdentity,
    MAX_ATTESTED_EXECUTABLE_BYTES, ProcessError, ProcessNotStartedReason, ProcessOutputSender,
    ProcessOwnedDirectory, ProcessSupervisor, ProductionProcessLaunchAuthority,
    RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    recover_orphaned_process_directories,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    EffectOperationSlot, JournalIntent, OutboxIntent, ProductionWriterAuthority, RuntimeStore,
    RuntimeStoreError, StorageActorAuthority,
    capability::CapabilityRoot,
    durable_process_service::{
        DurableProcessActor, DurableProcessBuildError, DurableProcessOutcomeObservation,
        DurableProcessRecoveryError, DurableProcessService, ProcessTimestampSource,
        RecoveredProcessDisposition, reconcile_recovered_process_attempt,
    },
    fs_util::{atomic_replace_private, open_dir_path_nofollow, sync_dir},
    runtime_home::ProductionBoundary,
    runtime_store::{
        OutboxEffectKind, OutboxState, PrivateProcessLedgerBoundary, PrivateProcessLedgerStore,
        ProcessEffectBinding,
    },
};

/// Fixed outer journal containing exact process-owned directory records.
pub const RUST_PILOT_PROCESS_OWNERSHIP_FILE: &str = "orchestrator.process-ownership.v1";

const OWNERSHIP_MAGIC: &[u8; 8] = b"NANPJO01";
const MAX_OWNERSHIP_RECORDS: usize = 8;
const MAX_OWNERSHIP_BYTES: usize = 4096;
const MAX_RECORDED_PROCESS_IDENTITIES: usize = 1000;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Exact logical identity of one pilot process attempt.
pub struct RustPilotProcessAttempt {
    mission_id: MissionId,
    phase_id: String,
    logical_attempt: u32,
}

impl RustPilotProcessAttempt {
    /// Creates a bounded mission/phase/attempt binding.
    pub fn new(
        mission_id: MissionId,
        phase_id: impl Into<String>,
        logical_attempt: u32,
    ) -> Result<Self, RustPilotProcessError> {
        let phase_id = phase_id.into();
        EffectOperationSlot::new(&phase_id)?;
        if logical_attempt == 0 {
            return Err(RustPilotProcessError::InvalidAttempt);
        }
        Ok(Self {
            mission_id,
            phase_id,
            logical_attempt,
        })
    }
}

/// Open, canonical, digest-pinned authority for one external executable.
pub struct RustPilotProcessExecutable {
    proof: Arc<ExternalExecutableProof>,
}

impl RustPilotProcessExecutable {
    /// Opens an already canonical executable path and pins its exact metadata and SHA-256.
    pub fn open(canonical_path: &Path) -> Result<Self, RustPilotProcessError> {
        if !canonical_path.is_absolute()
            || std::fs::canonicalize(canonical_path)
                .map_err(|_| RustPilotProcessError::ExecutableAdmission)?
                != canonical_path
        {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        let file = File::from(
            rustix::fs::open(
                canonical_path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
        );
        let metadata = file
            .metadata()
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?;
        let snapshot = ExecutableMetadata::admit(&metadata)?;
        let digest = hash_file(&file, snapshot)?;
        let attestation = ExecutableFileAttestation::new(snapshot.length, digest);
        let logical_id = logical_executable_id(canonical_path, snapshot, digest)?;
        let proof = Arc::new(ExternalExecutableProof {
            canonical_path: canonical_path.to_path_buf(),
            file,
            snapshot,
            attestation,
            logical_id,
        });
        proof.verify()?;
        Ok(Self { proof })
    }

    /// Opaque logical ID that the exact [`ProcessRequest`] must name.
    #[must_use]
    pub fn logical_id(&self) -> &str {
        &self.proof.logical_id
    }

    /// Exact admitted byte length and SHA-256.
    #[must_use]
    pub fn attestation(&self) -> ExecutableFileAttestation {
        self.proof.attestation
    }
}

/// Result of reopening one logical attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustPilotProcessRecovery {
    /// No release was possible; the durable attempt is now terminally not-started.
    RecoveredBeforeStartNotStarted,
    /// Release may have occurred; cleanup is exact but the target outcome remains unknown.
    ReleasedLostOutcomeUncertain,
    /// The logical attempt already had a durable terminal outcome and was not dispatched again.
    PreviouslyResolved,
}

/// Either a new dispatch authority or a no-redispatch recovery result.
pub enum RustPilotProcessOpen {
    Ready(Box<RustPilotProcessSession>),
    Recovered(RustPilotProcessRecovery),
}

/// Read-back evidence for a process attempt reconciled from the durable
/// ledger. A cleanup-only launcher can carry an absence witness while
/// `target_released` remains false.
pub struct RustPilotRecoveredProcessEvidence {
    target_released: bool,
    group_absence: Option<ExactProcessGroupAbsence>,
}

impl RustPilotRecoveredProcessEvidence {
    /// True only when a durable started receipt confirmed target release.
    #[must_use]
    pub const fn target_released(&self) -> bool {
        self.target_released
    }

    /// Exact current absence of the owned launcher's recorded process group.
    #[must_use]
    pub const fn group_absence(&self) -> Option<&ExactProcessGroupAbsence> {
        self.group_absence.as_ref()
    }
}

/// Truthful durable receipt plus independently classified execution and cleanup evidence.
pub struct RustPilotProcessExecution {
    not_started_reason: Option<ProcessNotStartedReason>,
    receipt: ProcessReceipt,
    post_release_identity: Option<KernelProcessIdentity>,
    group_absence: Option<ExactProcessGroupAbsence>,
}

impl RustPilotProcessExecution {
    /// Reason committed by the durable process owner when the target never started.
    /// Missing identity alone does not establish this classification.
    #[must_use]
    pub const fn not_started_reason(&self) -> Option<ProcessNotStartedReason> {
        self.not_started_reason
    }

    #[must_use]
    pub const fn receipt(&self) -> &ProcessReceipt {
        &self.receipt
    }

    #[must_use]
    pub const fn post_release_identity(&self) -> Option<&KernelProcessIdentity> {
        self.post_release_identity.as_ref()
    }

    #[must_use]
    pub const fn group_absence(&self) -> Option<&ExactProcessGroupAbsence> {
        self.group_absence.as_ref()
    }
}

/// One-use process session retaining the pilot writer lease through actor shutdown.
pub struct RustPilotProcessSession {
    actor: Option<DurableProcessActor>,
    service: Option<DurableProcessService>,
    boundary: Arc<ProductionBoundary>,
}

impl RustPilotProcessSession {
    /// Reads real cleanup and release evidence for an already-admitted exact
    /// attempt. This performs no claim, release, provider dispatch, or retry.
    pub fn recovered_metric_evidence(
        writer: &ProductionWriterAuthority,
        attempt: RustPilotProcessAttempt,
        request: &ProcessRequest,
    ) -> Result<RustPilotRecoveredProcessEvidence, RustPilotProcessError> {
        let boundary = writer.boundary();
        boundary
            .verify()
            .map_err(|_| RustPilotProcessError::WrongWriterOrigin)?;
        if !boundary.is_rust_pilot_writer() {
            return Err(RustPilotProcessError::WrongWriterOrigin);
        }
        let (_idempotency_key, binding) = prepare_effect(&attempt, request)?;
        let store = RuntimeStore::open_private(
            PrivateProcessLedgerBoundary::new(Arc::clone(&boundary)),
            StorageActorAuthority::new(),
        )?;
        let evidence = store.process_metric_identity(&binding, request);
        let close = store.close();
        let (identity, target_released) = match (evidence, close) {
            (Ok(evidence), Ok(())) => evidence,
            (Err(error), _) | (Ok(_), Err(error)) => return Err(error.into()),
        };
        let group_absence = match identity {
            Some(identity) => match inspect_recorded_process_identity(
                identity.pid(),
                identity.process_group_id(),
                identity.process_start_identity(),
            )
            .map_err(|_| RustPilotProcessError::CleanupUnproven)?
            {
                RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Some(absence),
                RecordedProcessIdentityStatus::ExactLive
                | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
                    return Err(RustPilotProcessError::CleanupUnproven);
                }
            },
            None => None,
        };
        if target_released && group_absence.is_none() {
            return Err(RustPilotProcessError::CleanupUnproven);
        }
        Ok(RustPilotRecoveredProcessEvidence {
            target_released,
            group_absence,
        })
    }

    /// Opens or reconciles one exact process attempt under a borrowed Rust pilot writer.
    ///
    /// The caller keeps the writer for surrounding mission journal writes. A ready
    /// session retains the writer's genuine lease-bearing boundary through actor shutdown.
    pub fn open(
        writer: &ProductionWriterAuthority,
        attempt: RustPilotProcessAttempt,
        request: &ProcessRequest,
        executable: RustPilotProcessExecutable,
        cwd_relative: PathBuf,
    ) -> Result<RustPilotProcessOpen, RustPilotProcessError> {
        Self::open_with_cancellation(
            writer,
            attempt,
            request,
            executable,
            cwd_relative,
            CancellationToken::new(),
        )
    }

    /// Opens or reconciles one exact process attempt with shared cancellation.
    pub fn open_with_cancellation(
        writer: &ProductionWriterAuthority,
        attempt: RustPilotProcessAttempt,
        request: &ProcessRequest,
        executable: RustPilotProcessExecutable,
        cwd_relative: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<RustPilotProcessOpen, RustPilotProcessError> {
        Self::open_with_cancellation_and_admission(
            writer,
            attempt,
            request,
            executable,
            cwd_relative,
            cancellation,
            None,
        )
    }

    /// Opens an exact process attempt whose final release is serialized by its mission owner.
    pub fn open_with_cancellation_and_admission(
        writer: &ProductionWriterAuthority,
        attempt: RustPilotProcessAttempt,
        request: &ProcessRequest,
        executable: RustPilotProcessExecutable,
        cwd_relative: PathBuf,
        cancellation: CancellationToken,
        release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
    ) -> Result<RustPilotProcessOpen, RustPilotProcessError> {
        let boundary = writer.boundary();
        boundary
            .verify()
            .map_err(|_| RustPilotProcessError::WrongWriterOrigin)?;
        if !boundary.is_rust_pilot_writer() {
            return Err(RustPilotProcessError::WrongWriterOrigin);
        }
        if !matches!(
            request.purpose(),
            ProcessPurpose::ProviderWorker | ProcessPurpose::Verification
        ) {
            return Err(RustPilotProcessError::UnsupportedPurpose);
        }
        executable.proof.verify()?;
        let cwd = Arc::new(PrivateCwdProof::open(Arc::clone(&boundary), cwd_relative)?);
        if request.executable_id() != executable.logical_id()
            || request.working_root() != cwd.canonical_path
        {
            return Err(RustPilotProcessError::RequestMismatch);
        }

        let mut store = RuntimeStore::open_private(
            PrivateProcessLedgerBoundary::new(Arc::clone(&boundary)),
            StorageActorAuthority::new(),
        )?;
        let prepared = prepare_effect(&attempt, request);
        let (idempotency_key, binding) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return close_store_with(store, error),
        };
        if let Err(error) = require_no_other_active_process(&store, &idempotency_key) {
            return close_store_with(store, error);
        }
        let effect = match build_effect(&attempt, request) {
            Ok(effect) => effect,
            Err(error) => return close_store_with(store, error),
        };
        let transition_id = transition_id(&attempt);
        let committed_at_utc = match store.transition_committed_at_utc(&transition_id) {
            Ok(Some(committed_at_utc)) => committed_at_utc,
            Ok(None) => now_utc(),
            Err(error) => return close_store_with(store, error.into()),
        };
        let transition = match build_transition(&attempt, effect, transition_id, committed_at_utc) {
            Ok(transition) => transition,
            Err(error) => return close_store_with(store, error),
        };
        if let Err(error) = store.append(&transition) {
            return close_store_with(store, error.into());
        }
        let snapshot = match store.exact_outbox_snapshot(&idempotency_key) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return close_store_with(store, RustPilotProcessError::ForeignProcessState),
            Err(error) => return close_store_with(store, error.into()),
        };
        match snapshot.effect().state() {
            OutboxState::Pending => {
                if let Err(error) = require_no_recovery_effects(&store) {
                    return close_store_with(store, error);
                }
                if let Err(error) = require_all_recorded_groups_absent(&store, None) {
                    return close_store_with(store, error);
                }
                if let Err(error) = recover_owned_directories(&boundary) {
                    return close_store_with(store, error);
                }
            }
            OutboxState::Executing | OutboxState::Uncertain => {
                let current_identity = snapshot.effect().execution_identity();
                if let Err(error) = require_only_current_recovery(&store, &idempotency_key) {
                    return close_store_with(store, error);
                }
                if let Err(error) = require_all_recorded_groups_absent(&store, current_identity) {
                    return close_store_with(store, error);
                }
                let observed_at = now_utc();
                let disposition = match reconcile_recovered_process_attempt(
                    &mut store,
                    &binding,
                    request,
                    &observed_at,
                ) {
                    Ok(disposition) => disposition,
                    Err(error) => return close_store_with(store, error.into()),
                };
                let recovery = match recovery_classification(disposition) {
                    Ok(recovery) => recovery,
                    Err(error) => return close_store_with(store, error),
                };
                if let Err(error) = require_all_recorded_groups_absent(&store, None) {
                    return close_store_with(store, error);
                }
                if let Err(error) = recover_owned_directories(&boundary) {
                    return close_store_with(store, error);
                }
                store.close()?;
                return Ok(RustPilotProcessOpen::Recovered(recovery));
            }
            OutboxState::Succeeded | OutboxState::Failed => {
                if let Err(error) = require_all_recorded_groups_absent(&store, None) {
                    return close_store_with(store, error);
                }
                if let Err(error) = recover_owned_directories(&boundary) {
                    return close_store_with(store, error);
                }
                store.close()?;
                return Ok(RustPilotProcessOpen::Recovered(
                    RustPilotProcessRecovery::PreviouslyResolved,
                ));
            }
        }

        let launch = match application_launch(
            match copy_request(request) {
                Ok(request) => request,
                Err(error) => return close_store_with(store, error.into()),
            },
            binding,
            executable.proof,
            cwd,
            Arc::clone(&boundary),
        ) {
            Ok(launch) => launch,
            Err(error) => return close_store_with(store, error),
        };
        let timestamp_source: Arc<dyn ProcessTimestampSource> = Arc::new(SystemTimestampSource);
        let (actor, service) = spawn_process_actor(
            store,
            launch,
            timestamp_source,
            cancellation,
            release_admission,
        )?;
        Ok(RustPilotProcessOpen::Ready(Box::new(Self {
            actor: Some(actor),
            service: Some(service),
            boundary,
        })))
    }

    /// Executes the one exact admitted request and cleanly closes all retained stores.
    pub fn execute(
        self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<RustPilotProcessExecution, RustPilotProcessError> {
        self.execute_inner(request, budget, None)
    }

    /// Executes with bounded best-effort live stdout and stderr delivery.
    pub fn execute_with_output(
        self,
        request: &ProcessRequest,
        budget: ProcessBudget,
        output: ProcessOutputSender,
    ) -> Result<RustPilotProcessExecution, RustPilotProcessError> {
        self.execute_inner(request, budget, Some(output))
    }

    fn execute_inner(
        mut self,
        request: &ProcessRequest,
        budget: ProcessBudget,
        output: Option<ProcessOutputSender>,
    ) -> Result<RustPilotProcessExecution, RustPilotProcessError> {
        let service = self
            .service
            .take()
            .ok_or(RustPilotProcessError::SessionClosed)?;
        let service = match output {
            Some(output) => service.with_output_sender(output),
            None => service,
        };
        if validate_existing_ownership_file(&self.boundary).is_err() {
            drop(service);
            self.shutdown_actor()?;
            return Err(RustPilotProcessError::OwnershipJournal);
        }
        let receipt = service.dispatch_once(request, budget);
        let observation = service.take_outcome_observation();
        drop(service);
        self.shutdown_actor()?;
        let receipt = receipt.map_err(|_| RustPilotProcessError::DispatchFailed)?;
        let not_started_reason = observation
            .as_ref()
            .and_then(|outcome| outcome.not_started_reason);
        let (identity, absence) = exact_execution_and_cleanup_evidence(observation)?;
        recover_owned_directories(&self.boundary)?;
        Ok(RustPilotProcessExecution {
            not_started_reason,
            receipt,
            post_release_identity: identity,
            group_absence: absence,
        })
    }

    /// Terminally records a request-bound cancellation or expired deadline without launching.
    pub fn finish_preflight(
        mut self,
        request: &ProcessRequest,
        preflight: BoundProcessPreflight,
    ) -> Result<(), RustPilotProcessError> {
        let service = self
            .service
            .take()
            .ok_or(RustPilotProcessError::SessionClosed)?;
        let result = service.finish_bound_preflight(request, preflight);
        drop(service);
        self.shutdown_actor()?;
        result.map_err(|_| RustPilotProcessError::DispatchFailed)?;
        recover_owned_directories(&self.boundary)
    }

    /// Closes an unused session without dispatching its admitted attempt.
    pub fn close(mut self) -> Result<(), RustPilotProcessError> {
        self.service.take();
        self.shutdown_actor()?;
        recover_owned_directories(&self.boundary)?;
        Ok(())
    }

    fn shutdown_actor(&mut self) -> Result<(), RustPilotProcessError> {
        if let Some(actor) = self.actor.take() {
            actor.shutdown_without_cancelling()?;
        }
        Ok(())
    }
}

fn spawn_process_actor(
    store: PrivateProcessLedgerStore,
    launch: crate::durable_process_service::EnrolledProviderLaunch,
    timestamp_source: Arc<dyn ProcessTimestampSource>,
    cancellation: CancellationToken,
    release_admission: Option<Arc<dyn RustPilotProcessReleaseAdmission>>,
) -> Result<(DurableProcessActor, DurableProcessService), DurableProcessBuildError> {
    #[cfg(feature = "test-support")]
    if let Some(observer) = PilotProcessTestObserver::from_environment() {
        return DurableProcessActor::spawn_with_observer_and_release_admission(
            store,
            launch,
            cancellation,
            timestamp_source,
            observer,
            release_admission,
        );
    }
    DurableProcessActor::spawn_with_release_admission(
        store,
        launch,
        cancellation,
        timestamp_source,
        release_admission,
    )
}

#[cfg(feature = "test-support")]
struct PilotProcessTestObserver {
    target: crate::durable_process_service::DurableProcessBarrierPoint,
    ready: PathBuf,
    release: Option<PathBuf>,
}

#[cfg(feature = "test-support")]
impl PilotProcessTestObserver {
    fn from_environment() -> Option<Self> {
        use crate::durable_process_service::DurableProcessBarrierPoint;

        let target = match std::env::var("NANIKA_PILOT_PROCESS_TEST_BARRIER")
            .ok()?
            .as_str()
        {
            "claimed" => DurableProcessBarrierPoint::ClaimedBeforeSpecification,
            "released" => DurableProcessBarrierPoint::ReleaseAuthorizedBeforeStartedReceipt,
            "started" => DurableProcessBarrierPoint::StartedPersistedBeforeTerminalResolution,
            _ => return None,
        };
        Some(Self {
            target,
            ready: PathBuf::from(std::env::var_os("NANIKA_PILOT_PROCESS_TEST_READY")?),
            release: std::env::var_os("NANIKA_PILOT_PROCESS_TEST_RELEASE").map(PathBuf::from),
        })
    }
}

#[cfg(feature = "test-support")]
impl crate::durable_process_service::DurableProcessBarrierObserver for PilotProcessTestObserver {
    fn observe(&self, point: crate::durable_process_service::DurableProcessBarrierPoint) {
        if point != self.target {
            return;
        }
        use std::os::unix::fs::OpenOptionsExt;

        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.ready)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(b"ready\n")?;
                file.sync_all()
            });
        if result.is_err() {
            return;
        }
        let Some(release) = self.release.as_ref() else {
            loop {
                std::thread::park();
            }
        };
        while !release.is_file() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RustPilotProcessSession {
    fn drop(&mut self) {
        self.service.take();
        if let Some(actor) = self.actor.take() {
            let _ = actor.shutdown();
        }
    }
}

/// Failures at the private Rust-pilot process composition boundary.
#[derive(Debug, Error)]
pub enum RustPilotProcessError {
    #[error("process session requires a RustPilotRuntimeHome writer")]
    WrongWriterOrigin,
    #[error("process attempt must be positive")]
    InvalidAttempt,
    #[error("process purpose is not admitted by the Rust pilot")]
    UnsupportedPurpose,
    #[error("external executable admission failed")]
    ExecutableAdmission,
    #[error("private process CWD admission failed")]
    CwdAdmission,
    #[error("exact process request does not match its executable or CWD")]
    RequestMismatch,
    #[error("process ownership journal is unsafe or malformed")]
    OwnershipJournal,
    #[error("another or foreign durable process state is present")]
    ForeignProcessState,
    #[error("durable process recovery is unresolved")]
    RecoveryUnresolved,
    #[error("durable process dispatch failed")]
    DispatchFailed,
    #[error("durable process outcome lacks exact cleanup evidence")]
    CleanupUnproven,
    #[error("process session is already closed")]
    SessionClosed,
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error(transparent)]
    Contract(#[from] ServiceContractError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("durable process actor construction failed")]
    ActorBuild,
    #[error("durable process recovery failed")]
    Recovery,
}

impl From<DurableProcessBuildError> for RustPilotProcessError {
    fn from(_error: DurableProcessBuildError) -> Self {
        Self::ActorBuild
    }
}

impl From<DurableProcessRecoveryError> for RustPilotProcessError {
    fn from(_error: DurableProcessRecoveryError) -> Self {
        Self::Recovery
    }
}

struct SystemTimestampSource;

impl ProcessTimestampSource for SystemTimestampSource {
    fn now_utc(&self) -> String {
        now_utc()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ExecutableMetadata {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ExecutableMetadata {
    fn admit(metadata: &Metadata) -> Result<Self, RustPilotProcessError> {
        let mode = metadata.permissions().mode();
        if !metadata.is_file()
            || (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
            || mode & 0o7000 != 0
            || mode & 0o100 == 0
            || mode & 0o022 != 0
            || metadata.len() == 0
            || metadata.len() > MAX_ATTESTED_EXECUTABLE_BYTES
        {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode,
            uid: metadata.uid(),
            gid: metadata.gid(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

struct ExternalExecutableProof {
    canonical_path: PathBuf,
    file: File,
    snapshot: ExecutableMetadata,
    attestation: ExecutableFileAttestation,
    logical_id: String,
}

impl ExternalExecutableProof {
    fn verify(&self) -> Result<(), RustPilotProcessError> {
        let retained = ExecutableMetadata::admit(
            &self
                .file
                .metadata()
                .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
        )?;
        if retained != self.snapshot
            || std::fs::canonicalize(&self.canonical_path)
                .map_err(|_| RustPilotProcessError::ExecutableAdmission)?
                != self.canonical_path
            || hash_file(&self.file, self.snapshot)? != self.attestation.sha256()
        {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        let mapped = File::from(
            rustix::fs::open(
                &self.canonical_path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
        );
        let mapped = ExecutableMetadata::admit(
            &mapped
                .metadata()
                .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
        )?;
        if mapped != self.snapshot {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        Ok(())
    }
}

struct PrivateCwdProof {
    boundary: Arc<ProductionBoundary>,
    file: File,
    relative: PathBuf,
    canonical_path: PathBuf,
    device: u64,
    inode: u64,
}

impl PrivateCwdProof {
    fn open(
        boundary: Arc<ProductionBoundary>,
        relative: PathBuf,
    ) -> Result<Self, RustPilotProcessError> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(RustPilotProcessError::CwdAdmission);
        }
        let directory = open_dir_path_nofollow(boundary.directory(), &relative)
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        let file = directory.into_std_file();
        let metadata = file
            .metadata()
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o777 != 0o700
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(RustPilotProcessError::CwdAdmission);
        }
        let canonical_path = boundary.canonical_path().join(&relative);
        if std::fs::canonicalize(&canonical_path)
            .map_err(|_| RustPilotProcessError::CwdAdmission)?
            != canonical_path
        {
            return Err(RustPilotProcessError::CwdAdmission);
        }
        let proof = Self {
            boundary,
            file,
            relative,
            canonical_path,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        proof.verify()?;
        Ok(proof)
    }

    fn verify(&self) -> Result<(), RustPilotProcessError> {
        self.boundary
            .verify()
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        let mapped = open_dir_path_nofollow(self.boundary.directory(), &self.relative)
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        let metadata = mapped
            .into_std_file()
            .metadata()
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        let retained = self
            .file
            .metadata()
            .map_err(|_| RustPilotProcessError::CwdAdmission)?;
        if !metadata.is_dir()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || retained.dev() != self.device
            || retained.ino() != self.inode
        {
            return Err(RustPilotProcessError::CwdAdmission);
        }
        Ok(())
    }
}

fn application_launch(
    request: ProcessRequest,
    binding: ProcessEffectBinding,
    executable: Arc<ExternalExecutableProof>,
    cwd: Arc<PrivateCwdProof>,
    boundary: Arc<ProductionBoundary>,
) -> Result<crate::durable_process_service::EnrolledProviderLaunch, RustPilotProcessError> {
    let verify_executable = Arc::clone(&executable);
    let verify_cwd = Arc::clone(&cwd);
    let verify_preclaim: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync> =
        Arc::new(move || {
            verify_executable
                .verify()
                .map_err(|_| ProcessError::InvalidSpec)?;
            verify_cwd.verify().map_err(|_| ProcessError::InvalidSpec)
        });
    let release_executable = Arc::clone(&executable);
    let release_cwd = Arc::clone(&cwd);
    let verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync> =
        Arc::new(move || {
            release_executable
                .verify()
                .map_err(|_| ProcessError::InvalidSpec)?;
            release_cwd.verify().map_err(|_| ProcessError::InvalidSpec)
        });
    let attestation = executable.attestation;
    let initialize = Box::new(move || {
        executable.verify().map_err(|_| ProcessError::InvalidSpec)?;
        cwd.verify().map_err(|_| ProcessError::InvalidSpec)?;
        let root = boundary
            .directory()
            .try_clone()
            .map_err(ProcessError::Spawn)?
            .into_std_file();
        let executable_file = executable.file.try_clone().map_err(ProcessError::Spawn)?;
        let cwd_file = cwd.file.try_clone().map_err(ProcessError::Spawn)?;
        let journal_boundary = Arc::clone(&boundary);
        let launch = ProductionProcessLaunchAuthority::new(
            root,
            executable_file,
            executable.canonical_path.clone(),
            cwd_file,
            cwd.relative.clone(),
            attestation,
            move |records| persist_owned_directories(&journal_boundary, records),
        )?;
        Ok((launch, ProcessSupervisor::process_wide()?))
    });
    crate::durable_process_service::EnrolledProviderLaunch::from_application(
        request,
        binding,
        attestation,
        verify_preclaim,
        initialize,
        verify_release,
    )
    .map_err(|_| RustPilotProcessError::RequestMismatch)
}

fn build_effect(
    attempt: &RustPilotProcessAttempt,
    request: &ProcessRequest,
) -> Result<OutboxIntent, RustPilotProcessError> {
    let slot = match request.purpose() {
        ProcessPurpose::ProviderWorker => "rust-pilot-provider",
        ProcessPurpose::Verification => "rust-pilot-verification",
        ProcessPurpose::Git | ProcessPurpose::Tool | ProcessPurpose::Plugin => {
            return Err(RustPilotProcessError::UnsupportedPurpose);
        }
    };
    Ok(OutboxIntent::for_process(
        attempt.mission_id.clone(),
        Some(attempt.phase_id.clone()),
        OutboxEffectKind::ProviderProcess,
        EffectOperationSlot::new(slot)?,
        attempt.logical_attempt,
        serde_json::json!({"schema_version": 1}),
        request,
    )?)
}

fn prepare_effect(
    attempt: &RustPilotProcessAttempt,
    request: &ProcessRequest,
) -> Result<(String, ProcessEffectBinding), RustPilotProcessError> {
    let effect = build_effect(attempt, request)?;
    let idempotency_key = effect.idempotency_key().to_owned();
    let binding = effect.bind_process(request)?;
    Ok((idempotency_key, binding))
}

fn build_transition(
    attempt: &RustPilotProcessAttempt,
    effect: OutboxIntent,
    transition_id: String,
    committed_at_utc: String,
) -> Result<JournalIntent, RustPilotProcessError> {
    Ok(JournalIntent::new(
        transition_id,
        Some(attempt.mission_id.clone()),
        "rust_pilot_process_admitted",
        serde_json::json!({
            "phase_id": attempt.phase_id,
            "logical_attempt": attempt.logical_attempt,
        }),
        committed_at_utc,
    )?
    .with_outbox(effect)?)
}

fn transition_id(attempt: &RustPilotProcessAttempt) -> String {
    let mut digest = Sha256::new();
    digest.update(b"nanika.rust-pilot-process-transition.v1\0");
    digest.update(attempt.mission_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(attempt.phase_id.as_bytes());
    digest.update(attempt.logical_attempt.to_be_bytes());
    format!("pilot-process-{}", hex_digest(&digest.finalize().into()))
}

fn recovery_classification(
    disposition: RecoveredProcessDisposition,
) -> Result<RustPilotProcessRecovery, RustPilotProcessError> {
    match disposition {
        RecoveredProcessDisposition::RecoveredBeforeSpawnNotStarted
        | RecoveredProcessDisposition::BlockedLauncherCleanedNotStarted => {
            Ok(RustPilotProcessRecovery::RecoveredBeforeStartNotStarted)
        }
        RecoveredProcessDisposition::AuthorizedWithoutStartedCleanedUncertain
        | RecoveredProcessDisposition::StartedProcessCleanedUncertain
        | RecoveredProcessDisposition::UnreleasedUncertainCleaned
        | RecoveredProcessDisposition::AuthorizedUncertainCleaned
        | RecoveredProcessDisposition::StartedUncertainCleaned => {
            Ok(RustPilotProcessRecovery::ReleasedLostOutcomeUncertain)
        }
        RecoveredProcessDisposition::SpawnUnobserved
        | RecoveredProcessDisposition::Unresolved
        | RecoveredProcessDisposition::LeaderAbsentGroupPresent(_) => {
            Err(RustPilotProcessError::RecoveryUnresolved)
        }
    }
}

fn require_no_recovery_effects(
    store: &PrivateProcessLedgerStore,
) -> Result<(), RustPilotProcessError> {
    if store
        .recovery_effects(MAX_RECORDED_PROCESS_IDENTITIES)?
        .is_empty()
    {
        Ok(())
    } else {
        Err(RustPilotProcessError::ForeignProcessState)
    }
}

fn require_no_other_active_process(
    store: &PrivateProcessLedgerStore,
    idempotency_key: &str,
) -> Result<(), RustPilotProcessError> {
    let pending = store.pending_effects(MAX_RECORDED_PROCESS_IDENTITIES)?;
    let recovery = store.recovery_effects(MAX_RECORDED_PROCESS_IDENTITIES)?;
    if pending.len() == MAX_RECORDED_PROCESS_IDENTITIES
        || recovery.len() == MAX_RECORDED_PROCESS_IDENTITIES
        || pending.iter().chain(&recovery).any(|effect| {
            effect.effect_kind() == OutboxEffectKind::ProviderProcess
                && effect.idempotency_key() != idempotency_key
        })
    {
        return Err(RustPilotProcessError::ForeignProcessState);
    }
    Ok(())
}

fn require_only_current_recovery(
    store: &PrivateProcessLedgerStore,
    idempotency_key: &str,
) -> Result<(), RustPilotProcessError> {
    let effects = store.recovery_effects(MAX_RECORDED_PROCESS_IDENTITIES)?;
    if effects.len() == 1 && effects[0].idempotency_key() == idempotency_key {
        Ok(())
    } else {
        Err(RustPilotProcessError::ForeignProcessState)
    }
}

fn require_all_recorded_groups_absent(
    store: &PrivateProcessLedgerStore,
    allowed_live: Option<&crate::ProcessExecutionIdentity>,
) -> Result<(), RustPilotProcessError> {
    let identities = store.recorded_process_identities(MAX_RECORDED_PROCESS_IDENTITIES)?;
    if identities.len() == MAX_RECORDED_PROCESS_IDENTITIES {
        return Err(RustPilotProcessError::ForeignProcessState);
    }
    for identity in identities {
        match inspect_recorded_process_identity(
            identity.pid(),
            identity.process_group_id(),
            identity.process_start_identity(),
        )
        .map_err(|_| RustPilotProcessError::RecoveryUnresolved)?
        {
            RecordedProcessIdentityStatus::ExactGroupAbsent(_) => {}
            RecordedProcessIdentityStatus::ExactLive
                if allowed_live.is_some_and(|allowed| allowed == &identity) => {}
            RecordedProcessIdentityStatus::ExactLive
            | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
                return Err(RustPilotProcessError::ForeignProcessState);
            }
        }
    }
    Ok(())
}

fn exact_execution_and_cleanup_evidence(
    observation: Option<DurableProcessOutcomeObservation>,
) -> Result<
    (
        Option<KernelProcessIdentity>,
        Option<ExactProcessGroupAbsence>,
    ),
    RustPilotProcessError,
> {
    let Some(observation) = observation else {
        return Ok((None, None));
    };
    let post_release_identity = if observation.release_confirmed {
        Some(
            observation
                .post_release_identity
                .ok_or(RustPilotProcessError::CleanupUnproven)?,
        )
    } else {
        None
    };
    if !observation.group_absent || !observation.cleanup_complete {
        return Err(RustPilotProcessError::CleanupUnproven);
    }
    let Some(cleanup_identity) = observation.cleanup_identity else {
        return if observation.release_confirmed {
            Err(RustPilotProcessError::CleanupUnproven)
        } else {
            Ok((None, None))
        };
    };
    let absence = match inspect_recorded_process_identity(
        cleanup_identity.pid(),
        cleanup_identity.process_group_id(),
        cleanup_identity.process_start_identity(),
    )
    .map_err(|_| RustPilotProcessError::CleanupUnproven)?
    {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => absence,
        RecordedProcessIdentityStatus::ExactLive
        | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
            return Err(RustPilotProcessError::CleanupUnproven);
        }
    };
    Ok((post_release_identity, Some(absence)))
}

fn persist_owned_directories(
    boundary: &ProductionBoundary,
    records: &[ProcessOwnedDirectory],
) -> std::io::Result<()> {
    if records.len() > MAX_OWNERSHIP_RECORDS {
        return Err(std::io::Error::other("too many process ownership records"));
    }
    validate_existing_ownership_file(boundary)?;
    let mut encoded = Vec::with_capacity(MAX_OWNERSHIP_BYTES);
    encoded.extend_from_slice(OWNERSHIP_MAGIC);
    encoded.push(u8::try_from(records.len()).map_err(|_| {
        std::io::Error::other("process ownership record count exceeds its encoding")
    })?);
    for record in records {
        let record = record.encode();
        let length = u16::try_from(record.len())
            .map_err(|_| std::io::Error::other("process ownership record is too large"))?;
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&record);
    }
    if encoded.len() > MAX_OWNERSHIP_BYTES {
        return Err(std::io::Error::other(
            "process ownership journal is too large",
        ));
    }
    atomic_replace_private(
        boundary.directory(),
        Path::new(RUST_PILOT_PROCESS_OWNERSHIP_FILE),
        &encoded,
    )?;
    sync_dir(boundary.directory())?;
    let observed = read_owned_directories(boundary)?;
    if observed.len() != records.len()
        || observed
            .iter()
            .zip(records)
            .any(|(left, right)| left.encode() != right.encode())
    {
        return Err(std::io::Error::other(
            "process ownership journal did not retain the exact records",
        ));
    }
    Ok(())
}

fn validate_existing_ownership_file(boundary: &ProductionBoundary) -> std::io::Result<()> {
    read_owned_directories(boundary).map(|_| ())
}

fn read_owned_directories(
    boundary: &ProductionBoundary,
) -> std::io::Result<Vec<ProcessOwnedDirectory>> {
    let descriptor = match rustix::fs::openat(
        boundary.directory(),
        Path::new(RUST_PILOT_PROCESS_OWNERSHIP_FILE),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut file = File::from(descriptor);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(std::io::Error::other(
            "process ownership journal has an unsafe filesystem shape",
        ));
    }
    let mut encoded = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(MAX_OWNERSHIP_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut encoded)?;
    if encoded.len() > MAX_OWNERSHIP_BYTES
        || encoded.get(..OWNERSHIP_MAGIC.len()) != Some(OWNERSHIP_MAGIC)
    {
        return Err(std::io::Error::other("invalid process ownership journal"));
    }
    let count = usize::from(
        *encoded
            .get(OWNERSHIP_MAGIC.len())
            .ok_or_else(|| std::io::Error::other("missing process ownership record count"))?,
    );
    if count > MAX_OWNERSHIP_RECORDS {
        return Err(std::io::Error::other("too many process ownership records"));
    }
    let mut offset = OWNERSHIP_MAGIC.len() + 1;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let end = offset.saturating_add(2);
        let length = u16::from_be_bytes(
            encoded
                .get(offset..end)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| std::io::Error::other("invalid ownership record length"))?,
        );
        offset = end;
        let end = offset.saturating_add(usize::from(length));
        let record = encoded
            .get(offset..end)
            .ok_or_else(|| std::io::Error::other("truncated process ownership record"))?;
        records.push(
            ProcessOwnedDirectory::decode(record)
                .map_err(|_| std::io::Error::other("invalid process ownership record"))?,
        );
        offset = end;
    }
    if offset != encoded.len() {
        return Err(std::io::Error::other(
            "unexpected process ownership journal bytes",
        ));
    }
    Ok(records)
}

fn recover_owned_directories(boundary: &ProductionBoundary) -> Result<(), RustPilotProcessError> {
    boundary
        .verify()
        .map_err(|_| RustPilotProcessError::OwnershipJournal)?;
    let records =
        read_owned_directories(boundary).map_err(|_| RustPilotProcessError::OwnershipJournal)?;
    let root = boundary
        .directory()
        .try_clone()
        .map_err(|_| RustPilotProcessError::OwnershipJournal)?
        .into_std_file();
    recover_orphaned_process_directories(&root, &records)
        .map_err(|_| RustPilotProcessError::OwnershipJournal)?;
    persist_owned_directories(boundary, &[]).map_err(|_| RustPilotProcessError::OwnershipJournal)
}

fn close_store_with<T>(
    store: PrivateProcessLedgerStore,
    error: RustPilotProcessError,
) -> Result<T, RustPilotProcessError> {
    match store.close() {
        Ok(()) => Err(error),
        Err(close) => Err(close.into()),
    }
}

fn hash_file(file: &File, expected: ExecutableMetadata) -> Result<[u8; 32], RustPilotProcessError> {
    let before = ExecutableMetadata::admit(
        &file
            .metadata()
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
    )?;
    if before != expected {
        return Err(RustPilotProcessError::ExecutableAdmission);
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut offset = 0_u64;
    while offset < expected.length {
        let remaining = expected.length.saturating_sub(offset);
        let length = usize::try_from(remaining.min(HASH_BUFFER_BYTES as u64))
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?;
        let read = file
            .read_at(&mut buffer[..length], offset)
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?;
        if read == 0 {
            return Err(RustPilotProcessError::ExecutableAdmission);
        }
        digest.update(&buffer[..read]);
        offset = offset
            .checked_add(
                u64::try_from(read).map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
            )
            .ok_or(RustPilotProcessError::ExecutableAdmission)?;
    }
    let after = ExecutableMetadata::admit(
        &file
            .metadata()
            .map_err(|_| RustPilotProcessError::ExecutableAdmission)?,
    )?;
    if after != expected {
        return Err(RustPilotProcessError::ExecutableAdmission);
    }
    Ok(digest.finalize().into())
}

fn logical_executable_id(
    canonical_path: &Path,
    snapshot: ExecutableMetadata,
    attestation_sha256: [u8; 32],
) -> Result<String, RustPilotProcessError> {
    let path = canonical_path.as_os_str().as_bytes();
    let path_length =
        u64::try_from(path.len()).map_err(|_| RustPilotProcessError::ExecutableAdmission)?;
    let mut digest = Sha256::new();
    digest.update(b"nanika.rust-pilot-executable-id.v2\0");
    digest.update(path_length.to_be_bytes());
    digest.update(path);
    digest.update(snapshot.device.to_be_bytes());
    digest.update(snapshot.inode.to_be_bytes());
    digest.update(snapshot.length.to_be_bytes());
    digest.update(snapshot.mode.to_be_bytes());
    digest.update(snapshot.uid.to_be_bytes());
    digest.update(snapshot.gid.to_be_bytes());
    digest.update(snapshot.modified_seconds.to_be_bytes());
    digest.update(snapshot.modified_nanoseconds.to_be_bytes());
    digest.update(snapshot.changed_seconds.to_be_bytes());
    digest.update(snapshot.changed_nanoseconds.to_be_bytes());
    digest.update(attestation_sha256);
    Ok(format!(
        "rust-pilot-exec-v2-{}",
        hex_digest(&digest.finalize().into())
    ))
}

fn copy_request(request: &ProcessRequest) -> Result<ProcessRequest, ServiceContractError> {
    let mut copy = ProcessRequest::new(
        request.purpose(),
        request.executable_id(),
        request.working_root(),
    )?;
    for argument in request.expose_arguments() {
        copy = copy.with_argument(argument)?;
    }
    for (key, value) in request.expose_environment() {
        copy = copy.with_environment(key, value)?;
    }
    if let Some(stdin) = request.expose_stdin() {
        copy = copy.with_stdin(stdin.to_vec())?;
    }
    copy = copy.with_max_output_bytes(request.max_output_bytes())?;
    if request.truncated_output_acknowledged() {
        copy = copy.with_truncated_output_acknowledged();
    }
    Ok(copy)
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut rendered = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

fn now_utc() -> String {
    let epoch_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(earlier) => -i64::try_from(earlier.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let days = epoch_secs.div_euclid(86_400);
    let seconds = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_missing_identity_with_incomplete_cleanup_is_unproven() {
        for (group_absent, cleanup_complete) in [(false, false), (false, true), (true, false)] {
            let observation = DurableProcessOutcomeObservation {
                not_started_reason: None,
                release_confirmed: false,
                post_release_identity: None,
                cleanup_identity: None,
                group_absent,
                cleanup_complete,
            };

            assert!(matches!(
                exact_execution_and_cleanup_evidence(Some(observation)),
                Err(RustPilotProcessError::CleanupUnproven)
            ));
        }
    }

    #[test]
    fn no_launch_with_complete_cleanup_needs_no_identity_witness() {
        let observation = DurableProcessOutcomeObservation {
            not_started_reason: None,
            release_confirmed: false,
            post_release_identity: None,
            cleanup_identity: None,
            group_absent: true,
            cleanup_complete: true,
        };

        let evidence = exact_execution_and_cleanup_evidence(Some(observation));
        assert!(matches!(evidence, Ok((None, None))));
    }
}
