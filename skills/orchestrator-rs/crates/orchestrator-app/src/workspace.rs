use crate::{
    capability::{CapabilityError, CapabilityRoot, SharedCapabilityRoot},
    checkpoint_projection::{
        FIXTURE_EVENT_SEQUENCE_KEY, JournalCheckpointExpectation, mission_status_name,
        phase_status_name,
    },
    fixture_authority::{FixtureAuthorityError, FixtureExtras, FreshFixtureAuthority},
    fs_util::{
        FileIdentity, atomic_replace_executable, atomic_replace_private, create_dir_private,
        create_private_file, identity, link_count, mode, open_dir_path_nofollow,
        open_file_nofollow, read_bounded_nofollow, remove_tree_owned, sync_dir,
    },
    runtime_home::ProductionBoundary,
};
use cap_std::fs::Dir;
use orchestrator_core::{
    CheckpointPlan, CheckpointProjection, CheckpointSourceShape, MissionId, MissionStatus, PhaseId,
    PhaseStatus, WorkerId, decode_checkpoint, decode_event_line, encode_current_checkpoint,
    encode_current_plan,
};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};
use thiserror::Error;

const MAX_SEED_BYTES: usize = 1024 * 1024;
const MAX_EVENT_LOG_BYTES: usize = 8 * 1024 * 1024;
const PHASE_WORKER_MARKER: &str = "nanika-worker-authority-v1";
const PHASE_WORKER_MARKER_DOMAIN: &[u8] = b"nanika-worker-authority";
const PHASE_WORKER_MARKER_VERSION: u8 = 1;
const PHASE_WORKER_MARKER_CREATE_ATTEMPTS: usize = 64;
static STAGING_NONCE: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
std::thread_local! {
    static PHASE_WORKER_MARKER_TEST_NONCE: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Authority(#[from] FixtureAuthorityError),
    #[error("workspace seed field {field} exceeds the one-MiB bound")]
    SeedTooLarge { field: &'static str },
    #[error("workspace seed checkpoint is invalid: {0}")]
    InvalidCheckpoint(String),
    #[error("workspace seed checkpoint belongs to {found}, not {expected}")]
    CheckpointMissionMismatch { expected: String, found: String },
    #[error("workspace {0} already exists")]
    Collision(String),
    #[error("workspace filesystem operation failed at {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace known entry {path:?} has the wrong type or mode")]
    InvalidKnownEntry { path: PathBuf },
    #[error("workspace identity changed during use")]
    IdentityChanged,
    #[error("optional sidecar {0:?} contains invalid data")]
    InvalidSidecar(OptionalSidecar),
    #[error("workspace event log is not newline-terminated")]
    UnterminatedEventLog,
    #[error("workspace event log exceeds its eight-MiB bound")]
    EventLogTooLarge,
    #[error("workspace event is not one valid event for this workspace")]
    InvalidFixtureEvent,
    #[error("workspace projection conflicts with existing transaction evidence")]
    ProjectionConflict,
    #[error("workspace lifecycle projection may have crossed the durable boundary at {operation}")]
    ProjectionIndeterminate { operation: &'static str },
    #[error("workspace lifecycle projection stopped at injected boundary {0}")]
    InjectedProjectionFault(&'static str),
    #[error("workspace requires recovery after {operation}: {source}")]
    RecoveryRequired {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("fixture-only capability is unavailable for this workspace authority")]
    FixtureCapabilityUnavailable,
    #[error("workspace projection already has an active writer")]
    ProjectionWriterLeased,
    #[error("production capability is unavailable for this workspace authority")]
    ProductionCapabilityUnavailable,
    #[error("workspace was durably published but requires admission recovery")]
    PublishedWorkspaceRequiresRecovery {
        #[source]
        source: Box<WorkspaceError>,
    },
    #[error("production projection recovery coordinator is not enrolled")]
    ProductionProjectionRecoveryUnavailable,
    #[error("process working root is not the exact derived phase-worker directory")]
    ProcessWorkingRootMismatch,
    #[error("workspace exact base file {path:?} diverges from its expected bytes")]
    ExactBaseDivergent { path: PathBuf },
    #[error(
        "workspace event log holds foreign (non-empty) history that a non-pristine recovery \
         constructor did not write and cannot adopt"
    )]
    ForeignEventLogHistory,
}

impl From<CapabilityError> for WorkspaceError {
    fn from(error: CapabilityError) -> Self {
        match error {
            // No workspace root is minted from an adopted fixture root, and
            // none is recovered from a fresh one, so neither freshness arm can
            // arise here; both fold into the fail-closed neighbour rather than
            // widening this enum for unreachable cases.
            CapabilityError::IdentityChanged
            | CapabilityError::RootNotFreshlyCreated
            | CapabilityError::RootFreshlyCreated => Self::IdentityChanged,
            CapabilityError::Io(source) => Self::Io {
                operation: "verify capability root",
                source,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleProjectionFault {
    EventSynced,
    CheckpointRenamed,
    CheckpointRenamedWithConflict,
    CheckpointRenamedWithIdentityChange,
}

/// Bounded bytes used to create the complete atomic base workspace.
///
/// The bytes carry no authority and are valid for either an isolated fixture
/// or an enrolled production boundary.
pub struct WorkspaceSeed {
    mission: Vec<u8>,
    checkpoint: Vec<u8>,
    plan: Vec<u8>,
}

/// Historical source-compatible name for an isolated-fixture workspace seed.
pub type FixtureWorkspaceSeed = WorkspaceSeed;

impl fmt::Debug for WorkspaceSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceSeed")
            .field("mission_bytes", &self.mission.len())
            .field("checkpoint_bytes", &self.checkpoint.len())
            .field("plan_bytes", &self.plan.len())
            .finish()
    }
}

impl WorkspaceSeed {
    pub fn new(
        mission: impl Into<Vec<u8>>,
        checkpoint: &CheckpointProjection,
        plan: impl Into<Vec<u8>>,
    ) -> Result<Self, WorkspaceError> {
        let checkpoint = encode_current_checkpoint(checkpoint)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        Self::from_encoded(mission, checkpoint, plan)
    }

    pub fn from_encoded(
        mission: impl Into<Vec<u8>>,
        checkpoint: impl Into<Vec<u8>>,
        plan: impl Into<Vec<u8>>,
    ) -> Result<Self, WorkspaceError> {
        let result = Self {
            mission: mission.into(),
            checkpoint: checkpoint.into(),
            plan: plan.into(),
        };
        for (field, bytes) in [
            ("mission", result.mission.as_slice()),
            ("checkpoint", result.checkpoint.as_slice()),
            ("plan", result.plan.as_slice()),
        ] {
            if bytes.len() > MAX_SEED_BYTES {
                return Err(WorkspaceError::SeedTooLarge { field });
            }
        }
        let decoded = decode_checkpoint(&result.checkpoint)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        if decoded.source_shape != CheckpointSourceShape::EnvelopeV1 {
            return Err(WorkspaceError::InvalidCheckpoint(
                "initial writer requires envelope-v1/payload-v2".to_owned(),
            ));
        }
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OptionalSidecar {
    LinearIssueId,
    MissionPath,
    TargetId,
    TaskType,
    PrUrl,
    Pid,
    Cancel,
}

impl OptionalSidecar {
    pub(crate) const ALL: [Self; 7] = [
        Self::LinearIssueId,
        Self::MissionPath,
        Self::TargetId,
        Self::TaskType,
        Self::PrUrl,
        Self::Pid,
        Self::Cancel,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::LinearIssueId => "linear_issue_id",
            Self::MissionPath => "mission_path",
            Self::TargetId => "target_id",
            Self::TaskType => "task_type",
            Self::PrUrl => "pr_url",
            Self::Pid => "pid",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum SidecarState {
    Missing,
    Present(Vec<u8>),
    Degraded(Vec<u8>),
}

impl fmt::Debug for SidecarState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("SidecarState::Missing"),
            Self::Present(bytes) => formatter
                .debug_struct("SidecarState::Present")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Degraded(bytes) => formatter
                .debug_struct("SidecarState::Degraded")
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceHealth {
    pub sidecars: BTreeMap<OptionalSidecar, SidecarState>,
}

/// Closed set of ordinary files beneath one lazy worker directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerFileKind {
    Instructions,
    Output,
    Context,
    Signal,
}

impl WorkerFileKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Instructions => "CLAUDE.md",
            Self::Output => "output.md",
            Self::Context => "workspace-context.md",
            Self::Signal => "orchestrator.signal.json",
        }
    }
}

/// Opaque authority for one exact documented lazy workspace file.
pub struct WorkspaceFileAuthority {
    boundary: SharedCapabilityRoot,
    workspace_id: MissionId,
    workspace_identity: FileIdentity,
    parent: Dir,
    parent_relative: PathBuf,
    parent_identity: FileIdentity,
    name: String,
    executable: bool,
}

impl fmt::Debug for WorkspaceFileAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceFileAuthority")
            .field("kind", &"bounded-workspace-file")
            .finish()
    }
}

impl WorkspaceFileAuthority {
    /// Atomically replaces this bounded file with its required exact mode.
    pub fn replace(&self, bytes: &[u8]) -> Result<(), WorkspaceError> {
        if bytes.len() > MAX_SEED_BYTES {
            return Err(WorkspaceError::SeedTooLarge {
                field: "lazy_workspace_file",
            });
        }
        self.verify()?;
        let name = Path::new(&self.name);
        validate_replace_target_mode(&self.parent, name, self.required_mode())?;
        let result = if self.executable {
            atomic_replace_executable(&self.parent, name, bytes)
        } else {
            atomic_replace_private(&self.parent, name, bytes)
        };
        result.map_err(|source| WorkspaceError::Io {
            operation: "replace lazy workspace file",
            source,
        })
    }

    fn required_mode(&self) -> u32 {
        if self.executable { 0o700 } else { 0o600 }
    }

    fn verify(&self) -> Result<(), WorkspaceError> {
        self.boundary.verify()?;
        let workspace_relative = Path::new("workspaces").join(self.workspace_id.as_str());
        let workspace = open_dir_path_nofollow(self.boundary.directory(), &workspace_relative)
            .map_err(|source| WorkspaceError::Io {
                operation: "reopen lazy-file workspace",
                source,
            })?;
        if identity(
            &workspace
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect lazy-file workspace",
                    source,
                })?,
        ) != self.workspace_identity
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        let current_parent =
            open_dir_path_nofollow(self.boundary.directory(), &self.parent_relative).map_err(
                |source| WorkspaceError::Io {
                    operation: "reopen lazy-file parent",
                    source,
                },
            )?;
        let current_identity =
            identity(
                &current_parent
                    .dir_metadata()
                    .map_err(|source| WorkspaceError::Io {
                        operation: "inspect lazy-file parent",
                        source,
                    })?,
            );
        let held_identity =
            identity(
                &self
                    .parent
                    .dir_metadata()
                    .map_err(|source| WorkspaceError::Io {
                        operation: "inspect held lazy-file parent",
                        source,
                    })?,
            );
        if current_identity != self.parent_identity || held_identity != self.parent_identity {
            return Err(WorkspaceError::IdentityChanged);
        }
        Ok(())
    }
}

impl WorkspaceHealth {
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.sidecars
            .values()
            .any(|state| matches!(state, SidecarState::Degraded(_)))
    }
}

/// Opaque authority for one validated `workspaces/<mission-id>` directory.
pub struct WorkspaceAuthority {
    id: MissionId,
    pub(crate) directory: Dir,
    identity: FileIdentity,
    kind: WorkspaceKind,
}

/// Read-only derivation of one exact Go-compatible phase-worker binding.
///
/// This type is deliberately crate-private and non-`Clone`. It has no raw
/// constructor: [`WorkspaceAuthority::phase_worker_binding`] is the only way
/// to obtain one, and [`PhaseWorkerBinding::materialize`] consumes it when the
/// lazy worker tree is finally materialized.
pub(crate) struct PhaseWorkerBinding {
    boundary: SharedCapabilityRoot,
    workspace_id: MissionId,
    workspace_directory: Dir,
    workspace_identity: FileIdentity,
    phase_id: PhaseId,
    worker_id: WorkerId,
    root_relative: PathBuf,
    canonical_path: PathBuf,
}

/// Opaque authority for one exact Go-compatible phase worker directory.
///
/// This type is deliberately crate-private and non-`Clone`. Only a sealed
/// [`PhaseWorkerBinding`] can mint it.
pub(crate) struct PhaseWorkerAuthority {
    boundary: SharedCapabilityRoot,
    workspace_id: MissionId,
    workspace_identity: FileIdentity,
    worker_id: WorkerId,
    directory: Dir,
    identity: FileIdentity,
    claude_identity: FileIdentity,
    marker_identity: FileIdentity,
    root_relative: PathBuf,
    canonical_path: PathBuf,
    marker_bytes: Vec<u8>,
}

enum WorkspaceKind {
    Fixture {
        boundary: SharedCapabilityRoot,
        extras: Arc<FixtureExtras>,
    },
    Production(Arc<ProductionBoundary>),
    /// Disposable verification-canary workspace bounded by the fixed
    /// compatibility target. It retains the fixture authority's shared lease
    /// sidecar so create/reopen cannot mint duplicate projection writers. The
    /// worker derived from this workspace is the only process cwd a hermetic
    /// process canary may spawn.
    ///
    /// Staged for the CLI opt-in wiring; constructed only by the canary
    /// workspace constructors below, which are exercised by the
    /// dispatch-integration tests.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    HermeticCanary {
        boundary: SharedCapabilityRoot,
        extras: Arc<FixtureExtras>,
    },
}

impl WorkspaceKind {
    fn boundary(&self) -> &dyn CapabilityRoot {
        match self {
            Self::Fixture { boundary, .. } => boundary.as_ref(),
            Self::Production(boundary) => boundary.as_ref(),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            Self::HermeticCanary { boundary, .. } => boundary.as_ref(),
        }
    }

    fn shared_boundary(&self) -> SharedCapabilityRoot {
        match self {
            Self::Fixture { boundary, .. } => Arc::clone(boundary),
            Self::Production(boundary) => boundary.clone(),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            Self::HermeticCanary { boundary, .. } => Arc::clone(boundary),
        }
    }
}

enum ProjectionLease {
    Fixture(Arc<FixtureExtras>),
    Production(Arc<ProductionBoundary>),
}

/// Exclusive authority for ordered event/checkpoint publication.
///
/// Construction consumes a [`WorkspaceAuthority`], snapshots its exact prior
/// event log and checkpoint, and acquires the only in-process writer lease for
/// that exact workspace identity. A production writer additionally remains
/// rooted in the retained whole-home kernel lease.
struct ProjectionWriterCore {
    workspace: WorkspaceAuthority,
    lease: ProjectionLease,
    expected_event_log: Vec<u8>,
    expected_checkpoint: Vec<u8>,
}

/// Exclusive isolated-fixture authority for ordered event/checkpoint publication.
pub struct FixtureProjectionWriter {
    core: ProjectionWriterCore,
}

/// Reserved production projection authority rooted in an enrolled boundary.
///
/// This type deliberately exposes no publication method. The production
/// lifecycle/recovery actor will own the only mutating API; until that actor is
/// implemented, callers can reserve and inspect this authority but cannot
/// publish a transaction through it.
///
/// Fixture authority cannot be substituted for production authority:
///
/// ```compile_fail,E0308
/// use orchestrator_app::{FixtureProjectionWriter, ProductionProjectionWriter};
///
/// fn cannot_promote(writer: FixtureProjectionWriter) -> ProductionProjectionWriter {
///     writer
/// }
/// ```
pub struct ProductionProjectionWriter {
    core: ProjectionWriterCore,
}

impl fmt::Debug for FixtureProjectionWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureProjectionWriter")
            .field("kind", &"exclusive-fixture-projection-writer")
            .finish()
    }
}

impl fmt::Debug for ProductionProjectionWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionProjectionWriter")
            .field("kind", &"reserved-production-projection-writer")
            .finish()
    }
}

impl Drop for ProjectionWriterCore {
    fn drop(&mut self) {
        match &self.lease {
            ProjectionLease::Fixture(extras) => {
                lock_unpoisoned(&extras.projection_leases).remove(&self.workspace.identity);
            }
            ProjectionLease::Production(boundary) => {
                boundary.release_projection_lease(self.workspace.identity);
            }
        }
    }
}

impl fmt::Debug for WorkspaceAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceAuthority")
            .field("kind", &"bounded-workspace")
            .finish()
    }
}

impl fmt::Debug for PhaseWorkerAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhaseWorkerAuthority")
            .field("kind", &"bounded-phase-worker")
            .finish()
    }
}

impl fmt::Debug for PhaseWorkerBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PhaseWorkerBinding")
            .field("kind", &"derived-phase-worker-binding")
            .finish()
    }
}

impl PhaseWorkerBinding {
    pub(crate) fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn shares_boundary(&self, boundary: &SharedCapabilityRoot) -> bool {
        Arc::ptr_eq(&self.boundary, boundary)
    }

    #[cfg(unix)]
    pub(crate) fn materialize(self) -> Result<PhaseWorkerAuthority, WorkspaceError> {
        self.verify()?;
        let PhaseWorkerBinding {
            boundary,
            workspace_id,
            workspace_directory,
            workspace_identity,
            phase_id,
            worker_id,
            root_relative,
            canonical_path,
        } = self;
        let workers = open_dir_path_nofollow(&workspace_directory, Path::new("workers")).map_err(
            |source| WorkspaceError::Io {
                operation: "open phase workers directory",
                source,
            },
        )?;
        let worker_directory = ensure_phase_worker_child(&workers, Path::new(worker_id.as_str()))?;
        let worker_identity =
            identity(
                &worker_directory
                    .dir_metadata()
                    .map_err(|source| WorkspaceError::Io {
                        operation: "inspect phase worker directory",
                        source,
                    })?,
            );
        verify_worker_entry_name(&workers, &worker_id, worker_identity)?;

        let claude = ensure_phase_worker_child(&worker_directory, Path::new(".claude"))?;
        let claude_identity =
            identity(&claude.dir_metadata().map_err(|source| WorkspaceError::Io {
                operation: "inspect phase worker authority directory",
                source,
            })?);
        let marker_bytes = phase_worker_marker_bytes(&workspace_id, &phase_id, &worker_id);
        let marker_identity = seal_phase_worker_marker(&claude, &marker_bytes)?;
        let hooks = ensure_phase_worker_child(&claude, Path::new("hooks"))?;

        let artifacts = open_dir_path_nofollow(&workspace_directory, Path::new("artifacts"))
            .map_err(|source| WorkspaceError::Io {
                operation: "open phase artifacts directory",
                source,
            })?;
        let artifact = ensure_phase_worker_child(&artifacts, Path::new(phase_id.as_str()))?;
        let scratch = ensure_phase_worker_child(&workspace_directory, Path::new("scratch"))?;
        let scratch_phase = ensure_phase_worker_child(&scratch, Path::new(phase_id.as_str()))?;

        for directory in [
            &hooks,
            &claude,
            &worker_directory,
            &workers,
            &artifact,
            &artifacts,
            &scratch_phase,
            &scratch,
            &workspace_directory,
        ] {
            sync_dir(directory).map_err(|source| WorkspaceError::Io {
                operation: "sync phase worker directory",
                source,
            })?;
        }

        let authority = PhaseWorkerAuthority {
            boundary,
            workspace_id,
            workspace_identity,
            worker_id,
            directory: worker_directory,
            identity: worker_identity,
            claude_identity,
            marker_identity,
            root_relative,
            canonical_path,
            marker_bytes,
        };
        authority.verify()?;
        Ok(authority)
    }

    #[cfg(unix)]
    pub(crate) fn verify(&self) -> Result<(), WorkspaceError> {
        self.boundary.verify()?;
        let workspace_relative = Path::new("workspaces").join(self.workspace_id.as_str());
        let current = open_dir_path_nofollow(self.boundary.directory(), &workspace_relative)
            .map_err(|source| WorkspaceError::Io {
                operation: "reopen derived phase worker workspace",
                source,
            })?;
        let current_metadata = current
            .dir_metadata()
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect derived phase worker workspace",
                source,
            })?;
        let held_metadata =
            self.workspace_directory
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect retained phase worker workspace",
                    source,
                })?;
        let expected_root_relative = Path::new("workspaces")
            .join(self.workspace_id.as_str())
            .join("workers")
            .join(self.worker_id.as_str());
        let expected_canonical_path = self.boundary.canonical_path().join(&expected_root_relative);
        if identity(&current_metadata) != self.workspace_identity
            || identity(&held_metadata) != self.workspace_identity
            || mode(&current_metadata) != 0o700
            || mode(&held_metadata) != 0o700
            || self.root_relative.as_os_str().as_bytes()
                != expected_root_relative.as_os_str().as_bytes()
            || self.canonical_path.as_os_str().as_bytes()
                != expected_canonical_path.as_os_str().as_bytes()
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        Ok(())
    }
}

impl PhaseWorkerAuthority {
    pub(crate) fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the retained worker directory capability. Crate-private so the
    /// worker-spawn layer can write the worker's `CLAUDE.md` and
    /// `settings.local.json` through the same no-follow capability that
    /// materialized the tree.
    pub(crate) fn worker_directory(&self) -> &Dir {
        &self.directory
    }

    /// Returns the canonical absolute path of the worker directory.
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn verify_process_cwd(&self) -> Result<(), WorkspaceError> {
        self.verify()
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn publish_canary_sentinel(
        &self,
        name: &'static str,
        bytes: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.verify()?;
        let name = Path::new(name);
        create_private_file(&self.directory, name, bytes, false).map_err(|source| {
            WorkspaceError::Io {
                operation: "create hermetic canary sentinel",
                source,
            }
        })?;
        if let Err(source) = sync_dir(&self.directory) {
            // The create and file fsync have already happened. Keep the named
            // evidence in place when the directory fsync is indeterminate;
            // an unsynced removal would destroy the only recovery signal.
            return Err(WorkspaceError::RecoveryRequired {
                operation: "sync hermetic canary sentinel directory",
                source,
            });
        }
        if let Err(error) = self.verify() {
            self.directory
                .remove_file(name)
                .and_then(|()| sync_dir(&self.directory))
                .map_err(|source| WorkspaceError::RecoveryRequired {
                    operation: "remove sentinel after worker identity changed",
                    source,
                })?;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn process_launch_descriptors(
        &self,
    ) -> Result<(std::fs::File, std::fs::File, PathBuf), WorkspaceError> {
        self.verify()?;
        let root = self
            .boundary
            .directory()
            .try_clone()
            .map_err(|source| WorkspaceError::Io {
                operation: "clone phase worker capability root",
                source,
            })?
            .into_std_file();
        let cwd = self
            .directory
            .try_clone()
            .map_err(|source| WorkspaceError::Io {
                operation: "clone phase worker process directory",
                source,
            })?
            .into_std_file();
        Ok((root, cwd, self.root_relative.clone()))
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "legacy fixture consumers retain this descriptor view until their next migration cell"
        )
    )]
    pub(crate) fn process_cwd_binding(
        &self,
    ) -> Result<(Dir, FileIdentity, PathBuf), WorkspaceError> {
        self.verify()?;
        Ok((
            self.directory
                .try_clone()
                .map_err(|source| WorkspaceError::Io {
                    operation: "clone phase worker process directory",
                    source,
                })?,
            self.identity,
            self.root_relative.clone(),
        ))
    }

    fn verify(&self) -> Result<(), WorkspaceError> {
        self.boundary.verify()?;
        let workspace_relative = Path::new("workspaces").join(self.workspace_id.as_str());
        let workspace = open_dir_path_nofollow(self.boundary.directory(), &workspace_relative)
            .map_err(|source| WorkspaceError::Io {
                operation: "reopen phase worker workspace",
                source,
            })?;
        if identity(
            &workspace
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect phase worker workspace",
                    source,
                })?,
        ) != self.workspace_identity
        {
            return Err(WorkspaceError::IdentityChanged);
        }

        let workers =
            open_dir_path_nofollow(&workspace, Path::new("workers")).map_err(|source| {
                WorkspaceError::Io {
                    operation: "reopen phase workers directory",
                    source,
                }
            })?;
        let current = open_dir_path_nofollow(&workers, Path::new(self.worker_id.as_str()))
            .map_err(|source| WorkspaceError::Io {
                operation: "reopen phase worker directory",
                source,
            })?;
        let current_metadata = current
            .dir_metadata()
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect reopened phase worker directory",
                source,
            })?;
        let held_metadata = self
            .directory
            .dir_metadata()
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect held phase worker directory",
                source,
            })?;
        if identity(&current_metadata) != self.identity
            || identity(&held_metadata) != self.identity
            || mode(&current_metadata) != 0o700
            || mode(&held_metadata) != 0o700
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        verify_worker_entry_name(&workers, &self.worker_id, self.identity)?;
        let current_claude =
            open_dir_path_nofollow(&current, Path::new(".claude")).map_err(|source| {
                WorkspaceError::Io {
                    operation: "reopen phase worker authority directory",
                    source,
                }
            })?;
        let held_claude =
            open_dir_path_nofollow(&self.directory, Path::new(".claude")).map_err(|source| {
                WorkspaceError::Io {
                    operation: "reopen held phase worker authority directory",
                    source,
                }
            })?;
        let current_claude_metadata =
            current_claude
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect reopened phase worker authority directory",
                    source,
                })?;
        let held_claude_metadata =
            held_claude
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect held phase worker authority directory",
                    source,
                })?;
        if identity(&current_claude_metadata) != self.claude_identity
            || identity(&held_claude_metadata) != self.claude_identity
            || mode(&current_claude_metadata) != 0o700
            || mode(&held_claude_metadata) != 0o700
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        let marker_identity = validate_phase_worker_marker(&current_claude, &self.marker_bytes)?;
        if marker_identity != self.marker_identity {
            return Err(WorkspaceError::IdentityChanged);
        }
        Ok(())
    }
}

impl FreshFixtureAuthority {
    pub fn create_workspace(
        &self,
        mission_id: MissionId,
        seed: WorkspaceSeed,
    ) -> Result<WorkspaceAuthority, WorkspaceError> {
        create_workspace_under(
            WorkspaceKind::Fixture {
                boundary: Arc::clone(&self.boundary),
                extras: Arc::clone(&self.extras),
            },
            mission_id,
            seed,
        )
    }

    pub fn open_workspace(
        &self,
        mission_id: MissionId,
    ) -> Result<WorkspaceAuthority, WorkspaceError> {
        open_workspace_under(
            WorkspaceKind::Fixture {
                boundary: Arc::clone(&self.boundary),
                extras: Arc::clone(&self.extras),
            },
            mission_id,
            "open workspace",
        )
    }
}

fn create_workspace_under(
    kind: WorkspaceKind,
    mission_id: MissionId,
    seed: WorkspaceSeed,
) -> Result<WorkspaceAuthority, WorkspaceError> {
    create_workspace_under_with_hook(kind, mission_id, seed, |_, _| Ok(()))
}

fn create_workspace_under_with_hook(
    kind: WorkspaceKind,
    mission_id: MissionId,
    seed: WorkspaceSeed,
    post_publish: impl FnOnce(&Dir, &MissionId) -> Result<(), WorkspaceError>,
) -> Result<WorkspaceAuthority, WorkspaceError> {
    let boundary = kind.shared_boundary();
    boundary.verify()?;
    let decoded = decode_checkpoint(&seed.checkpoint)
        .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
    if decoded.projection.workspace_id != mission_id.as_str() {
        return Err(WorkspaceError::CheckpointMissionMismatch {
            expected: mission_id.to_string(),
            found: decoded.projection.workspace_id,
        });
    }
    let workspaces = ensure_private_child(boundary.directory(), Path::new("workspaces"))?;
    if workspaces.symlink_metadata(mission_id.as_str()).is_ok() {
        return Err(WorkspaceError::Collision(mission_id.to_string()));
    }

    let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
    let staging_name = format!(".creating-{}-{nonce}", std::process::id());
    let staging_path = Path::new(&staging_name);
    create_dir_private(&workspaces, staging_path).map_err(|source| WorkspaceError::Io {
        operation: "create workspace staging directory",
        source,
    })?;
    let staging = match open_dir_path_nofollow(&workspaces, staging_path) {
        Ok(staging) => staging,
        Err(source) => {
            cleanup_staging(&workspaces, staging_path);
            return Err(WorkspaceError::Io {
                operation: "open workspace staging directory",
                source,
            });
        }
    };

    if let Err(error) = build_base_layout(&staging, &seed) {
        drop(staging);
        cleanup_staging(&workspaces, staging_path);
        return Err(error);
    }
    if let Err(source) = sync_dir(&staging) {
        drop(staging);
        cleanup_staging(&workspaces, staging_path);
        return Err(WorkspaceError::Io {
            operation: "sync workspace staging directory",
            source,
        });
    }
    drop(staging);
    if let Err(source) = rustix::fs::renameat_with(
        &workspaces,
        staging_path,
        &workspaces,
        Path::new(mission_id.as_str()),
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
    {
        cleanup_staging(&workspaces, staging_path);
        return if source.kind() == std::io::ErrorKind::AlreadyExists {
            Err(WorkspaceError::Collision(mission_id.to_string()))
        } else {
            Err(WorkspaceError::Io {
                operation: "publish workspace without replacement",
                source,
            })
        };
    }
    sync_dir(&workspaces).map_err(|source| WorkspaceError::RecoveryRequired {
        operation: "sync workspaces directory",
        source,
    })?;
    post_publish(&workspaces, &mission_id).map_err(|source| {
        WorkspaceError::PublishedWorkspaceRequiresRecovery {
            source: Box::new(source),
        }
    })?;
    open_workspace_under(kind, mission_id, "open published workspace").map_err(|source| {
        WorkspaceError::PublishedWorkspaceRequiresRecovery {
            source: Box::new(source),
        }
    })
}

fn open_workspace_under(
    kind: WorkspaceKind,
    mission_id: MissionId,
    operation: &'static str,
) -> Result<WorkspaceAuthority, WorkspaceError> {
    let boundary = kind.shared_boundary();
    boundary.verify()?;
    let relative = Path::new("workspaces").join(mission_id.as_str());
    let directory = open_dir_path_nofollow(boundary.directory(), &relative)
        .map_err(|source| WorkspaceError::Io { operation, source })?;
    validate_base_layout(&directory)?;
    let metadata = directory
        .dir_metadata()
        .map_err(|source| WorkspaceError::Io {
            operation: "inspect workspace",
            source,
        })?;
    Ok(WorkspaceAuthority {
        id: mission_id,
        identity: identity(&metadata),
        directory,
        kind,
    })
}

impl WorkspaceAuthority {
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.id
    }

    pub(crate) fn verify(&self) -> Result<(), WorkspaceError> {
        let boundary = self.kind.boundary();
        boundary.verify()?;
        let current = open_dir_path_nofollow(
            boundary.directory(),
            &Path::new("workspaces").join(self.id.as_str()),
        )
        .map_err(|source| WorkspaceError::Io {
            operation: "reopen workspace",
            source,
        })?;
        if identity(
            &current
                .dir_metadata()
                .map_err(|source| WorkspaceError::Io {
                    operation: "inspect reopened workspace",
                    source,
                })?,
        ) != self.identity
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        Ok(())
    }

    pub(crate) fn process_cwd_binding(
        &self,
    ) -> Result<(Dir, FileIdentity, PathBuf), WorkspaceError> {
        self.verify()?;
        Ok((
            self.directory
                .try_clone()
                .map_err(|source| WorkspaceError::Io {
                    operation: "clone workspace process directory",
                    source,
                })?,
            self.identity,
            Path::new("workspaces").join(self.id.as_str()),
        ))
    }

    /// Derives the sole phase-worker binding accepted by process composition
    /// without creating any lazy workspace entry.
    ///
    /// The caller supplies only request facts. The worker identifier and both
    /// path bindings are derived here, and the request path is compared by
    /// exact Unix bytes before any lazy worker or phase directory is created.
    #[cfg(unix)]
    pub(crate) fn phase_worker_binding(
        &self,
        persona: &str,
        phase: &PhaseId,
        requested: &Path,
    ) -> Result<PhaseWorkerBinding, WorkspaceError> {
        self.verify()?;
        let worker_id = WorkerId::for_phase(persona, phase)
            .map_err(|_| WorkspaceError::ProcessWorkingRootMismatch)?;
        let root_relative = Path::new("workspaces")
            .join(self.id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        let canonical_path = self.kind.boundary().canonical_path().join(&root_relative);
        if requested.as_os_str().as_bytes() != canonical_path.as_os_str().as_bytes() {
            return Err(WorkspaceError::ProcessWorkingRootMismatch);
        }
        Ok(PhaseWorkerBinding {
            boundary: self.kind.shared_boundary(),
            workspace_id: self.id.clone(),
            workspace_directory: self.directory.try_clone().map_err(|source| {
                WorkspaceError::Io {
                    operation: "clone derived phase worker workspace",
                    source,
                }
            })?,
            workspace_identity: self.identity,
            phase_id: phase.clone(),
            worker_id,
            root_relative,
            canonical_path,
        })
    }

    pub(crate) fn shares_boundary(&self, boundary: &SharedCapabilityRoot) -> bool {
        Arc::ptr_eq(&self.kind.shared_boundary(), boundary)
    }

    /// Verifies both sealed capability roots and compares their canonical
    /// physical-root names.
    ///
    /// Pointer identity alone is insufficient for disjoint-writer checks:
    /// two independently enrolled boundaries can retain different `Arc`
    /// values while naming the same canonical directory.
    pub(crate) fn aliases_boundary_root(
        &self,
        boundary: &SharedCapabilityRoot,
    ) -> Result<bool, WorkspaceError> {
        self.verify()?;
        boundary.verify()?;
        Ok(self.kind.boundary().canonical_path() == boundary.canonical_path())
    }

    /// Borrows the exact fixture root carried by this workspace authority.
    ///
    /// Production workspaces fail closed before fixture-only callers can
    /// create worker state, artifact namespaces, or settings transactions.
    pub(crate) fn verified_fixture_boundary(
        &self,
    ) -> Result<&SharedCapabilityRoot, WorkspaceError> {
        self.verify()?;
        match &self.kind {
            WorkspaceKind::Fixture { boundary, .. } => Ok(boundary),
            WorkspaceKind::Production(_) => Err(WorkspaceError::FixtureCapabilityUnavailable),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            WorkspaceKind::HermeticCanary { boundary, .. } => Ok(boundary),
        }
    }

    /// Fills the seeded checkpoint's absent `plan` with the run's authored one.
    ///
    /// CF-M4a-4. `checkpoint.json` is seeded before a plan exists — the
    /// composition root admits the workspace at seal time, and resolution
    /// happens after — so the first durable checkpoint has `plan: null`. Go's
    /// `internal/cmd/status.go:63` dereferences `cp.Plan` with no nil check, so
    /// a rollback onto a Rust-authored workspace crashed the Go binary's
    /// `status` with a nil-pointer `SIGSEGV`. The Go nil check is TRK-1291 and
    /// belongs to the owner; the Rust side owes a Go-readable plan regardless,
    /// because "the field is optional" is not what Go's reader believes.
    ///
    /// This is deliberately **not** a general checkpoint writer. It accepts
    /// exactly two prior states — an absent plan, and the empty placeholder the
    /// composition root seeds so that even a crash between admission and
    /// dispatch leaves Go something to dereference — and refuses every plan
    /// that carries content. The operation is therefore monotone: placeholder
    /// to authored, exactly once. It re-encodes the decoded projection with
    /// only that one field changed, so no status, identity or
    /// forward-compatible key can ride along. A general publisher belongs to
    /// the leased [`FixtureProjectionWriter`], and this door cannot stand in
    /// for one.
    ///
    /// # Errors
    /// Returns [`WorkspaceError::InvalidCheckpoint`] when the retained
    /// checkpoint cannot be decoded or already carries a plan, and the usual
    /// I/O and identity failures otherwise.
    pub fn publish_authored_plan(
        &self,
        plan: &CheckpointPlan,
    ) -> Result<VerifiedCheckpointProjection, WorkspaceError> {
        self.verify()?;
        let path = Path::new("checkpoint.json");
        let file = open_verified_exact_base_entry(&self.directory, path)?;
        let current = read_bounded_from_open_file(file, MAX_SEED_BYTES).map_err(|source| {
            WorkspaceError::Io {
                operation: "read checkpoint before publishing the authored plan",
                source,
            }
        })?;
        let decoded = decode_checkpoint(&current)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        // The placeholder is recognised by content, not by byte equality with
        // `CheckpointPlan::default()`: the Go-compatible codec renders an
        // unset `created_at` as Go's zero time (`0001-01-01T00:00:00Z`), so a
        // seeded plan never decodes back to the Rust default.
        // A second run over a workspace a previous process left — the resume
        // path — finds its own plan already published. That is the crossed
        // crash boundary `reconcile_checkpoint` calls `AlreadyTarget`, not a
        // divergence: nothing is written and the retained bytes are returned.
        //
        // Sameness is the mission identity, the task, and the phase ids in
        // order — not byte equality. The plan's `created_at` belongs to the
        // run that first published it, and each phase's `status` belongs to
        // whatever has since advanced it; a republish that overwrote either
        // would lose durable progress to make a comparison succeed.
        if decoded.projection.plan.as_ref().is_some_and(|retained| {
            retained.id == plan.id
                && retained.task == plan.task
                && retained.phases.len() == plan.phases.len()
                && retained
                    .phases
                    .iter()
                    .zip(&plan.phases)
                    .all(|(retained, authored)| retained.id == authored.id)
        }) {
            return Ok(VerifiedCheckpointProjection::new(
                self.id.clone(),
                current,
                CheckpointReconciliationDisposition::AlreadyTarget,
            ));
        }
        if decoded.projection.plan.as_ref().is_some_and(|retained| {
            !retained.id.is_empty() || !retained.task.is_empty() || !retained.phases.is_empty()
        }) {
            return Err(WorkspaceError::InvalidCheckpoint(
                "the retained checkpoint already carries an authored plan; this door only fills \
                 the seeded placeholder"
                    .to_owned(),
            ));
        }
        let mut projection = decoded.projection;
        projection.plan = Some(plan.clone());
        let target = encode_current_checkpoint(&projection)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        atomic_replace_private(&self.directory, path, &target).map_err(|source| {
            WorkspaceError::RecoveryRequired {
                operation: "publish the authored plan into the checkpoint",
                source,
            }
        })?;
        self.verify()?;
        let readback_file = open_verified_exact_base_entry(&self.directory, path)?;
        let readback =
            read_bounded_from_open_file(readback_file, MAX_SEED_BYTES).map_err(|source| {
                WorkspaceError::Io {
                    operation: "read back the checkpoint carrying the authored plan",
                    source,
                }
            })?;
        if readback != target {
            return Err(WorkspaceError::ExactBaseDivergent {
                path: path.to_path_buf(),
            });
        }
        Ok(VerifiedCheckpointProjection::new(
            self.id.clone(),
            target,
            CheckpointReconciliationDisposition::Published,
        ))
    }

    /// Consumes this workspace authority and acquires its exclusive fixture
    /// projection writer.
    ///
    /// The writer is unavailable for production workspaces. A second writer
    /// for the same admitted workspace identity is rejected until the first is
    /// dropped.
    pub fn into_fixture_projection_writer(self) -> Result<FixtureProjectionWriter, WorkspaceError> {
        self.verify()?;
        let extras = match &self.kind {
            WorkspaceKind::Fixture { extras, .. } => Arc::clone(extras),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            WorkspaceKind::HermeticCanary { extras, .. } => Arc::clone(extras),
            WorkspaceKind::Production(_) => {
                return Err(WorkspaceError::FixtureCapabilityUnavailable);
            }
        };
        {
            let mut leases = lock_unpoisoned(&extras.projection_leases);
            if !leases.insert(self.identity) {
                return Err(WorkspaceError::ProjectionWriterLeased);
            }
        }
        Ok(FixtureProjectionWriter {
            core: self.into_projection_writer(ProjectionLease::Fixture(extras))?,
        })
    }

    /// Consumes an enrolled production workspace and acquires its exclusive
    /// projection writer. The retained production boundary owns the whole-home
    /// kernel lease; this additional lease prevents two actors in the same
    /// Rust process from publishing the same workspace concurrently.
    pub fn into_production_projection_writer(
        self,
    ) -> Result<ProductionProjectionWriter, WorkspaceError> {
        self.verify()?;
        let WorkspaceKind::Production(boundary) = &self.kind else {
            return Err(WorkspaceError::ProductionCapabilityUnavailable);
        };
        let boundary = Arc::clone(boundary);
        if !boundary.acquire_projection_lease(self.identity)? {
            return Err(WorkspaceError::ProjectionWriterLeased);
        }
        Ok(ProductionProjectionWriter {
            core: self.into_projection_writer(ProjectionLease::Production(boundary))?,
        })
    }

    /// Consumes a non-pristine, already-enrolled production workspace and
    /// acquires its exclusive projection writer by requiring the caller to
    /// prove knowledge of its exact durable state, rather than relying on
    /// [`Self::into_production_projection_writer`]'s pristine-shape proof.
    ///
    /// # Authority model
    ///
    /// [`Self::into_production_projection_writer`] mints a writer purely
    /// from on-disk shape: an empty event log and a checkpoint still
    /// reading `"pending"` are, by construction, proof enough that no prior
    /// actor holds conflicting intent for this workspace. Once a mission's
    /// checkpoint has ever advanced, that shape proof is gone for good — the
    /// disk alone can no longer distinguish "safe to resume writing" from
    /// "some other writer's history that must not be silently adopted".
    ///
    /// This constructor closes that gap with a different proof: the caller
    /// supplies a [`JournalCheckpointExpectation`], which can only be minted
    /// for the exact canonical checkpoint fixed point independently
    /// reconstructed from journal-reduced state. This constructor's only job
    /// is to verify that proof against disk, through the identical single-fd
    /// open+fstat(mode `0600`, exactly one link, is a regular file)+read
    /// discipline [`ProductionProjectionWriter::verify_exact_base`] and
    /// [`ProductionProjectionWriter::reconcile_checkpoint`] use
    /// ([`open_verified_exact_base_entry`]), before granting write
    /// authority:
    ///
    /// - the proof must belong to this exact workspace mission;
    /// - `checkpoint.json` must be byte-identical to the proof's bytes;
    /// - the workspace-local `events.jsonl` — which no composition in this
    ///   crate ever writes for a production workspace, only for an isolated
    ///   fixture — must be **absent, or present and exactly empty**. Any
    ///   other content there belonged to a fixture-lane writer; it is not
    ///   this constructor's history to adopt, so recovery fails closed
    ///   rather than silently discarding or overwriting it.
    ///
    /// On success, the returned writer's retained expected event log and
    /// checkpoint are seeded from this verified on-disk state — never from
    /// the proof beyond the byte-compare that just proved it true. Any
    /// divergence (wrong mission, wrong bytes, wrong shape, or foreign
    /// event-log history) is a typed error with nothing mutated and no writer
    /// minted. Lease acquisition and release are the exact
    /// production lease path [`Self::into_production_projection_writer`]
    /// uses — there is no second lease mechanism, so a concurrent writer for
    /// the same workspace identity is rejected exactly as it would be for
    /// the pristine constructor.
    ///
    /// ## Root-of-trust boundary
    ///
    /// The byte-compare proves crash-consistency **within one journal
    /// timeline**; it is not, by itself, proof that the on-disk checkpoint
    /// was never superseded. A workspace file rolled back to an earlier
    /// valid state is indistinguishable here from a legitimate
    /// crash-before-write. That is safe because write targets are always
    /// re-derived from the RuntimeStore journal via pure reduction — the
    /// journal, not this file, is the root of trust — so the worst a
    /// rollback can induce is an idempotent re-publish of the correct
    /// journal-derived state, and the projector's final replay-vs-disk
    /// comparison fails closed on any residual divergence.
    pub(crate) fn into_production_projection_writer_recovered(
        self,
        expectation: JournalCheckpointExpectation,
    ) -> Result<ProductionProjectionWriter, WorkspaceError> {
        self.verify()?;
        let WorkspaceKind::Production(boundary) = &self.kind else {
            return Err(WorkspaceError::ProductionCapabilityUnavailable);
        };
        if expectation.mission_id() != self.mission_id() {
            return Err(WorkspaceError::CheckpointMissionMismatch {
                expected: self.mission_id().as_str().to_owned(),
                found: expectation.mission_id().as_str().to_owned(),
            });
        }
        let boundary = Arc::clone(boundary);
        if !boundary.acquire_projection_lease(self.identity)? {
            return Err(WorkspaceError::ProjectionWriterLeased);
        }
        // The lease is held in `writer.lease` from this point on: any early
        // `?`/`return Err` below drops `writer`, releasing it via
        // `ProjectionWriterCore`'s `Drop` impl — the exact same
        // acquire-then-drop-on-failure shape
        // [`Self::into_production_projection_writer`] relies on via
        // `into_projection_writer`.
        let mut writer = ProjectionWriterCore {
            workspace: self,
            lease: ProjectionLease::Production(boundary),
            expected_event_log: Vec::new(),
            expected_checkpoint: Vec::new(),
        };
        let on_disk_checkpoint = writer.workspace.verified_lifecycle_checkpoint_bytes()?;
        if on_disk_checkpoint != expectation.checkpoint_bytes() {
            return Err(WorkspaceError::ExactBaseDivergent {
                path: PathBuf::from("checkpoint.json"),
            });
        }
        let on_disk_event_log = verified_recovery_event_log_bytes(&writer.workspace.directory)?;
        if !on_disk_event_log.is_empty() {
            return Err(WorkspaceError::ForeignEventLogHistory);
        }
        writer.expected_checkpoint = on_disk_checkpoint;
        writer.expected_event_log = on_disk_event_log;
        writer.workspace.confirm_lifecycle_projection(
            &writer.expected_event_log,
            &writer.expected_checkpoint,
        )?;
        Ok(ProductionProjectionWriter { core: writer })
    }

    fn into_projection_writer(
        self,
        lease: ProjectionLease,
    ) -> Result<ProjectionWriterCore, WorkspaceError> {
        let mut writer = ProjectionWriterCore {
            workspace: self,
            lease,
            expected_event_log: Vec::new(),
            expected_checkpoint: Vec::new(),
        };
        writer.expected_event_log = writer.workspace.lifecycle_event_log_bytes()?;
        writer.expected_checkpoint = writer.workspace.lifecycle_checkpoint_bytes()?;
        writer.workspace.confirm_lifecycle_projection(
            &writer.expected_event_log,
            &writer.expected_checkpoint,
        )?;
        if matches!(&writer.lease, ProjectionLease::Production(_)) {
            let checkpoint = decode_checkpoint(&writer.expected_checkpoint)
                .map_err(|_| WorkspaceError::ProductionProjectionRecoveryUnavailable)?
                .projection;
            let pristine = writer.expected_event_log.is_empty()
                && checkpoint.workspace_id == writer.workspace.mission_id().as_str()
                && checkpoint.status == "pending"
                && checkpoint
                    .plan
                    .as_ref()
                    .is_none_or(|plan| plan.phases.iter().all(|phase| phase.status == "pending"));
            if !pristine {
                return Err(WorkspaceError::ProductionProjectionRecoveryUnavailable);
            }
        }
        Ok(writer)
    }

    /// Fixture-only leasing sidecar. Returns an error for production-backed
    /// workspaces, which carry no extras and must never reach the settings
    /// overlay path.
    pub(crate) fn extras(&self) -> Result<&Arc<FixtureExtras>, WorkspaceError> {
        match &self.kind {
            WorkspaceKind::Fixture { extras, .. } => Ok(extras),
            WorkspaceKind::Production(_) => Err(WorkspaceError::FixtureCapabilityUnavailable),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            WorkspaceKind::HermeticCanary { extras, .. } => Ok(extras),
        }
    }

    /// Atomically creates a production workspace beneath an enrolled boundary.
    /// The boundary can only originate from retained writer authority, so this
    /// constructor cannot be used with a fixture or read-only resolved home.
    pub fn create_production(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
        seed: WorkspaceSeed,
    ) -> Result<Self, WorkspaceError> {
        boundary.verify()?;
        create_workspace_under(WorkspaceKind::Production(boundary), mission_id, seed)
    }

    /// Admits a previously-created production workspace beneath an enrolled
    /// production capability root. The workspace directory must already exist
    /// with the validated base layout; admission performs no mutation.
    /// Production workspaces carry no fixture leasing extras.
    ///
    /// The boundary is a concrete [`ProductionBoundary`] (held in an `Arc` so it
    /// can be shared as a sealed [`SharedCapabilityRoot`]). Because
    /// [`ProductionBoundary`] has no public constructor that bypasses enrollment
    /// and [`CapabilityRoot`] is sealed, an external caller cannot forge this
    /// argument or substitute a fixture root.
    pub fn admit_production(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
    ) -> Result<Self, WorkspaceError> {
        boundary.verify()?;
        open_workspace_under(
            WorkspaceKind::Production(boundary),
            mission_id,
            "open production workspace",
        )
    }

    /// Atomically creates a disposable verification-canary workspace beneath
    /// the fixed compatibility target derived from the v1 canary layout.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    pub(crate) fn create_hermetic_canary(
        compatibility_home: Arc<ProductionBoundary>,
        mission_id: MissionId,
        seed: WorkspaceSeed,
    ) -> Result<Self, WorkspaceError> {
        compatibility_home.verify()?;
        let extras = compatibility_home.hermetic_canary_extras()?;
        let boundary: SharedCapabilityRoot = compatibility_home;
        create_workspace_under(
            WorkspaceKind::HermeticCanary { boundary, extras },
            mission_id,
            seed,
        )
    }

    /// Admits a previously-created hermetic-canary workspace beneath the same
    /// fixed compatibility target. Performs no mutation.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    pub(crate) fn admit_hermetic_canary(
        compatibility_home: Arc<ProductionBoundary>,
        mission_id: MissionId,
    ) -> Result<Self, WorkspaceError> {
        compatibility_home.verify()?;
        let extras = compatibility_home.hermetic_canary_extras()?;
        let boundary: SharedCapabilityRoot = compatibility_home;
        open_workspace_under(
            WorkspaceKind::HermeticCanary { boundary, extras },
            mission_id,
            "open hermetic-canary workspace",
        )
    }

    pub fn inspect(&self) -> Result<WorkspaceHealth, WorkspaceError> {
        self.verify()?;
        validate_base_layout(&self.directory)?;
        let mut sidecars = BTreeMap::new();
        for sidecar in OptionalSidecar::ALL {
            let name = Path::new(sidecar.name());
            let bytes =
                read_bounded_nofollow(&self.directory, name, MAX_SEED_BYTES).map_err(|source| {
                    WorkspaceError::Io {
                        operation: "read optional sidecar",
                        source,
                    }
                })?;
            let state = match bytes {
                None => SidecarState::Missing,
                Some(bytes) => {
                    validate_private_file(&self.directory, name)?;
                    let degraded = match sidecar {
                        OptionalSidecar::LinearIssueId => trim_ascii(&bytes).is_empty(),
                        OptionalSidecar::MissionPath => {
                            let path = std::str::from_utf8(trim_ascii(&bytes)).ok().map(Path::new);
                            !matches!(path, Some(path) if path.is_absolute() && path.exists())
                        }
                        _ => false,
                    };
                    if degraded {
                        SidecarState::Degraded(bytes)
                    } else {
                        SidecarState::Present(bytes)
                    }
                }
            };
            sidecars.insert(sidecar, state);
        }
        Ok(WorkspaceHealth { sidecars })
    }

    pub fn set_sidecar(
        &self,
        sidecar: OptionalSidecar,
        bytes: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.verify()?;
        if bytes.len() > MAX_SEED_BYTES || bytes.contains(&0) {
            return Err(WorkspaceError::InvalidSidecar(sidecar));
        }
        validate_replace_target(&self.directory, Path::new(sidecar.name()))?;
        atomic_replace_private(&self.directory, Path::new(sidecar.name()), bytes).map_err(
            |source| WorkspaceError::Io {
                operation: "replace optional sidecar",
                source,
            },
        )
    }

    pub fn prepare_worker(&self, worker: &WorkerId) -> Result<(), WorkspaceError> {
        self.verify()?;
        let workers =
            open_dir_path_nofollow(&self.directory, Path::new("workers")).map_err(|source| {
                WorkspaceError::Io {
                    operation: "open workers directory",
                    source,
                }
            })?;
        let worker_dir = ensure_private_child(&workers, Path::new(worker.as_str()))?;
        let claude = ensure_private_child(&worker_dir, Path::new(".claude"))?;
        let _hooks = ensure_private_child(&claude, Path::new("hooks"))?;
        Ok(())
    }

    pub fn prepare_phase(&self, worker: &WorkerId, phase: &PhaseId) -> Result<(), WorkspaceError> {
        self.prepare_worker(worker)?;
        let artifacts =
            open_dir_path_nofollow(&self.directory, Path::new("artifacts")).map_err(|source| {
                WorkspaceError::Io {
                    operation: "open artifacts directory",
                    source,
                }
            })?;
        let _artifact = ensure_private_child(&artifacts, Path::new(phase.as_str()))?;
        let scratch = ensure_private_child(&self.directory, Path::new("scratch"))?;
        let _phase = ensure_private_child(&scratch, Path::new(phase.as_str()))?;
        Ok(())
    }

    pub fn worker_file(
        &self,
        worker: &WorkerId,
        kind: WorkerFileKind,
    ) -> Result<WorkspaceFileAuthority, WorkspaceError> {
        self.prepare_worker(worker)?;
        self.file_authority(
            Path::new("workers").join(worker.as_str()),
            kind.name().to_owned(),
            false,
        )
    }

    pub fn worker_stop_hook(
        &self,
        worker: &WorkerId,
    ) -> Result<WorkspaceFileAuthority, WorkspaceError> {
        self.prepare_worker(worker)?;
        self.file_authority(
            Path::new("workers")
                .join(worker.as_str())
                .join(".claude/hooks"),
            "stop.sh".to_owned(),
            true,
        )
    }

    pub fn learning_file(
        &self,
        worker: &WorkerId,
    ) -> Result<WorkspaceFileAuthority, WorkspaceError> {
        self.verify()?;
        self.file_authority(
            PathBuf::from("learnings"),
            format!("{}.json", worker.as_str()),
            false,
        )
    }

    pub fn scratch_notes(&self, phase: &PhaseId) -> Result<WorkspaceFileAuthority, WorkspaceError> {
        self.verify()?;
        let scratch = ensure_private_child(&self.directory, Path::new("scratch"))?;
        let _phase = ensure_private_child(&scratch, Path::new(phase.as_str()))?;
        self.file_authority(
            Path::new("scratch").join(phase.as_str()),
            "notes.md".to_owned(),
            false,
        )
    }

    fn file_authority(
        &self,
        relative_parent: PathBuf,
        name: String,
        executable: bool,
    ) -> Result<WorkspaceFileAuthority, WorkspaceError> {
        self.verify()?;
        let parent =
            open_dir_path_nofollow(&self.directory, &relative_parent).map_err(|source| {
                WorkspaceError::Io {
                    operation: "open lazy-file parent",
                    source,
                }
            })?;
        let parent_identity =
            identity(&parent.dir_metadata().map_err(|source| WorkspaceError::Io {
                operation: "inspect lazy-file parent",
                source,
            })?);
        Ok(WorkspaceFileAuthority {
            boundary: self.kind.shared_boundary(),
            workspace_id: self.id.clone(),
            workspace_identity: self.identity,
            parent,
            parent_relative: Path::new("workspaces")
                .join(self.id.as_str())
                .join(relative_parent),
            parent_identity,
            name,
            executable,
        })
    }

    fn commit_lifecycle_event_checkpoint(
        &self,
        event_without_newline: &[u8],
        expected_event_log: &[u8],
        expected_old_checkpoint: &[u8],
        checkpoint: &[u8],
        fault: Option<LifecycleProjectionFault>,
    ) -> Result<(), WorkspaceError> {
        self.commit_event_checkpoint_inner(
            event_without_newline,
            expected_event_log,
            expected_old_checkpoint,
            checkpoint,
            fault,
        )
    }

    fn commit_event_checkpoint_inner(
        &self,
        event_without_newline: &[u8],
        expected_event_log: &[u8],
        expected_old_checkpoint: &[u8],
        checkpoint: &[u8],
        fault: Option<LifecycleProjectionFault>,
    ) -> Result<(), WorkspaceError> {
        let publication_started = std::cell::Cell::new(false);
        match self.commit_event_checkpoint_unclassified(
            event_without_newline,
            expected_event_log,
            expected_old_checkpoint,
            checkpoint,
            fault,
            &publication_started,
        ) {
            Err(
                error @ (WorkspaceError::InjectedProjectionFault(_)
                | WorkspaceError::RecoveryRequired { .. }),
            ) => Err(error),
            Err(_) if publication_started.get() => Err(WorkspaceError::ProjectionIndeterminate {
                operation: "acknowledge published fixture lifecycle projection",
            }),
            result => result,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_event_checkpoint_unclassified(
        &self,
        event_without_newline: &[u8],
        expected_event_log: &[u8],
        expected_old_checkpoint: &[u8],
        checkpoint: &[u8],
        fault: Option<LifecycleProjectionFault>,
        publication_started: &std::cell::Cell<bool>,
    ) -> Result<(), WorkspaceError> {
        self.verify()?;
        let decoded = decode_checkpoint(checkpoint)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        if decoded.projection.workspace_id != self.id.as_str() {
            return Err(WorkspaceError::CheckpointMissionMismatch {
                expected: self.id.to_string(),
                found: decoded.projection.workspace_id,
            });
        }
        let mut history = read_bounded_nofollow(
            &self.directory,
            Path::new("events.jsonl"),
            MAX_EVENT_LOG_BYTES,
        )
        .map_err(|source| WorkspaceError::Io {
            operation: "read fixture event history",
            source,
        })?
        .unwrap_or_default();
        if !history.is_empty() && !history.ends_with(b"\n") {
            return Err(WorkspaceError::UnterminatedEventLog);
        }
        let event = event_without_newline
            .strip_suffix(b"\n")
            .unwrap_or(event_without_newline);
        if event.is_empty()
            || event.len() > MAX_SEED_BYTES
            || event.contains(&b'\n')
            || event.contains(&b'\r')
        {
            return Err(WorkspaceError::InvalidFixtureEvent);
        }
        let decoded_event =
            decode_event_line(event).map_err(|_| WorkspaceError::InvalidFixtureEvent)?;
        if decoded_event.record.mission_id != self.id.as_str() {
            return Err(WorkspaceError::InvalidFixtureEvent);
        }
        let mut expected_tail = event.to_vec();
        expected_tail.push(b'\n');
        let mut published = expected_event_log.to_vec();
        published.extend_from_slice(&expected_tail);
        let expected_already_ends_with_event = expected_event_log.ends_with(&expected_tail)
            && (expected_event_log.len() == expected_tail.len()
                || expected_event_log.get(
                    expected_event_log
                        .len()
                        .saturating_sub(expected_tail.len() + 1),
                ) == Some(&b'\n'));
        if history != expected_event_log
            && (expected_already_ends_with_event || history != published)
        {
            return Err(WorkspaceError::ProjectionConflict);
        }
        validate_event_checkpoint_pair(&decoded_event.record, &decoded.projection)?;
        let current_checkpoint = read_bounded_nofollow(
            &self.directory,
            Path::new("checkpoint.json"),
            MAX_SEED_BYTES,
        )
        .map_err(|source| WorkspaceError::Io {
            operation: "inspect current fixture checkpoint",
            source,
        })?;
        let exact_tail = history.ends_with(&expected_tail)
            && (history.len() == expected_tail.len()
                || history.get(history.len().saturating_sub(expected_tail.len() + 1))
                    == Some(&b'\n'));
        let event_temp = Path::new(".events.jsonl.projection-tmp");
        let checkpoint_temp = Path::new(".checkpoint.json.projection-tmp");
        if exact_tail {
            publication_started.set(true);
        }
        if exact_tail && current_checkpoint.as_deref() == Some(checkpoint) {
            if fault == Some(LifecycleProjectionFault::CheckpointRenamed) {
                return Err(WorkspaceError::InjectedProjectionFault(
                    "after-checkpoint-rename",
                ));
            }
            return self.acknowledge_projection(
                &history,
                checkpoint,
                "resync acknowledged fixture projection",
            );
        }
        if exact_tail {
            if fault == Some(LifecycleProjectionFault::EventSynced) {
                return Err(WorkspaceError::InjectedProjectionFault("after-event-sync"));
            }
            if current_checkpoint.as_deref() != Some(expected_old_checkpoint) {
                return Err(WorkspaceError::ProjectionConflict);
            }
            match read_bounded_nofollow(&self.directory, checkpoint_temp, MAX_SEED_BYTES).map_err(
                |source| WorkspaceError::Io {
                    operation: "inspect staged checkpoint during exact-event recovery",
                    source,
                },
            )? {
                Some(staged) if staged == checkpoint => self
                    .directory
                    .rename(
                        checkpoint_temp,
                        &self.directory,
                        Path::new("checkpoint.json"),
                    )
                    .map_err(|source| WorkspaceError::RecoveryRequired {
                        operation: "publish staged checkpoint after exact event",
                        source,
                    })?,
                Some(_) => return Err(WorkspaceError::ProjectionConflict),
                None => atomic_replace_private(
                    &self.directory,
                    Path::new("checkpoint.json"),
                    checkpoint,
                )
                .map_err(|source| WorkspaceError::RecoveryRequired {
                    operation: "repair lost staged checkpoint after exact event",
                    source,
                })?,
            }
            return self.acknowledge_projection(
                &history,
                checkpoint,
                "sync repaired checkpoint after exact event",
            );
        }
        if current_checkpoint.as_deref() != Some(expected_old_checkpoint) {
            return Err(WorkspaceError::ProjectionConflict);
        }
        let scan = orchestrator_core::scan_event_log(&history);
        if scan.events.iter().any(|existing| {
            existing.record.id == decoded_event.record.id
                || existing.record.sequence == decoded_event.record.sequence
        }) {
            return Err(WorkspaceError::ProjectionConflict);
        }
        if history.len().saturating_add(event.len()).saturating_add(1) > MAX_EVENT_LOG_BYTES {
            return Err(WorkspaceError::EventLogTooLarge);
        }
        history.extend_from_slice(event);
        history.push(b'\n');

        // Stage both complete values before publishing either one.
        let existing_checkpoint_temp =
            read_bounded_nofollow(&self.directory, checkpoint_temp, MAX_SEED_BYTES).map_err(
                |source| WorkspaceError::Io {
                    operation: "inspect staged fixture checkpoint",
                    source,
                },
            )?;
        if let Some(staged) = existing_checkpoint_temp {
            if staged != checkpoint {
                return Err(WorkspaceError::ProjectionConflict);
            }
            let current = read_bounded_nofollow(
                &self.directory,
                Path::new("events.jsonl"),
                MAX_EVENT_LOG_BYTES,
            )
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect partially published fixture event",
                source,
            })?
            .unwrap_or_default();
            if current.ends_with(&expected_tail) {
                publication_started.set(true);
                self.directory
                    .rename(
                        checkpoint_temp,
                        &self.directory,
                        Path::new("checkpoint.json"),
                    )
                    .map_err(|source| WorkspaceError::Io {
                        operation: "recover fixture checkpoint after event",
                        source,
                    })?;
                return self.acknowledge_projection(
                    &current,
                    checkpoint,
                    "sync recovered fixture checkpoint",
                );
            }
        }

        match read_bounded_nofollow(&self.directory, event_temp, MAX_EVENT_LOG_BYTES).map_err(
            |source| WorkspaceError::Io {
                operation: "inspect staged fixture event log",
                source,
            },
        )? {
            Some(staged) if staged != history => return Err(WorkspaceError::ProjectionConflict),
            Some(_) => {}
            None => create_private_file(&self.directory, event_temp, &history, false).map_err(
                |source| WorkspaceError::Io {
                    operation: "stage fixture event log",
                    source,
                },
            )?,
        }
        match read_bounded_nofollow(&self.directory, checkpoint_temp, MAX_SEED_BYTES).map_err(
            |source| WorkspaceError::Io {
                operation: "inspect staged fixture checkpoint",
                source,
            },
        )? {
            Some(staged) if staged != checkpoint => {
                return Err(WorkspaceError::ProjectionConflict);
            }
            Some(_) => {}
            None => {
                if let Err(source) =
                    create_private_file(&self.directory, checkpoint_temp, checkpoint, false)
                {
                    let _ = self.directory.remove_file(event_temp);
                    return Err(WorkspaceError::Io {
                        operation: "stage fixture checkpoint",
                        source,
                    });
                }
            }
        }
        publication_started.set(true);
        if let Err(source) =
            self.directory
                .rename(event_temp, &self.directory, Path::new("events.jsonl"))
        {
            let _ = self.directory.remove_file(event_temp);
            let _ = self.directory.remove_file(checkpoint_temp);
            return Err(WorkspaceError::Io {
                operation: "publish fixture event log",
                source,
            });
        }
        sync_dir(&self.directory).map_err(|source| WorkspaceError::RecoveryRequired {
            operation: "sync published fixture event log",
            source,
        })?;
        if fault == Some(LifecycleProjectionFault::EventSynced) {
            return Err(WorkspaceError::InjectedProjectionFault("after-event-sync"));
        }
        self.directory
            .rename(
                checkpoint_temp,
                &self.directory,
                Path::new("checkpoint.json"),
            )
            .map_err(|source| WorkspaceError::RecoveryRequired {
                operation: "publish fixture checkpoint after event",
                source,
            })?;
        if fault == Some(LifecycleProjectionFault::CheckpointRenamed) {
            return Err(WorkspaceError::InjectedProjectionFault(
                "after-checkpoint-rename",
            ));
        }
        if fault == Some(LifecycleProjectionFault::CheckpointRenamedWithConflict) {
            atomic_replace_private(
                &self.directory,
                Path::new("events.jsonl"),
                b"fixture-conflicting-history\n",
            )
            .map_err(|source| WorkspaceError::Io {
                operation: "inject post-publication projection conflict",
                source,
            })?;
        }
        if fault == Some(LifecycleProjectionFault::CheckpointRenamedWithIdentityChange) {
            let workspace = Path::new("workspaces").join(self.id.as_str());
            let displaced = Path::new("workspaces")
                .join(format!(".{}.projection-identity-fault", self.id.as_str()));
            let boundary = self.kind.boundary();
            boundary
                .directory()
                .rename(&workspace, boundary.directory(), &displaced)
                .map_err(|source| WorkspaceError::Io {
                    operation: "inject post-publication workspace displacement",
                    source,
                })?;
            create_dir_private(boundary.directory(), &workspace).map_err(|source| {
                WorkspaceError::Io {
                    operation: "inject post-publication workspace replacement",
                    source,
                }
            })?;
        }
        self.acknowledge_projection(&history, checkpoint, "sync published fixture checkpoint")
    }

    fn acknowledge_projection(
        &self,
        expected_event_log: &[u8],
        expected_checkpoint: &[u8],
        operation: &'static str,
    ) -> Result<(), WorkspaceError> {
        sync_dir(&self.directory)
            .map_err(|source| WorkspaceError::RecoveryRequired { operation, source })?;
        self.verify()?;
        if self.lifecycle_event_log_bytes()? != expected_event_log
            || self.lifecycle_checkpoint_bytes()? != expected_checkpoint
        {
            return Err(WorkspaceError::ProjectionConflict);
        }
        self.verify()
    }

    pub(crate) fn confirm_lifecycle_projection(
        &self,
        expected_event_log: &[u8],
        expected_checkpoint: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.acknowledge_projection(
            expected_event_log,
            expected_checkpoint,
            "confirm lifecycle projection before restart acknowledgement",
        )
    }

    pub(crate) fn lifecycle_checkpoint_bytes(&self) -> Result<Vec<u8>, WorkspaceError> {
        self.verify()?;
        read_bounded_nofollow(
            &self.directory,
            Path::new("checkpoint.json"),
            MAX_SEED_BYTES,
        )
        .map_err(|source| WorkspaceError::Io {
            operation: "read lifecycle checkpoint",
            source,
        })?
        .ok_or(WorkspaceError::InvalidKnownEntry {
            path: PathBuf::from("checkpoint.json"),
        })
    }

    /// Reads `checkpoint.json` through the same single-fd open+fstat(is a
    /// regular file, mode `0600`, exactly one link)+read discipline
    /// [`ProductionProjectionWriter::verify_exact_base`] and
    /// [`ProductionProjectionWriter::reconcile_checkpoint`] use, rather than
    /// [`Self::lifecycle_checkpoint_bytes`]'s weaker no-follow-only read.
    ///
    /// Intended for the no-writer-available fallback compatibility paths
    /// (Cell 2D's hermetic projector) that mint a compatibility receipt
    /// without ever acquiring a [`ProductionProjectionWriter`] — those
    /// receipts should attest the same shape guarantee a full writer
    /// verification would, not a weaker one. A hard-linked, wrong-mode, or
    /// symlinked `checkpoint.json` is rejected without being read.
    pub(crate) fn verified_lifecycle_checkpoint_bytes(&self) -> Result<Vec<u8>, WorkspaceError> {
        self.verify()?;
        let path = Path::new("checkpoint.json");
        let file = open_verified_exact_base_entry(&self.directory, path)?;
        let bytes = read_bounded_from_open_file(file, MAX_SEED_BYTES).map_err(|source| {
            WorkspaceError::Io {
                operation: "read verified lifecycle checkpoint",
                source,
            }
        })?;
        // Re-verify after the read: a concurrent replacement of the
        // workspace directory itself during the read must still fail closed
        // rather than let a stale-content receipt be minted from it.
        self.verify()?;
        Ok(bytes)
    }

    pub(crate) fn lifecycle_event_log_bytes(&self) -> Result<Vec<u8>, WorkspaceError> {
        self.verify()?;
        Ok(read_bounded_nofollow(
            &self.directory,
            Path::new("events.jsonl"),
            MAX_EVENT_LOG_BYTES,
        )
        .map_err(|source| WorkspaceError::Io {
            operation: "read lifecycle event log",
            source,
        })?
        .unwrap_or_default())
    }

    pub(crate) fn worker_directory(&self, worker: &WorkerId) -> Result<Dir, WorkspaceError> {
        self.verify()?;
        open_dir_path_nofollow(&self.directory, &Path::new("workers").join(worker.as_str()))
            .map_err(|source| WorkspaceError::Io {
                operation: "open worker directory",
                source,
            })
    }
}

impl ProjectionWriterCore {
    fn mission_id(&self) -> &MissionId {
        self.workspace.mission_id()
    }

    fn commit_event_checkpoint(
        &mut self,
        event_without_newline: &[u8],
        checkpoint: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.commit_event_checkpoint_with_fault(event_without_newline, checkpoint, None)
    }

    fn commit_event_checkpoint_with_fault(
        &mut self,
        event_without_newline: &[u8],
        checkpoint: &[u8],
        fault: Option<LifecycleProjectionFault>,
    ) -> Result<(), WorkspaceError> {
        self.workspace.commit_lifecycle_event_checkpoint(
            event_without_newline,
            &self.expected_event_log,
            &self.expected_checkpoint,
            checkpoint,
            fault,
        )?;
        let event = event_without_newline
            .strip_suffix(b"\n")
            .unwrap_or(event_without_newline);
        let mut tail = event.to_vec();
        tail.push(b'\n');
        if !self.expected_event_log.ends_with(&tail) {
            self.expected_event_log.extend_from_slice(&tail);
        }
        self.expected_checkpoint.clear();
        self.expected_checkpoint.extend_from_slice(checkpoint);
        Ok(())
    }

    fn confirm_lifecycle_projection(&self) -> Result<(), WorkspaceError> {
        self.workspace
            .confirm_lifecycle_projection(&self.expected_event_log, &self.expected_checkpoint)
    }

    fn lifecycle_checkpoint_bytes(&self) -> &[u8] {
        &self.expected_checkpoint
    }

    fn lifecycle_event_log_bytes(&self) -> &[u8] {
        &self.expected_event_log
    }
}

impl FixtureProjectionWriter {
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        self.core.mission_id()
    }

    /// Publishes one event and its checkpoint from this writer's exact prior
    /// state. Exact replay after a partial publication repairs the checkpoint;
    /// every divergent, duplicate, or concurrent history is rejected.
    pub fn commit_event_checkpoint(
        &mut self,
        event_without_newline: &[u8],
        checkpoint: &[u8],
    ) -> Result<(), WorkspaceError> {
        self.core
            .commit_event_checkpoint(event_without_newline, checkpoint)
    }

    pub(crate) fn commit_event_checkpoint_with_fault(
        &mut self,
        event_without_newline: &[u8],
        checkpoint: &[u8],
        fault: Option<LifecycleProjectionFault>,
    ) -> Result<(), WorkspaceError> {
        self.core
            .commit_event_checkpoint_with_fault(event_without_newline, checkpoint, fault)
    }

    pub(crate) fn confirm_lifecycle_projection(&self) -> Result<(), WorkspaceError> {
        self.core.confirm_lifecycle_projection()
    }

    pub(crate) fn lifecycle_checkpoint_bytes(&self) -> &[u8] {
        self.core.lifecycle_checkpoint_bytes()
    }

    pub(crate) fn lifecycle_event_log_bytes(&self) -> &[u8] {
        self.core.lifecycle_event_log_bytes()
    }
}

impl ProductionProjectionWriter {
    /// Returns the mission whose publication authority is reserved.
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        self.core.mission_id()
    }

    /// Verifies `mission.md`, `plan.json`, and `checkpoint.json` are byte-exact
    /// against the caller's expected content, through this writer's retained
    /// no-follow production authority.
    ///
    /// `expected_plan` is encoded with [`encode_current_plan`] so `plan.json`
    /// can never drift from the checkpoint-plan vocabulary a caller derives
    /// its expectation from. Any divergent byte, missing file, wrong mode, or
    /// hard-linked entry fails closed with no filesystem mutation; nothing is
    /// ever repaired or overwritten by this method.
    pub fn verify_exact_base(
        &self,
        expected_mission: &[u8],
        expected_plan: &CheckpointPlan,
        expected_checkpoint: &[u8],
    ) -> Result<VerifiedWorkspaceBase, WorkspaceError> {
        let expected_plan_bytes = encode_current_plan(expected_plan)
            .map_err(|error| WorkspaceError::InvalidCheckpoint(error.to_string()))?;
        self.core.workspace.verify()?;
        for (name, expected) in [
            ("mission.md", expected_mission),
            ("plan.json", expected_plan_bytes.as_slice()),
            ("checkpoint.json", expected_checkpoint),
        ] {
            let path = Path::new(name);
            let file = open_verified_exact_base_entry(&self.core.workspace.directory, path)?;
            let actual = read_bounded_from_open_file(file, MAX_SEED_BYTES).map_err(|source| {
                WorkspaceError::Io {
                    operation: "read exact base file for verification",
                    source,
                }
            })?;
            if actual != expected {
                return Err(WorkspaceError::ExactBaseDivergent {
                    path: path.to_path_buf(),
                });
            }
        }
        // Re-verify after every read: a concurrent replacement of the
        // workspace directory itself during the read loop must still fail
        // closed rather than mint a base receipt for stale content.
        self.core.workspace.verify()?;
        Ok(VerifiedWorkspaceBase::new(self.core.mission_id().clone()))
    }

    /// Reconciles `checkpoint.json` to `target`'s exact bytes.
    ///
    /// Publication happens only when the current retained bytes equal
    /// `expected_prior` exactly. Current bytes already equal to `target` are
    /// treated as a crossed crash boundary — a previous publish whose
    /// acknowledgement was lost — and are verified without a further write.
    /// Any other retained content is a divergence: it is rejected and never
    /// overwritten.
    pub fn reconcile_checkpoint(
        &self,
        expected_prior: &[u8],
        target: &[u8],
    ) -> Result<VerifiedCheckpointProjection, WorkspaceError> {
        self.core.workspace.verify()?;
        let path = Path::new("checkpoint.json");
        // Degenerate case (`expected_prior == target`): the `current != target`
        // branch below is unreachable once `current` is read through the
        // shape-verified fd, so the only way this call can succeed is the
        // exact-target no-op readback at the bottom — there is no write path
        // for a caller that asks to reconcile a checkpoint to its own prior
        // value.
        let file = open_verified_exact_base_entry(&self.core.workspace.directory, path)?;
        let current = read_bounded_from_open_file(file, MAX_SEED_BYTES).map_err(|source| {
            WorkspaceError::Io {
                operation: "read checkpoint before reconcile",
                source,
            }
        })?;
        let disposition = if current == target {
            CheckpointReconciliationDisposition::AlreadyTarget
        } else {
            if current != expected_prior {
                return Err(WorkspaceError::ExactBaseDivergent {
                    path: path.to_path_buf(),
                });
            }
            atomic_replace_private(&self.core.workspace.directory, path, target).map_err(
                |source| WorkspaceError::RecoveryRequired {
                    operation: "publish reconciled checkpoint",
                    source,
                },
            )?;
            CheckpointReconciliationDisposition::Published
        };
        self.core.workspace.verify()?;
        let readback_file = open_verified_exact_base_entry(&self.core.workspace.directory, path)?;
        let readback =
            read_bounded_from_open_file(readback_file, MAX_SEED_BYTES).map_err(|source| {
                WorkspaceError::Io {
                    operation: "read back reconciled checkpoint",
                    source,
                }
            })?;
        if readback != target {
            return Err(WorkspaceError::ExactBaseDivergent {
                path: path.to_path_buf(),
            });
        }
        Ok(VerifiedCheckpointProjection::new(
            self.core.mission_id().clone(),
            target.to_vec(),
            disposition,
        ))
    }
}

/// Verifies one exact-base entry (`mission.md`, `plan.json`, or
/// `checkpoint.json`) has the shape a publisher requires before trusting or
/// replacing its content — a regular file, mode `0600`, and exactly one
/// link — and returns the file opened to make that verification.
///
/// The shape check and the eventual read are anchored to this single
/// returned descriptor: the caller must read bytes from this exact file
/// (via [`read_bounded_from_open_file`]), never through a second,
/// independent open of the same path. Opening once and fstat-ing that
/// descriptor closes a check-then-read TOCTOU a symlink or hard-link swap
/// could otherwise exploit between an initial shape check and a later,
/// separately-opened read: a same-shaped-looking file substituted between
/// two independent opens would have its bytes trusted without the shape
/// guarantee actually holding for the bytes read. A symlink, wrong type,
/// wrong mode, or hard-linked entry is rejected without ever being read or
/// written.
fn open_verified_exact_base_entry(
    directory: &Dir,
    path: &Path,
) -> Result<cap_std::fs::File, WorkspaceError> {
    let symlink_metadata =
        directory
            .symlink_metadata(path)
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect exact base file",
                source,
            })?;
    if symlink_metadata.file_type().is_symlink() {
        return Err(WorkspaceError::InvalidKnownEntry {
            path: path.to_path_buf(),
        });
    }
    let file = open_file_nofollow(directory, path).map_err(|source| WorkspaceError::Io {
        operation: "open exact base file",
        source,
    })?;
    let file_metadata = file.metadata().map_err(|source| WorkspaceError::Io {
        operation: "inspect opened exact base file",
        source,
    })?;
    if !file_metadata.is_file() || mode(&file_metadata) != 0o600 || link_count(&file_metadata) != 1
    {
        return Err(WorkspaceError::InvalidKnownEntry {
            path: path.to_path_buf(),
        });
    }
    Ok(file)
}

/// Reads the exact bytes of an already-opened, already-shape-verified exact
/// base file, bounded to `limit`. This must only be called with the file
/// handle [`open_verified_exact_base_entry`] returned for the same path — it
/// performs no further shape check of its own, by design, so the shape and
/// the bytes always come from the one retained descriptor.
fn read_bounded_from_open_file(
    mut file: cap_std::fs::File,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds the capability size limit",
        ));
    }
    Ok(bytes)
}

/// Reads workspace-local `events.jsonl` through the identical single-fd
/// open+fstat(mode `0600`, exactly one link, is a regular file)+read
/// discipline [`open_verified_exact_base_entry`] enforces for
/// `mission.md`/`plan.json`/`checkpoint.json`, except a wholly absent file
/// is tolerated as the trivial empty case: no composition in this crate
/// ever creates `events.jsonl` for a production workspace (only the
/// isolated-fixture lane does), so its ordinary state is "does not exist
/// yet". Any other shape violation — symlink, wrong mode, hard link — is
/// rejected exactly as it would be for the three exact-base files.
///
/// Used only by
/// [`WorkspaceAuthority::into_production_projection_writer_recovered`] to
/// prove the workspace holds no foreign (fixture-lane) history before
/// granting recovered write authority.
fn verified_recovery_event_log_bytes(directory: &Dir) -> Result<Vec<u8>, WorkspaceError> {
    let path = Path::new("events.jsonl");
    match open_verified_exact_base_entry(directory, path) {
        Ok(file) => read_bounded_from_open_file(file, MAX_EVENT_LOG_BYTES).map_err(|source| {
            WorkspaceError::Io {
                operation: "read event log for recovery verification",
                source,
            }
        }),
        Err(WorkspaceError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

/// Opaque proof that a workspace's exact base (`mission.md`, `plan.json`,
/// `checkpoint.json`) matched its caller-supplied expected bytes, verified
/// through the retained no-follow production authority.
///
/// Only [`ProductionProjectionWriter::verify_exact_base`] can construct one.
/// Its fields are private and it has no public constructor, so external code
/// cannot fabricate this value by assertion — it is the value from which a
/// base compatibility receipt may be minted:
///
/// Doctest honesty note: as of this writing, `VerifiedWorkspaceBase` is not
/// yet re-exported from `orchestrator_app`'s `lib.rs` (that export lands with
/// Cell 2D's root integration). The `use` below therefore fails to resolve
/// today — rustc reports **E0432** (unresolved import), not the **E0451**
/// (private field) annotation on this block. `rustdoc` only checks that a
/// `compile_fail` block fails to compile at all; it does not verify the
/// annotated error code, so this passes either way without lying about which
/// failure it caught. Once Cell 2D adds the `pub use`, this same block must
/// start failing with E0451 instead — if it does not (e.g. because the
/// private-field seal was accidentally loosened), this doctest must be
/// re-verified by hand; do not remove the export without restoring an
/// equivalent seal proof.
///
/// ```compile_fail,E0451
/// use orchestrator_app::VerifiedWorkspaceBase;
/// use orchestrator_core::MissionId;
///
/// fn cannot_forge(mission_id: MissionId) -> VerifiedWorkspaceBase {
///     VerifiedWorkspaceBase { mission_id }
/// }
/// ```
pub struct VerifiedWorkspaceBase {
    mission_id: MissionId,
}

impl fmt::Debug for VerifiedWorkspaceBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedWorkspaceBase")
            .field("kind", &"opaque-verified-workspace-base")
            .finish()
    }
}

impl VerifiedWorkspaceBase {
    fn new(mission_id: MissionId) -> Self {
        Self { mission_id }
    }

    /// Returns the mission this verified base belongs to.
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }
}

/// How [`ProductionProjectionWriter::reconcile_checkpoint`] reached its exact
/// durable target.
///
/// This is an observation, not write authority. In particular,
/// [`Self::AlreadyTarget`] proves recovery recognized a crossed crash boundary
/// without replacing `checkpoint.json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointReconciliationDisposition {
    /// `checkpoint.json` matched the expected prior bytes and was atomically
    /// replaced with the target bytes.
    Published,
    /// `checkpoint.json` already contained the exact target bytes, so no write
    /// was performed.
    AlreadyTarget,
}

/// Opaque proof that `checkpoint.json` now holds one exact target's bytes,
/// verified through the retained no-follow production authority after either
/// publishing them or observing a crossed-crash-boundary replay.
///
/// Only [`ProductionProjectionWriter::reconcile_checkpoint`] can construct
/// one, for the same sealed-construction reason as
/// [`VerifiedWorkspaceBase`].
///
/// Doctest honesty note: as with [`VerifiedWorkspaceBase`]'s doctest above,
/// `VerifiedCheckpointProjection` is not yet re-exported from
/// `orchestrator_app`'s `lib.rs` (Cell 2D's root integration adds it). The
/// `use` below fails to resolve today with **E0432** (unresolved import),
/// not the annotated **E0451** (private field) — `rustdoc` only checks that
/// the block fails to compile, not which code it failed with, so this
/// remains an honest failing test either way. Once Cell 2D adds the `pub
/// use`, this block must start failing with E0451 instead; if it does not,
/// re-verify by hand — do not remove the export without restoring an
/// equivalent seal proof.
///
/// ```compile_fail,E0451
/// use orchestrator_app::VerifiedCheckpointProjection;
/// use orchestrator_core::MissionId;
///
/// fn cannot_forge(mission_id: MissionId) -> VerifiedCheckpointProjection {
///     VerifiedCheckpointProjection {
///         mission_id,
///         checkpoint_bytes: Vec::new(),
///     }
/// }
/// ```
pub struct VerifiedCheckpointProjection {
    mission_id: MissionId,
    checkpoint_bytes: Vec<u8>,
    disposition: CheckpointReconciliationDisposition,
}

impl fmt::Debug for VerifiedCheckpointProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedCheckpointProjection")
            .field("kind", &"opaque-verified-checkpoint-projection")
            .field("checkpoint_bytes", &self.checkpoint_bytes.len())
            .field("disposition", &self.disposition)
            .finish()
    }
}

impl VerifiedCheckpointProjection {
    fn new(
        mission_id: MissionId,
        checkpoint_bytes: Vec<u8>,
        disposition: CheckpointReconciliationDisposition,
    ) -> Self {
        Self {
            mission_id,
            checkpoint_bytes,
            disposition,
        }
    }

    /// Returns the mission this verified checkpoint belongs to.
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// Returns the exact bytes now durably published at `checkpoint.json`.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    /// Returns whether this call published the target or recognized it as
    /// already durable.
    #[must_use]
    pub const fn disposition(&self) -> CheckpointReconciliationDisposition {
        self.disposition
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn build_base_layout(directory: &Dir, seed: &WorkspaceSeed) -> Result<(), WorkspaceError> {
    let workers = create_and_open(directory, "workers")?;
    let artifacts = create_and_open(directory, "artifacts")?;
    let merged = create_and_open(&artifacts, "merged")?;
    let learnings = create_and_open(directory, "learnings")?;
    for child in [&workers, &artifacts, &merged, &learnings] {
        sync_dir(child).map_err(|source| WorkspaceError::Io {
            operation: "sync workspace child directory",
            source,
        })?;
    }
    for (name, bytes) in [
        ("mission.md", seed.mission.as_slice()),
        ("checkpoint.json", seed.checkpoint.as_slice()),
        ("plan.json", seed.plan.as_slice()),
    ] {
        create_private_file(directory, Path::new(name), bytes, false).map_err(|source| {
            WorkspaceError::Io {
                operation: "write workspace seed file",
                source,
            }
        })?;
    }
    Ok(())
}

fn cleanup_staging(workspaces: &Dir, staging_path: &Path) {
    let _ = remove_tree_owned(workspaces, staging_path);
    let _ = sync_dir(workspaces);
}

fn create_and_open(parent: &Dir, name: &str) -> Result<Dir, WorkspaceError> {
    create_dir_private(parent, Path::new(name)).map_err(|source| WorkspaceError::Io {
        operation: "create workspace directory",
        source,
    })?;
    open_dir_path_nofollow(parent, Path::new(name)).map_err(|source| WorkspaceError::Io {
        operation: "open workspace directory",
        source,
    })
}

fn ensure_private_child(parent: &Dir, name: &Path) -> Result<Dir, WorkspaceError> {
    match create_dir_private(parent, name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(WorkspaceError::Io {
                operation: "create private child directory",
                source,
            });
        }
    }
    let directory = open_dir_path_nofollow(parent, name).map_err(|source| WorkspaceError::Io {
        operation: "open private child directory",
        source,
    })?;
    if mode(
        &directory
            .dir_metadata()
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect private child directory",
                source,
            })?,
    ) != 0o700
    {
        return Err(WorkspaceError::InvalidKnownEntry {
            path: name.to_path_buf(),
        });
    }
    Ok(directory)
}

fn ensure_phase_worker_child(parent: &Dir, name: &Path) -> Result<Dir, WorkspaceError> {
    ensure_private_child(parent, name).map_err(|error| match error {
        WorkspaceError::InvalidKnownEntry { .. } => WorkspaceError::IdentityChanged,
        other => other,
    })
}

#[cfg(unix)]
fn verify_worker_entry_name(
    workers: &Dir,
    worker_id: &WorkerId,
    expected_identity: FileIdentity,
) -> Result<(), WorkspaceError> {
    let mut found = false;
    let entries = workers.entries().map_err(|source| WorkspaceError::Io {
        operation: "enumerate phase workers directory",
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| WorkspaceError::Io {
            operation: "read phase worker directory entry",
            source,
        })?;
        let name = entry.file_name();
        if name.as_os_str().as_bytes() != worker_id.as_str().as_bytes() {
            continue;
        }
        if found {
            return Err(WorkspaceError::IdentityChanged);
        }
        let metadata = workers
            .symlink_metadata(Path::new(&name))
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect phase worker directory entry",
                source,
            })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || mode(&metadata) != 0o700
            || identity(&metadata) != expected_identity
        {
            return Err(WorkspaceError::IdentityChanged);
        }
        found = true;
    }
    if !found {
        return Err(WorkspaceError::IdentityChanged);
    }
    Ok(())
}

fn phase_worker_marker_bytes(
    mission_id: &MissionId,
    phase_id: &PhaseId,
    worker_id: &WorkerId,
) -> Vec<u8> {
    let fields = [
        mission_id.as_str().as_bytes(),
        phase_id.as_str().as_bytes(),
        worker_id.as_str().as_bytes(),
    ];
    let capacity = PHASE_WORKER_MARKER_DOMAIN.len()
        + 1
        + fields
            .iter()
            .map(|field| std::mem::size_of::<u64>() + field.len())
            .sum::<usize>();
    let mut marker = Vec::with_capacity(capacity);
    marker.extend_from_slice(PHASE_WORKER_MARKER_DOMAIN);
    marker.push(PHASE_WORKER_MARKER_VERSION);
    for field in fields {
        marker.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        marker.extend_from_slice(field);
    }
    marker
}

fn seal_phase_worker_marker(claude: &Dir, expected: &[u8]) -> Result<FileIdentity, WorkspaceError> {
    let marker = Path::new(PHASE_WORKER_MARKER);
    match claude.symlink_metadata(marker) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            publish_phase_worker_marker(claude, marker, expected)?;
        }
        Err(source) => {
            return Err(WorkspaceError::Io {
                operation: "inspect phase worker authority marker",
                source,
            });
        }
    }
    let marker_identity = validate_phase_worker_marker(claude, expected)?;
    sync_dir(claude).map_err(|source| WorkspaceError::Io {
        operation: "sync phase worker authority marker directory",
        source,
    })?;
    Ok(marker_identity)
}

fn publish_phase_worker_marker(
    claude: &Dir,
    marker: &Path,
    expected: &[u8],
) -> Result<(), WorkspaceError> {
    let temporary = create_phase_worker_marker_temporary(claude, expected)?;
    let publish = rustix::fs::renameat_with(
        claude,
        &temporary,
        claude,
        marker,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from);
    match publish {
        Ok(()) => sync_dir(claude).map_err(|source| WorkspaceError::Io {
            operation: "publish phase worker authority marker",
            source,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => claude
            .remove_file(&temporary)
            .and_then(|()| sync_dir(claude))
            .map_err(|source| WorkspaceError::Io {
                operation: "remove redundant phase worker authority marker",
                source,
            }),
        Err(source) => {
            let _ = claude.remove_file(&temporary);
            let _ = sync_dir(claude);
            Err(WorkspaceError::Io {
                operation: "publish phase worker authority marker",
                source,
            })
        }
    }
}

fn create_phase_worker_marker_temporary(
    claude: &Dir,
    expected: &[u8],
) -> Result<PathBuf, WorkspaceError> {
    for _ in 0..PHASE_WORKER_MARKER_CREATE_ATTEMPTS {
        let nonce = next_phase_worker_marker_nonce();
        let temporary = PathBuf::from(format!(
            ".{PHASE_WORKER_MARKER}.creating-{}-{nonce}",
            std::process::id()
        ));
        match create_private_file(claude, &temporary, expected, false) {
            Ok(()) => return Ok(temporary),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Never inspect or delete a colliding entry: a crashed write
                // may be partial and an attacker-controlled entry must remain
                // untouched. The bounded monotonic nonce search safely moves
                // to a fresh exact name.
                continue;
            }
            Err(source) => {
                return Err(WorkspaceError::Io {
                    operation: "create phase worker authority marker",
                    source,
                });
            }
        }
    }
    Err(WorkspaceError::IdentityChanged)
}

fn next_phase_worker_marker_nonce() -> u64 {
    #[cfg(test)]
    if let Some(nonce) = PHASE_WORKER_MARKER_TEST_NONCE.with(|slot| {
        slot.get().inspect(|nonce| {
            slot.set(Some(nonce.wrapping_add(1)));
        })
    }) {
        return nonce;
    }
    STAGING_NONCE.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
fn with_phase_worker_marker_test_nonce<T>(first: u64, operation: impl FnOnce() -> T) -> T {
    PHASE_WORKER_MARKER_TEST_NONCE.with(|slot| {
        let previous = slot.replace(Some(first));
        let output = operation();
        slot.set(previous);
        output
    })
}

fn validate_phase_worker_marker(
    claude: &Dir,
    expected: &[u8],
) -> Result<FileIdentity, WorkspaceError> {
    let mut exact_name_found = false;
    let entries = claude
        .entries()
        .map_err(|_| WorkspaceError::IdentityChanged)?;
    for entry in entries {
        let entry = entry.map_err(|_| WorkspaceError::IdentityChanged)?;
        if entry.file_name().as_os_str().as_bytes() == PHASE_WORKER_MARKER.as_bytes() {
            if exact_name_found {
                return Err(WorkspaceError::IdentityChanged);
            }
            exact_name_found = true;
        }
    }
    if !exact_name_found {
        return Err(WorkspaceError::IdentityChanged);
    }
    let mut file = open_file_nofollow(claude, Path::new(PHASE_WORKER_MARKER))
        .map_err(|_| WorkspaceError::IdentityChanged)?;
    let metadata = file
        .metadata()
        .map_err(|_| WorkspaceError::IdentityChanged)?;
    #[cfg(unix)]
    let exact_mode = {
        use cap_std::fs::MetadataExt;
        metadata.mode() & 0o7777
    };
    #[cfg(not(unix))]
    let exact_mode = mode(&metadata);
    if !metadata.is_file() || exact_mode != 0o600 || link_count(&metadata) != 1 {
        return Err(WorkspaceError::IdentityChanged);
    }
    let mut bytes = Vec::with_capacity(expected.len().saturating_add(1));
    Read::by_ref(&mut file)
        .take(u64::try_from(expected.len()).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WorkspaceError::IdentityChanged)?;
    if bytes != expected {
        return Err(WorkspaceError::IdentityChanged);
    }
    file.sync_all()
        .map_err(|_| WorkspaceError::IdentityChanged)?;
    Ok(identity(&metadata))
}

fn validate_base_layout(directory: &Dir) -> Result<(), WorkspaceError> {
    if mode(
        &directory
            .dir_metadata()
            .map_err(|source| WorkspaceError::Io {
                operation: "inspect workspace mode",
                source,
            })?,
    ) != 0o700
    {
        return Err(WorkspaceError::InvalidKnownEntry {
            path: PathBuf::from("."),
        });
    }
    for path in ["workers", "artifacts", "artifacts/merged", "learnings"] {
        let child = open_dir_path_nofollow(directory, Path::new(path)).map_err(|source| {
            WorkspaceError::Io {
                operation: "validate workspace directory",
                source,
            }
        })?;
        if mode(&child.dir_metadata().map_err(|source| WorkspaceError::Io {
            operation: "inspect workspace directory mode",
            source,
        })?) != 0o700
        {
            return Err(WorkspaceError::InvalidKnownEntry {
                path: PathBuf::from(path),
            });
        }
    }
    for path in ["mission.md", "checkpoint.json", "plan.json"] {
        validate_private_file(directory, Path::new(path))?;
    }
    Ok(())
}

// Intentionally checks only `is_file` + mode `0600`: this is base-layout
// validation for `open_workspace_under`/`validate_base_layout`, run on every
// open regardless of whether the caller ever publishes through this
// workspace. `open_verified_exact_base_entry` additionally enforces
// `nlink == 1` because it backs projector-grade verification, where a
// pre-existing hard link is itself an attack (a second name for the same
// bytes a concurrent writer could mutate through). Whether base-layout
// validation should also require `nlink == 1` is a deliberate open question,
// not an oversight — alignment is left for a follow-up decision.
fn validate_private_file(directory: &Dir, path: &Path) -> Result<(), WorkspaceError> {
    let file = crate::fs_util::open_file_nofollow(directory, path).map_err(|source| {
        WorkspaceError::Io {
            operation: "open private workspace file",
            source,
        }
    })?;
    let metadata = file.metadata().map_err(|source| WorkspaceError::Io {
        operation: "inspect private workspace file",
        source,
    })?;
    if !metadata.is_file() || mode(&metadata) != 0o600 {
        return Err(WorkspaceError::InvalidKnownEntry {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_replace_target(directory: &Dir, path: &Path) -> Result<(), WorkspaceError> {
    validate_replace_target_mode(directory, path, 0o600)
}

fn validate_replace_target_mode(
    directory: &Dir,
    path: &Path,
    required_mode: u32,
) -> Result<(), WorkspaceError> {
    match directory.symlink_metadata(path) {
        Ok(_) => {
            let file = crate::fs_util::open_file_nofollow(directory, path).map_err(|source| {
                WorkspaceError::Io {
                    operation: "open workspace replacement target",
                    source,
                }
            })?;
            let metadata = file.metadata().map_err(|source| WorkspaceError::Io {
                operation: "inspect workspace replacement target",
                source,
            })?;
            if !metadata.is_file() || mode(&metadata) != required_mode {
                return Err(WorkspaceError::InvalidKnownEntry {
                    path: path.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WorkspaceError::Io {
            operation: "inspect replacement target",
            source,
        }),
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn validate_event_checkpoint_pair(
    event: &orchestrator_core::EventRecord,
    checkpoint: &CheckpointProjection,
) -> Result<(), WorkspaceError> {
    if matches!(
        event.event_type.as_str(),
        "worker.spawned" | "worker.output" | "worker.completed" | "worker.failed"
    ) {
        let phase_id = event
            .phase_id
            .as_deref()
            .ok_or(WorkspaceError::InvalidFixtureEvent)?;
        let has_worker = event
            .worker_id
            .as_deref()
            .is_some_and(|worker| !worker.is_empty());
        let phase_is_running = checkpoint
            .plan
            .as_ref()
            .and_then(|plan| plan.phases.iter().find(|phase| phase.id == phase_id))
            .is_some_and(|phase| phase.status == phase_status_name(PhaseStatus::Running));
        let checkpoint_sequence = checkpoint
            .extra
            .get(FIXTURE_EVENT_SEQUENCE_KEY)
            .and_then(serde_json::Value::as_i64);
        return if checkpoint.status == mission_status_name(MissionStatus::InProgress)
            && has_worker
            && phase_is_running
            && checkpoint_sequence == Some(event.sequence)
        {
            Ok(())
        } else {
            Err(WorkspaceError::InvalidFixtureEvent)
        };
    }
    let expected_mission = match event.event_type.as_str() {
        "mission.started" => Some(mission_status_name(MissionStatus::InProgress)),
        "mission.completed" => Some(mission_status_name(MissionStatus::Completed)),
        "mission.failed" => Some(mission_status_name(MissionStatus::Failed)),
        "mission.cancelled" => Some(mission_status_name(MissionStatus::Cancelled)),
        _ => None,
    };
    if let Some(expected) = expected_mission {
        return if checkpoint.status == expected && event.phase_id.is_none() {
            Ok(())
        } else {
            Err(WorkspaceError::InvalidFixtureEvent)
        };
    }
    let expected_phase = match event.event_type.as_str() {
        "phase.started" | "phase.retrying" => phase_status_name(PhaseStatus::Running),
        "phase.completed" => phase_status_name(PhaseStatus::Completed),
        "phase.failed" => phase_status_name(PhaseStatus::Failed),
        "phase.skipped" => phase_status_name(PhaseStatus::Skipped),
        _ => return Err(WorkspaceError::InvalidFixtureEvent),
    };
    let phase_id = event
        .phase_id
        .as_deref()
        .ok_or(WorkspaceError::InvalidFixtureEvent)?;
    let phase = checkpoint
        .plan
        .as_ref()
        .and_then(|plan| plan.phases.iter().find(|phase| phase.id == phase_id))
        .ok_or(WorkspaceError::InvalidFixtureEvent)?;
    if phase.status == expected_phase {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidFixtureEvent)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        FixtureArtifactErrorKind, OverlayError, RoleDenyPolicy, SettingsOverlay, WorkerRole,
        checkpoint_projection::{checkpoint_for_state, journal_checkpoint_expectation},
        fixture_authority::{FixtureAdmissionPolicy, FreshFixtureAuthority},
        runtime_home::{IsolatedFixtureRoot, ProductionBoundary},
    };
    use orchestrator_core::{
        CheckpointPhase, EventId, MissionId, MissionState, PhaseDefinition, PhaseId, ReducerInput,
        ReducerTransition, WorkerId, reduce,
    };

    static PROD_CASE: AtomicU64 = AtomicU64::new(1);

    fn private_dir(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn restore_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, bytes)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    fn checkpoint(id: &str, status: &str) -> CheckpointProjection {
        CheckpointProjection {
            workspace_id: id.to_owned(),
            status: status.to_owned(),
            started_at: "2026-07-14T00:00:00Z".to_owned(),
            ..CheckpointProjection::default()
        }
    }

    fn admit_fixture(
        label: &str,
    ) -> Result<(PathBuf, FreshFixtureAuthority), Box<dyn std::error::Error>> {
        let number = PROD_CASE.fetch_add(1, Ordering::Relaxed);
        let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = canonical_temp.join(format!(
            "orchestrator-rs-workspace-prod-{}-{number}-{label}",
            std::process::id()
        ));
        private_dir(&parent)?;
        let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy =
            FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &canonical_temp);
        let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
        Ok((parent, authority))
    }

    type PhaseWorkerWorkspace = (
        PathBuf,
        FreshFixtureAuthority,
        MissionId,
        PhaseId,
        WorkspaceAuthority,
        PathBuf,
    );

    fn phase_worker_workspace(
        label: &str,
    ) -> Result<PhaseWorkerWorkspace, Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture(label)?;
        let mission_id = MissionId::new(format!("{label}-mission"))?;
        let phase_id = PhaseId::new("phase-1")?;
        let workspace = authority.create_workspace(
            mission_id.clone(),
            WorkspaceSeed::new(
                b"phase worker fixture\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
        )?;
        let worker_id = WorkerId::for_phase("fixture", &phase_id)?;
        let worker_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        Ok((
            parent,
            authority,
            mission_id,
            phase_id,
            workspace,
            worker_path,
        ))
    }

    fn materialize_phase_worker(
        workspace: &WorkspaceAuthority,
        persona: &str,
        phase: &PhaseId,
        requested: &Path,
    ) -> Result<PhaseWorkerAuthority, WorkspaceError> {
        let binding = workspace.phase_worker_binding(persona, phase, requested)?;
        binding.materialize()
    }

    #[test]
    fn phase_worker_authority_uses_exact_go_path_and_survives_workspace_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, fixture, mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("phase-worker-reopen")?;
        let first = materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;
        let (first_cwd, first_identity, first_relative) = first.process_cwd_binding()?;
        assert_eq!(first.worker_id().as_str(), "fixture-phase-1");
        assert_eq!(
            first_relative,
            Path::new("workspaces")
                .join(mission_id.as_str())
                .join("workers")
                .join("fixture-phase-1")
        );
        assert_eq!(identity(&first_cwd.dir_metadata()?), first_identity);
        assert!(
            open_dir_path_nofollow(&first_cwd, Path::new(".claude/hooks")).is_ok(),
            "process CWD binding was not the retained worker directory"
        );
        drop(first);

        let marker_mission = mission_id.clone();
        let reopened = fixture.open_workspace(mission_id)?;
        let second = materialize_phase_worker(&reopened, "fixture", &phase_id, &worker_path)?;
        let (_, second_identity, second_relative) = second.process_cwd_binding()?;
        assert_eq!(second_identity, first_identity);
        assert_eq!(second_relative, first_relative);
        assert_eq!(
            std::fs::read(worker_path.join(".claude").join(PHASE_WORKER_MARKER))?,
            phase_worker_marker_bytes(&marker_mission, &phase_id, second.worker_id(),)
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[cfg(feature = "verification-process-canary")]
    #[test]
    fn canary_sentinel_is_published_private_and_byte_exact()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("canary-sentinel-valid")?;
        let authority = materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;

        authority.publish_canary_sentinel("sentinel.txt", b"canary-proof\n")?;

        let sentinel = worker_path.join("sentinel.txt");
        assert_eq!(std::fs::read(&sentinel)?, b"canary-proof\n");
        assert_eq!(std::fs::symlink_metadata(&sentinel)?.mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[cfg(feature = "verification-process-canary")]
    #[test]
    fn canary_sentinel_existing_file_or_symlink_fails_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        for collision in ["file", "symlink"] {
            let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
                phase_worker_workspace(&format!("canary-sentinel-{collision}"))?;
            let authority =
                materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;
            let sentinel = worker_path.join("sentinel.txt");
            let outside = parent.join("outside-sentinel");
            restore_private_file(&outside, b"preserve")?;
            if collision == "file" {
                restore_private_file(&sentinel, b"existing")?;
            } else {
                symlink(&outside, &sentinel)?;
            }

            assert!(
                authority
                    .publish_canary_sentinel("sentinel.txt", b"replacement\n")
                    .is_err(),
                "{collision}"
            );
            assert_eq!(std::fs::read(&outside)?, b"preserve");
            if collision == "file" {
                assert_eq!(std::fs::read(&sentinel)?, b"existing");
            } else {
                assert!(
                    std::fs::symlink_metadata(&sentinel)?
                        .file_type()
                        .is_symlink()
                );
            }
            let _ = std::fs::remove_dir_all(&parent);
        }
        Ok(())
    }

    #[cfg(feature = "verification-process-canary")]
    #[test]
    fn canary_sentinel_replaced_worker_directory_fails_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("canary-sentinel-replaced-worker")?;
        let authority = materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;
        let displaced = parent.join("displaced-worker");
        std::fs::rename(&worker_path, &displaced)?;
        private_dir(&worker_path)?;

        assert!(
            authority
                .publish_canary_sentinel("sentinel.txt", b"must-not-publish\n")
                .is_err()
        );
        assert!(!worker_path.join("sentinel.txt").exists());
        assert!(!displaced.join("sentinel.txt").exists());
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn phase_worker_path_aliases_are_rejected_before_lazy_directory_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStringExt;

        let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("phase-worker-alias")?;
        let mut alias_bytes = worker_path
            .parent()
            .ok_or("worker path has no parent")?
            .as_os_str()
            .as_bytes()
            .to_vec();
        alias_bytes.extend_from_slice(b"//fixture-phase-1");
        let alias = PathBuf::from(std::ffi::OsString::from_vec(alias_bytes));
        assert_eq!(alias, worker_path);
        assert!(matches!(
            workspace.phase_worker_binding("fixture", &phase_id, &alias),
            Err(WorkspaceError::ProcessWorkingRootMismatch)
        ));
        assert!(matches!(
            workspace
                .phase_worker_binding("fixture", &phase_id, &parent.join("wrong-worker-root"),),
            Err(WorkspaceError::ProcessWorkingRootMismatch)
        ));
        assert!(!worker_path.exists());
        let workspace_path = worker_path
            .parent()
            .and_then(Path::parent)
            .ok_or("worker path has no workspace ancestor")?;
        assert!(
            !workspace_path
                .join("artifacts")
                .join(phase_id.as_str())
                .exists()
        );
        assert!(!workspace_path.join("scratch").exists());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn derived_phase_worker_binding_rejects_workspace_replacement_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("phase-worker-workspace-replaced")?;
        let binding = workspace.phase_worker_binding("fixture", &phase_id, &worker_path)?;
        let workspace_path = worker_path
            .parent()
            .and_then(Path::parent)
            .ok_or("worker path has no workspace ancestor")?
            .to_path_buf();
        let displaced = parent.join("displaced-workspace");
        std::fs::rename(&workspace_path, &displaced)?;
        private_dir(&workspace_path)?;
        private_dir(&workspace_path.join("workers"))?;
        private_dir(&workspace_path.join("artifacts"))?;

        assert!(matches!(
            binding.materialize(),
            Err(WorkspaceError::IdentityChanged)
        ));
        assert!(!worker_path.exists());
        assert!(
            !workspace_path
                .join("artifacts")
                .join(phase_id.as_str())
                .exists()
        );
        assert!(!workspace_path.join("scratch").exists());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn marker_publication_preserves_hostile_collision_and_uses_fresh_nonce()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, fixture) = admit_fixture("phase-worker-marker-collision")?;
        let worker = ensure_private_child(fixture.boundary.directory(), Path::new("worker"))?;
        let claude = ensure_private_child(&worker, Path::new(".claude"))?;
        let mission_id = MissionId::new("marker-collision-mission")?;
        let phase_id = PhaseId::new("phase-1")?;
        let worker_id = WorkerId::new("fixture-phase-1")?;
        let expected = phase_worker_marker_bytes(&mission_id, &phase_id, &worker_id);
        let hostile_nonce = 424_242_u64;
        let hostile_name = format!(
            ".{PHASE_WORKER_MARKER}.creating-{}-{hostile_nonce}",
            std::process::id()
        );
        create_private_file(
            &claude,
            Path::new(&hostile_name),
            b"hostile stale bytes",
            false,
        )?;

        with_phase_worker_marker_test_nonce(hostile_nonce, || {
            publish_phase_worker_marker(&claude, Path::new(PHASE_WORKER_MARKER), &expected)
        })?;

        let claude_path = fixture
            .boundary
            .canonical_path()
            .join("worker")
            .join(".claude");
        assert_eq!(
            std::fs::read(claude_path.join(&hostile_name))?,
            b"hostile stale bytes"
        );
        assert_eq!(
            std::fs::read(claude_path.join(PHASE_WORKER_MARKER))?,
            expected
        );
        let fresh_name = format!(
            ".{PHASE_WORKER_MARKER}.creating-{}-{}",
            std::process::id(),
            hostile_nonce + 1
        );
        assert!(
            !claude_path.join(fresh_name).exists(),
            "fresh temporary marker was not renamed into place"
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn phase_worker_authority_rejects_mismatched_existing_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("phase-worker-marker-mismatch")?;
        private_dir(&worker_path)?;
        private_dir(&worker_path.join(".claude"))?;
        private_dir(&worker_path.join(".claude/hooks"))?;
        restore_private_file(
            &worker_path.join(".claude").join(PHASE_WORKER_MARKER),
            b"foreign-worker-authority",
        )?;

        assert!(matches!(
            materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path),
            Err(WorkspaceError::IdentityChanged)
        ));
        assert_eq!(
            std::fs::read(worker_path.join(".claude").join(PHASE_WORKER_MARKER))?,
            b"foreign-worker-authority"
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn phase_worker_authority_rejects_replaced_claude_or_marker_before_cwd()
    -> Result<(), Box<dyn std::error::Error>> {
        for replacement in ["claude-directory", "marker-inode"] {
            let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
                phase_worker_workspace(&format!("phase-worker-{replacement}"))?;
            let authority =
                materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;
            let claude_path = worker_path.join(".claude");
            if replacement == "claude-directory" {
                std::fs::rename(&claude_path, parent.join("displaced-claude-directory"))?;
                private_dir(&claude_path)?;
            } else {
                let marker = claude_path.join(PHASE_WORKER_MARKER);
                let marker_bytes = std::fs::read(&marker)?;
                std::fs::rename(&marker, parent.join("displaced-marker"))?;
                restore_private_file(&marker, &marker_bytes)?;
            }

            assert!(matches!(
                authority.process_cwd_binding(),
                Err(WorkspaceError::IdentityChanged)
            ));
            let _ = std::fs::remove_dir_all(&parent);
        }
        Ok(())
    }

    #[test]
    fn phase_worker_authority_rejects_replaced_or_symlinked_worker_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        for replacement in ["directory", "symlink"] {
            let (parent, _fixture, _mission_id, phase_id, workspace, worker_path) =
                phase_worker_workspace(&format!("phase-worker-replaced-{replacement}"))?;
            let authority =
                materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)?;
            let displaced = parent.join(format!("displaced-{replacement}"));
            std::fs::rename(&worker_path, &displaced)?;
            if replacement == "directory" {
                private_dir(&worker_path)?;
            } else {
                symlink(&displaced, &worker_path)?;
            }

            assert!(authority.process_cwd_binding().is_err(), "{replacement}");
            let _ = std::fs::remove_dir_all(&parent);
        }
        Ok(())
    }

    #[test]
    fn worker_entry_name_verification_rejects_case_and_normalization_aliases()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, fixture) = admit_fixture("phase-worker-entry-bytes")?;
        let workers = ensure_private_child(fixture.boundary.directory(), Path::new("workers"))?;
        for (actual, requested) in [
            ("Fixture-phase-1", "fixture-phase-1"),
            ("caf\u{e9}-phase-1", "cafe\u{301}-phase-1"),
        ] {
            let actual_dir = ensure_private_child(&workers, Path::new(actual))?;
            let actual_identity = identity(&actual_dir.dir_metadata()?);
            assert!(matches!(
                verify_worker_entry_name(&workers, &WorkerId::new(requested)?, actual_identity,),
                Err(WorkspaceError::IdentityChanged)
            ));
        }

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn concurrent_phase_worker_authorities_converge_on_one_complete_marker()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, fixture, mission_id, phase_id, workspace, worker_path) =
            phase_worker_workspace("phase-worker-concurrent")?;
        let second_workspace = fixture.open_workspace(mission_id)?;
        let second_phase = phase_id.clone();
        let second_path = worker_path.clone();
        let expected_worker_path = worker_path.clone();
        let first_thread = std::thread::spawn(move || {
            materialize_phase_worker(&workspace, "fixture", &phase_id, &worker_path)
        });
        let second_thread = std::thread::spawn(move || {
            materialize_phase_worker(&second_workspace, "fixture", &second_phase, &second_path)
        });
        let first = first_thread
            .join()
            .map_err(|_| std::io::Error::other("first phase worker thread panicked"))??;
        let second = second_thread
            .join()
            .map_err(|_| std::io::Error::other("second phase worker thread panicked"))??;
        assert_eq!(first.worker_id(), second.worker_id());
        assert_eq!(
            first.process_cwd_binding()?.1,
            second.process_cwd_binding()?.1
        );
        let claude_path = expected_worker_path.join(".claude");
        assert!(
            std::fs::read_dir(&claude_path)?
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".creating-"))
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn admit_production_rejects_a_missing_workspace() -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("missing")?;
        let mission_id = MissionId::new("missing-mission")?;
        // Production admission requires a sealed ProductionBoundary; a forged or
        // fixture root cannot be substituted (the type system rejects it).
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let result = WorkspaceAuthority::admit_production(production_root, mission_id);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn admit_production_admits_an_existing_layout_without_extras()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("shared")?;
        let mission_id = MissionId::new("shared-mission")?;
        let seed = FixtureWorkspaceSeed::new(
            b"fixture mission\n".to_vec(),
            &checkpoint("shared-mission", "pending"),
            br#"{"future_plan":{"preserve":true}}"#.to_vec(),
        )?;
        // Build the validated layout under the shared capability root.
        let fixture_workspace = authority.create_workspace(mission_id.clone(), seed)?;
        assert!(matches!(
            &fixture_workspace.kind,
            WorkspaceKind::Fixture { .. }
        ));

        // Re-admit the same workspace through the production constructor. It
        // must re-validate the layout and carry no fixture leasing extras. A
        // real ProductionBoundary bound to the same canonical root is required;
        // a fixture boundary cannot be passed (typed admission).
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let production_workspace =
            WorkspaceAuthority::admit_production(production_root, mission_id.clone())?;
        assert!(matches!(
            &production_workspace.kind,
            WorkspaceKind::Production(_)
        ));
        assert_eq!(production_workspace.mission_id(), &mission_id);
        assert!(matches!(
            production_workspace.verified_fixture_boundary(),
            Err(WorkspaceError::FixtureCapabilityUnavailable)
        ));
        let phase = PhaseId::new("phase-1")?;
        let Err(artifact_error) =
            production_workspace.bind_fixture_artifact(&phase, 1, "result.json", b"{}")
        else {
            return Err(std::io::Error::other(
                "a production workspace admitted fixture artifact authority",
            )
            .into());
        };
        assert_eq!(artifact_error.kind(), FixtureArtifactErrorKind::FixtureOnly);
        assert!(
            !authority
                .boundary
                .canonical_path()
                .join("workspaces")
                .join(mission_id.as_str())
                .join("artifacts/phase-1")
                .exists(),
            "rejected fixture artifact binding created an artifact namespace"
        );
        let worker = WorkerId::new("worker-a")?;
        let target = authority.create_target("target-a")?;
        assert!(matches!(
            SettingsOverlay::install(
                target,
                &production_workspace,
                &worker,
                RoleDenyPolicy::for_role(WorkerRole::Implementer),
            ),
            Err(OverlayError::Workspace(
                WorkspaceError::FixtureCapabilityUnavailable
            ))
        ));
        assert!(matches!(
            authority.recover_settings_overlay(&production_workspace, &worker),
            Err(OverlayError::Workspace(
                WorkspaceError::FixtureCapabilityUnavailable
            ))
        ));
        assert!(
            !authority
                .boundary
                .canonical_path()
                .join("workspaces")
                .join(mission_id.as_str())
                .join("workers")
                .join(worker.as_str())
                .exists(),
            "rejected fixture-only operations created production worker state"
        );
        assert!(
            !authority
                .boundary
                .canonical_path()
                .join("targets/target-a/.claude")
                .exists(),
            "rejected settings overlay created a target settings namespace"
        );
        // Production and fixture boundaries are distinct sealed roots bound to
        // the same canonical directory; they share the root path, not the Arc.
        assert_eq!(
            production_workspace.kind.boundary().canonical_path(),
            fixture_workspace.kind.boundary().canonical_path(),
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn production_boundary_creates_workspace_and_exclusively_owns_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("production-create")?;
        let mission_id = MissionId::new("production-create-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let seed = WorkspaceSeed::new(
            b"production canary mission\n".to_vec(),
            &checkpoint(mission_id.as_str(), "pending"),
            b"{}".to_vec(),
        )?;

        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            seed,
        )?;
        assert!(matches!(&workspace.kind, WorkspaceKind::Production(_)));
        assert!(!workspace.inspect()?.is_degraded());
        assert!(matches!(
            WorkspaceAuthority::create_production(
                Arc::clone(&production_root),
                mission_id.clone(),
                WorkspaceSeed::new(
                    b"collision\n".to_vec(),
                    &checkpoint(mission_id.as_str(), "pending"),
                    b"{}".to_vec(),
                )?,
            ),
            Err(WorkspaceError::Collision(_))
        ));

        let competing =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let writer = workspace.into_production_projection_writer()?;
        assert!(matches!(
            competing.into_production_projection_writer(),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));
        assert!(matches!(
            WorkspaceAuthority::admit_production(
                Arc::clone(&production_root),
                mission_id.clone(),
            )?
            .into_fixture_projection_writer(),
            Err(WorkspaceError::FixtureCapabilityUnavailable)
        ));
        drop(writer);
        assert!(
            WorkspaceAuthority::admit_production(production_root, mission_id)?
                .into_production_projection_writer()
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn post_publication_admission_failure_is_recovery_required()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("published-recovery")?;
        let mission_id = MissionId::new("published-recovery-mission")?;
        let result = create_workspace_under_with_hook(
            WorkspaceKind::Fixture {
                boundary: Arc::clone(&authority.boundary),
                extras: Arc::clone(&authority.extras),
            },
            mission_id.clone(),
            WorkspaceSeed::new(
                b"fixture mission\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
            |workspaces, mission| {
                let workspace = open_dir_path_nofollow(workspaces, Path::new(mission.as_str()))
                    .map_err(|source| WorkspaceError::Io {
                        operation: "open injected published workspace",
                        source,
                    })?;
                workspace
                    .remove_file("plan.json")
                    .map_err(|source| WorkspaceError::Io {
                        operation: "inject post-publication admission failure",
                        source,
                    })
            },
        );
        assert!(matches!(
            result,
            Err(WorkspaceError::PublishedWorkspaceRequiresRecovery { .. })
        ));
        assert!(
            authority
                .boundary
                .canonical_path()
                .join("workspaces")
                .join(mission_id.as_str())
                .is_dir(),
            "durably published workspace was incorrectly reported as absent"
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn fixture_projection_writer_is_exclusive_and_unavailable_to_production()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("projection-lease")?;
        let mission_id = MissionId::new("projection-lease-mission")?;
        let seed = FixtureWorkspaceSeed::new(
            b"fixture mission\n".to_vec(),
            &checkpoint(mission_id.as_str(), "pending"),
            b"{}".to_vec(),
        )?;
        let workspace = authority.create_workspace(mission_id.clone(), seed)?;
        let second = authority.open_workspace(mission_id.clone())?;
        assert!(matches!(
            authority
                .open_workspace(mission_id.clone())?
                .into_production_projection_writer(),
            Err(WorkspaceError::ProductionCapabilityUnavailable)
        ));
        let writer = workspace.into_fixture_projection_writer()?;
        assert!(matches!(
            second.into_fixture_projection_writer(),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));

        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let production = WorkspaceAuthority::admit_production(production_root, mission_id.clone())?;
        assert!(matches!(
            production.into_fixture_projection_writer(),
            Err(WorkspaceError::FixtureCapabilityUnavailable)
        ));
        drop(writer);
        assert!(
            authority
                .open_workspace(mission_id)?
                .into_fixture_projection_writer()
                .is_ok()
        );
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn fixture_backed_production_projection_shares_the_fixture_writer_registry()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("fixture-production-shared-lease")?;
        let mission_id = MissionId::new("fixture-production-shared-lease-mission")?;
        let workspace = authority.create_workspace(
            mission_id.clone(),
            WorkspaceSeed::new(
                b"shared projection lease mission\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
        )?;
        let production_root = Arc::new(ProductionBoundary::from_fixture_authority(&authority)?);
        let production =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;

        let fixture_writer = workspace.into_fixture_projection_writer()?;
        assert!(matches!(
            production.into_production_projection_writer(),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));
        drop(fixture_writer);

        let production =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let production_writer = production.into_production_projection_writer()?;
        assert!(matches!(
            authority
                .open_workspace(mission_id)?
                .into_fixture_projection_writer(),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));
        drop(production_writer);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn fixture_projection_acquisition_failure_releases_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("fixture-acquire-failure")?;
        let mission_id = MissionId::new("fixture-acquire-failure-mission")?;
        let workspace = authority.create_workspace(
            mission_id.clone(),
            WorkspaceSeed::new(
                b"fixture mission\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
        )?;
        let retry = authority.open_workspace(mission_id.clone())?;
        let checkpoint_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");
        let checkpoint_bytes = std::fs::read(&checkpoint_path)?;
        std::fs::remove_file(&checkpoint_path)?;

        assert!(workspace.into_fixture_projection_writer().is_err());
        restore_private_file(&checkpoint_path, &checkpoint_bytes)?;
        assert!(retry.into_fixture_projection_writer().is_ok());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn production_projection_acquisition_failure_releases_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("production-acquire-failure")?;
        let mission_id = MissionId::new("production-acquire-failure-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            WorkspaceSeed::new(
                b"production canary mission\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
        )?;
        let retry =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let checkpoint_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");
        let checkpoint_bytes = std::fs::read(&checkpoint_path)?;
        std::fs::remove_file(&checkpoint_path)?;

        assert!(workspace.into_production_projection_writer().is_err());
        restore_private_file(&checkpoint_path, &checkpoint_bytes)?;
        assert!(retry.into_production_projection_writer().is_ok());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    // Historical name: this test predates the recovery coordinator it asks
    // for. That coordinator now exists —
    // `into_production_projection_writer_recovered` (Cell 2E) — and its own
    // tests live below (`recovery_writer_*`). This test still proves the
    // narrower, still-true fact that the PRISTINE constructor alone refuses
    // a workspace with history.
    fn production_projection_with_history_requires_recovery_coordinator()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("production-recovery-gate")?;
        let mission_id = MissionId::new("production-recovery-gate-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            WorkspaceSeed::new(
                b"production canary mission\n".to_vec(),
                &checkpoint(mission_id.as_str(), "pending"),
                b"{}".to_vec(),
            )?,
        )?;
        let retry =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let event_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("events.jsonl");
        restore_private_file(&event_path, b"partial-event\n")?;

        assert!(matches!(
            workspace.into_production_projection_writer(),
            Err(WorkspaceError::ProductionProjectionRecoveryUnavailable)
        ));
        std::fs::remove_file(&event_path)?;
        assert!(retry.into_production_projection_writer().is_ok());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    /// Return value of [`advance_past_pristine`]: everything a recovery-writer
    /// test needs to reopen a non-pristine production workspace.
    struct AdvancedWorkspace {
        parent: PathBuf,
        production_root: Arc<ProductionBoundary>,
        mission_id: MissionId,
        target_checkpoint: CheckpointProjection,
        target_state: MissionState,
        target_checkpoint_bytes: Vec<u8>,
    }

    /// Advances a fresh production workspace one checkpoint past pristine
    /// (status `"pending"` -> `"in_progress"`) via the ordinary pristine
    /// writer, then drops that writer so its lease is released. Returns the
    /// production root and the exact target checkpoint bytes now durable on
    /// disk — the shared setup every
    /// `into_production_projection_writer_recovered` test below builds on,
    /// since that constructor only has anything to prove once
    /// `into_production_projection_writer` itself would refuse.
    fn advance_past_pristine(label: &str) -> Result<AdvancedWorkspace, Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture(label)?;
        let mission_id = MissionId::new(format!("{label}-mission"))?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let phase_id = PhaseId::new("phase-1")?;
        let plan = CheckpointPlan {
            phases: vec![CheckpointPhase {
                id: phase_id.as_str().to_owned(),
                ..CheckpointPhase::default()
            }],
            ..CheckpointPlan::default()
        };
        let pristine_state = MissionState::new(
            mission_id.clone(),
            vec![PhaseDefinition {
                id: phase_id,
                dependencies: Vec::new(),
            }],
        )?;
        let prior = checkpoint_for_state(
            &CheckpointProjection {
                workspace_id: mission_id.as_str().to_owned(),
                plan: Some(plan),
                ..CheckpointProjection::default()
            },
            &pristine_state,
        )?;
        let started = ReducerInput {
            event_id: EventId::new(format!("evt-{label}-mission-started"))?,
            sequence: 1,
            timestamp: "2026-07-14T00:00:00Z".to_owned(),
            mission_id: mission_id.clone(),
            phase_id: None,
            worker_id: None,
            data: serde_json::Value::Null,
            extra: BTreeMap::new(),
            transition: ReducerTransition::MissionStarted,
        };
        let target_state = reduce(&pristine_state, &started)?.state;
        let target = checkpoint_for_state(&prior, &target_state)?;
        let prior_bytes = encode_current_checkpoint(&prior)?;
        let target_bytes = encode_current_checkpoint(&target)?;
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            WorkspaceSeed::new(
                b"recovery canary mission\n".to_vec(),
                &prior,
                b"{}".to_vec(),
            )?,
        )?;
        let writer = workspace.into_production_projection_writer()?;
        writer.reconcile_checkpoint(&prior_bytes, &target_bytes)?;
        drop(writer);
        Ok(AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint: target,
            target_state,
            target_checkpoint_bytes: target_bytes,
        })
    }

    #[test]
    fn recovery_writer_grants_authority_for_verified_matching_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            target_checkpoint_bytes: target_bytes,
        } = advance_past_pristine("recovery-happy-path")?;

        // The pristine constructor genuinely refuses this workspace now —
        // proves this test's setup actually reached the gap, not some other
        // failure.
        let stale =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        assert!(matches!(
            stale.into_production_projection_writer(),
            Err(WorkspaceError::ProductionProjectionRecoveryUnavailable)
        ));

        let recovering =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        let writer = recovering.into_production_projection_writer_recovered(expectation)?;
        assert_eq!(writer.mission_id(), &mission_id);

        // The recovered writer is a fully functioning production writer, not
        // a read-only stand-in: an idempotent reconcile to its own already-
        // durable target succeeds through it.
        let replay = writer.reconcile_checkpoint(&target_bytes, &target_bytes)?;
        assert_eq!(replay.checkpoint_bytes(), target_bytes.as_slice());

        drop(writer);
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn recovery_writer_rejects_divergent_checkpoint_bytes_without_mutating_or_leaking_the_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            target_checkpoint_bytes: target_bytes,
        } = advance_past_pristine("recovery-divergent-checkpoint")?;
        let mut wrong = target_checkpoint.clone();
        wrong.status = "completed".to_owned();
        let wrong_bytes = encode_current_checkpoint(&wrong)?;
        let checkpoint_path = production_root
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");
        restore_private_file(&checkpoint_path, &wrong_bytes)?;

        let recovering =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(matches!(
            recovering.into_production_projection_writer_recovered(expectation),
            Err(WorkspaceError::ExactBaseDivergent { .. })
        ));

        // Nothing mutated: the on-disk checkpoint is exactly what it was
        // before the rejected attempt.
        assert_eq!(std::fs::read(&checkpoint_path)?, wrong_bytes);

        // Lease released: a fresh attempt with the correct bytes succeeds
        // rather than observing `ProjectionWriterLeased` as residue of the
        // rejected one.
        restore_private_file(&checkpoint_path, &target_bytes)?;
        let retry = WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id)?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(
            retry
                .into_production_projection_writer_recovered(expectation)
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn recovery_writer_rejects_another_workspace_marker_without_mutation_or_lease_residue()
    -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            target_checkpoint_bytes,
        } = advance_past_pristine("recovery-marker-local")?;
        let AdvancedWorkspace {
            parent: foreign_parent,
            target_checkpoint: foreign_checkpoint,
            target_state: foreign_state,
            ..
        } = advance_past_pristine("recovery-marker-foreign")?;
        let checkpoint_path = production_root
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");

        let recovering =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let foreign_expectation =
            journal_checkpoint_expectation(&foreign_checkpoint, &foreign_state)?;
        assert!(matches!(
            recovering.into_production_projection_writer_recovered(foreign_expectation),
            Err(WorkspaceError::CheckpointMissionMismatch { .. })
        ));
        assert_eq!(std::fs::read(&checkpoint_path)?, target_checkpoint_bytes);

        let retry = WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id)?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(
            retry
                .into_production_projection_writer_recovered(expectation)
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&foreign_parent);
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn recovery_writer_rejects_foreign_fixture_history_in_the_event_log()
    -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            ..
        } = advance_past_pristine("recovery-foreign-history")?;
        let event_path = production_root
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("events.jsonl");
        // Simulates a fixture-lane writer having owned this workspace at
        // some point: workspace-local `events.jsonl` is non-empty, which no
        // production composition in this crate ever writes.
        restore_private_file(&event_path, b"fixture-lane-history\n")?;

        let recovering =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(matches!(
            recovering.into_production_projection_writer_recovered(expectation),
            Err(WorkspaceError::ForeignEventLogHistory)
        ));

        // Removing the foreign history (never done by this constructor
        // itself — a human/operator action) lets recovery proceed normally,
        // proving the rejection was specifically about the foreign content,
        // not a permanent property of this checkpoint's bytes.
        std::fs::remove_file(&event_path)?;
        let retry = WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id)?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(
            retry
                .into_production_projection_writer_recovered(expectation)
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn recovery_writer_rejects_a_hard_linked_event_log() -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            ..
        } = advance_past_pristine("recovery-event-log-hardlink")?;
        let workspace_dir = production_root
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str());
        let event_path = workspace_dir.join("events.jsonl");
        restore_private_file(&event_path, b"")?;
        std::fs::hard_link(&event_path, workspace_dir.join("events.jsonl.extra-link"))?;

        let recovering =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id)?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(matches!(
            recovering.into_production_projection_writer_recovered(expectation),
            Err(WorkspaceError::InvalidKnownEntry { .. })
        ));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn recovery_writer_lease_remains_exclusive() -> Result<(), Box<dyn std::error::Error>> {
        let AdvancedWorkspace {
            parent,
            production_root,
            mission_id,
            target_checkpoint,
            target_state,
            ..
        } = advance_past_pristine("recovery-lease-exclusive")?;

        let first =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        let writer = first.into_production_projection_writer_recovered(expectation)?;

        // A second recovered attempt for the same identity is rejected while
        // the first writer is live...
        let second =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(matches!(
            second.into_production_projection_writer_recovered(expectation),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));
        // ...and so is the ordinary pristine constructor: this is the same
        // lease, not a second mechanism.
        let third =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id.clone())?;
        assert!(matches!(
            third.into_production_projection_writer(),
            Err(WorkspaceError::ProjectionWriterLeased)
        ));

        drop(writer);
        let fourth =
            WorkspaceAuthority::admit_production(Arc::clone(&production_root), mission_id)?;
        let expectation = journal_checkpoint_expectation(&target_checkpoint, &target_state)?;
        assert!(
            fourth
                .into_production_projection_writer_recovered(expectation)
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn projection_acknowledgement_rejects_workspace_rename()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("projection-rename")?;
        let mission_id = MissionId::new("projection-rename-mission")?;
        let seed = FixtureWorkspaceSeed::new(
            b"fixture mission\n".to_vec(),
            &checkpoint(mission_id.as_str(), "pending"),
            b"{}".to_vec(),
        )?;
        let workspace = authority.create_workspace(mission_id.clone(), seed)?;
        let writer = workspace.into_fixture_projection_writer()?;
        let expected_event_log = writer.core.expected_event_log.clone();
        let expected_checkpoint = writer.core.expected_checkpoint.clone();
        let workspace_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str());
        let displaced = parent.join("displaced-workspace");
        std::fs::rename(&workspace_path, &displaced)?;
        private_dir(&workspace_path)?;

        assert!(matches!(
            writer.core.workspace.acknowledge_projection(
                &expected_event_log,
                &expected_checkpoint,
                "test projection acknowledgement",
            ),
            Err(WorkspaceError::IdentityChanged)
        ));
        drop(writer);
        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn verify_exact_base_accepts_exact_bytes_and_rejects_divergence()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("verify-exact-base")?;
        let mission_id = MissionId::new("verify-exact-base-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let plan = CheckpointPlan {
            phases: vec![CheckpointPhase {
                id: "phase-1".to_owned(),
                status: "pending".to_owned(),
                ..CheckpointPhase::default()
            }],
            ..CheckpointPlan::default()
        };
        let mut cp = checkpoint(mission_id.as_str(), "pending");
        cp.plan = Some(plan.clone());
        let plan_bytes = encode_current_plan(&plan)?;
        let mission_bytes = b"production canary mission\n".to_vec();
        let checkpoint_bytes = encode_current_checkpoint(&cp)?;
        let seed = WorkspaceSeed::new(mission_bytes.clone(), &cp, plan_bytes.clone())?;
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            seed,
        )?;
        let writer = workspace.into_production_projection_writer()?;

        let verified = writer.verify_exact_base(&mission_bytes, &plan, &checkpoint_bytes)?;
        assert_eq!(verified.mission_id(), &mission_id);

        assert!(matches!(
            writer.verify_exact_base(b"tampered mission\n", &plan, &checkpoint_bytes),
            Err(WorkspaceError::ExactBaseDivergent { .. })
        ));

        // A rejected verification must never mutate the workspace.
        let on_disk = std::fs::read(
            authority
                .boundary
                .canonical_path()
                .join("workspaces")
                .join(mission_id.as_str())
                .join("mission.md"),
        )?;
        assert_eq!(on_disk, mission_bytes);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn verify_exact_base_rejects_a_hard_linked_entry() -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("verify-exact-base-hardlink")?;
        let mission_id = MissionId::new("verify-exact-base-hardlink-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let plan = CheckpointPlan::default();
        let mut cp = checkpoint(mission_id.as_str(), "pending");
        cp.plan = Some(plan.clone());
        let plan_bytes = encode_current_plan(&plan)?;
        let mission_bytes = b"hardlink canary mission\n".to_vec();
        let checkpoint_bytes = encode_current_checkpoint(&cp)?;
        let seed = WorkspaceSeed::new(mission_bytes.clone(), &cp, plan_bytes.clone())?;
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            seed,
        )?;
        let workspace_dir = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str());
        std::fs::hard_link(
            workspace_dir.join("mission.md"),
            workspace_dir.join("mission.md.extra-link"),
        )?;
        let writer = workspace.into_production_projection_writer()?;

        assert!(matches!(
            writer.verify_exact_base(&mission_bytes, &plan, &checkpoint_bytes),
            Err(WorkspaceError::InvalidKnownEntry { .. })
        ));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn reconcile_checkpoint_publishes_then_replays_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("reconcile-checkpoint")?;
        let mission_id = MissionId::new("reconcile-checkpoint-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let prior = checkpoint(mission_id.as_str(), "pending");
        let mut target = prior.clone();
        target.status = "in_progress".to_owned();
        let prior_bytes = encode_current_checkpoint(&prior)?;
        let target_bytes = encode_current_checkpoint(&target)?;
        let seed = WorkspaceSeed::new(
            b"reconcile canary mission\n".to_vec(),
            &prior,
            b"{}".to_vec(),
        )?;
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            seed,
        )?;
        let writer = workspace.into_production_projection_writer()?;

        let verified = writer.reconcile_checkpoint(&prior_bytes, &target_bytes)?;
        assert_eq!(verified.checkpoint_bytes(), target_bytes.as_slice());
        assert_eq!(
            verified.disposition(),
            CheckpointReconciliationDisposition::Published
        );

        let checkpoint_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");
        assert_eq!(std::fs::read(&checkpoint_path)?, target_bytes);

        // A retry after the first publish's acknowledgement was lost is an
        // idempotent crossed-crash-boundary success, not a write or an error.
        let replay = writer.reconcile_checkpoint(&prior_bytes, &target_bytes)?;
        assert_eq!(replay.checkpoint_bytes(), target_bytes.as_slice());
        assert_eq!(
            replay.disposition(),
            CheckpointReconciliationDisposition::AlreadyTarget
        );
        assert_eq!(std::fs::read(&checkpoint_path)?, target_bytes);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn reconcile_checkpoint_rejects_divergent_current_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (parent, authority) = admit_fixture("reconcile-checkpoint-divergent")?;
        let mission_id = MissionId::new("reconcile-checkpoint-divergent-mission")?;
        let production_root = Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?);
        let prior = checkpoint(mission_id.as_str(), "pending");
        let mut target = prior.clone();
        target.status = "in_progress".to_owned();
        let prior_bytes = encode_current_checkpoint(&prior)?;
        let target_bytes = encode_current_checkpoint(&target)?;
        let seed = WorkspaceSeed::new(
            b"divergent canary mission\n".to_vec(),
            &prior,
            b"{}".to_vec(),
        )?;
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&production_root),
            mission_id.clone(),
            seed,
        )?;
        let writer = workspace.into_production_projection_writer()?;

        let checkpoint_path = authority
            .boundary
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("checkpoint.json");
        let divergent = b"{\"unexpected\":true}".to_vec();
        restore_private_file(&checkpoint_path, &divergent)?;

        assert!(matches!(
            writer.reconcile_checkpoint(&prior_bytes, &target_bytes),
            Err(WorkspaceError::ExactBaseDivergent { .. })
        ));
        assert_eq!(std::fs::read(&checkpoint_path)?, divergent);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }
}
