//! Retained whole-home writer authority for enrolled production execution.

use std::{
    ffi::OsString,
    fmt,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};

use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
use serde::Serialize;
use thiserror::Error;

use crate::{
    ApplicationError, AuthorizedProductionRuntimeHome, ProductionBoundary,
    fs_util::{FileIdentity, identity, link_count, mode, open_file_nofollow, sync_dir},
};

/// Cross-implementation lock path fixed by ADR-0001.
pub const WRITER_LOCK_FILE: &str = "orchestrator.writer.lock";

const LEGACY_DAEMON_PID_FILE: &str = "daemon.pid";
const LEGACY_WORKSPACES_DIRECTORY: &str = "workspaces";
const LEGACY_WORKSPACE_PID_FILE: &str = "pid";
const MAX_LEGACY_PID_BYTES: usize = 32;
const MAX_LEGACY_WORKSPACE_NAME_BYTES: usize = 255;
const MAX_LEGACY_WORKSPACES: usize = 65_536;
const MAX_VERSION_BYTES: usize = 128;
const MAX_START_IDENTITY_BYTES: usize = 256;

/// OS-derived process-start identity retained in writer diagnostics.
#[derive(Clone, Eq, PartialEq)]
struct ProcessStartIdentity(String);

impl ProcessStartIdentity {
    fn new(value: impl Into<String>) -> Result<Self, WriterAuthorityError> {
        let value = value.into();
        if !valid_diagnostic_atom(&value, MAX_START_IDENTITY_BYTES) {
            return Err(WriterAuthorityError::InvalidMetadata);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProcessStartIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcessStartIdentity(REDACTED)")
    }
}

/// Validated diagnostic identity written only after the kernel lease is held.
struct WriterLeaseMetadata {
    binary_version: String,
    process_start_identity: ProcessStartIdentity,
    acquired_at_unix_ms: u64,
}

impl WriterLeaseMetadata {
    fn new(
        binary_version: impl Into<String>,
        process_start_identity: ProcessStartIdentity,
        acquired_at: SystemTime,
    ) -> Result<Self, WriterAuthorityError> {
        let binary_version = binary_version.into();
        if !valid_diagnostic_atom(&binary_version, MAX_VERSION_BYTES) {
            return Err(WriterAuthorityError::InvalidMetadata);
        }
        let acquired_at_unix_ms = acquired_at
            .duration_since(UNIX_EPOCH)
            .map_err(|_| WriterAuthorityError::InvalidMetadata)?
            .as_millis()
            .try_into()
            .map_err(|_| WriterAuthorityError::InvalidMetadata)?;
        if std::process::id() == 0 {
            return Err(WriterAuthorityError::InvalidMetadata);
        }
        Ok(Self {
            binary_version,
            process_start_identity,
            acquired_at_unix_ms,
        })
    }

    fn record(&self) -> WriterLockRecord<'_> {
        WriterLockRecord {
            schema_version: 1,
            implementation: "rust",
            binary_version: &self.binary_version,
            pid: std::process::id(),
            process_start_identity: self.process_start_identity.as_str(),
            acquired_at_unix_ms: self.acquired_at_unix_ms,
        }
    }
}

#[derive(Eq, PartialEq)]
struct LegacyPidSnapshot {
    identity: FileIdentity,
    bytes: Vec<u8>,
}

#[derive(Eq, PartialEq)]
struct LegacyWorkspaceSnapshot {
    name: OsString,
    identity: FileIdentity,
    pid: Option<LegacyPidSnapshot>,
}

#[derive(Eq, PartialEq)]
struct LegacyWorkspacesSnapshot {
    identity: FileIdentity,
    workspaces: Vec<LegacyWorkspaceSnapshot>,
}

#[derive(Eq, PartialEq)]
struct LegacyWriterSnapshot {
    daemon_pid: Option<LegacyPidSnapshot>,
    workspaces: Option<LegacyWorkspacesSnapshot>,
}

/// Read-only proof that no legacy Go daemon or mission writer was live for one
/// existing production home at inspection time.
///
/// The proof retains the inspected root capability and filesystem identity. It
/// is deliberately opaque, non-cloneable, and consumed by
/// [`ProductionWriterAuthority::acquire`], which repeats the bounded scan under
/// its preparation mutex immediately before the first mutation. The sole
/// inspector remains crate-private until every Go mutator honors the shared
/// writer lease: double scanning alone cannot close the race with a newly
/// launched legacy process.
pub struct LegacyQuiescenceProof {
    canonical_path: PathBuf,
    directory: Dir,
    root_identity: FileIdentity,
    snapshot: LegacyWriterSnapshot,
}

impl LegacyQuiescenceProof {
    /// Inspects an existing enrolled home without creating, deleting, or
    /// rewriting legacy Go state.
    ///
    /// Missing homes fail closed in this enrollment slice. PID files are
    /// accepted only when private, single-linked, bounded, and well formed.
    /// Stale PID files are retained and represented in the proof; any PID that
    /// currently names a process prevents admission.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "external proof minting stays disabled until Go shares the writer lease"
        )
    )]
    pub(crate) fn inspect(
        authorized: &AuthorizedProductionRuntimeHome,
    ) -> Result<Self, WriterAuthorityError> {
        let (directory, root_identity) = authorized.inspect_existing_root()?;
        validate_legacy_directory(&directory, root_identity)?;
        let snapshot = legacy_writer_snapshot(&directory, root_identity)?;
        let (reopened, reopened_identity) = reopen_legacy_root(authorized)?;
        if reopened_identity != root_identity {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        validate_legacy_directory(&reopened, root_identity)?;
        if legacy_writer_snapshot(&reopened, root_identity)? != snapshot {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        Ok(Self {
            canonical_path: authorized.path().to_path_buf(),
            directory,
            root_identity,
            snapshot,
        })
    }

    fn revalidate(
        &self,
        authorized: &AuthorizedProductionRuntimeHome,
        candidate: &Dir,
    ) -> Result<(), WriterAuthorityError> {
        if self.canonical_path != authorized.path() {
            return Err(WriterAuthorityError::QuiescenceMismatch);
        }
        validate_legacy_directory(&self.directory, self.root_identity)?;
        validate_legacy_directory(candidate, self.root_identity)?;
        if stable_legacy_writer_snapshot(candidate, self.root_identity)? != self.snapshot {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }

        let (reopened, reopened_identity) = reopen_legacy_root(authorized)?;
        if reopened_identity != self.root_identity {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        validate_legacy_directory(&reopened, self.root_identity)?;
        Ok(())
    }
}

impl fmt::Debug for LegacyQuiescenceProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacyQuiescenceProof")
            .field("home", &"REDACTED")
            .finish()
    }
}

impl fmt::Debug for WriterLeaseMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterLeaseMetadata")
            .field("implementation", &"rust")
            .field("pid", &std::process::id())
            .field("process_start_identity", &"REDACTED")
            .finish()
    }
}

#[derive(Serialize)]
struct WriterLockRecord<'a> {
    schema_version: u32,
    implementation: &'a str,
    binary_version: &'a str,
    pid: u32,
    process_start_identity: &'a str,
    acquired_at_unix_ms: u64,
}

/// Failures that prevent exclusive production-home authority.
#[derive(Debug, Error)]
pub enum WriterAuthorityError {
    /// The diagnostic identity is empty, unbounded, or not portable ASCII.
    #[error("writer authority metadata is invalid")]
    InvalidMetadata,
    /// The platform cannot securely derive the current process start identity.
    #[error("writer process identity is unsupported on this platform")]
    UnsupportedPlatform,
    /// The supplied quiescence proof belongs to a different runtime home.
    #[error("legacy-writer quiescence proof does not match the runtime home")]
    QuiescenceMismatch,
    /// Legacy PID state is malformed or cannot be interpreted unambiguously.
    #[error("legacy-writer PID state is invalid")]
    InvalidLegacyPid,
    /// A legacy PID path or directory has an unsafe filesystem shape.
    #[error("legacy-writer state has an unsafe filesystem shape")]
    UnsafeLegacyState,
    /// Legacy writer state or its retained root changed between observations.
    #[error("legacy-writer state changed during quiescence reconciliation")]
    LegacyStateChanged,
    /// Another cooperative implementation currently owns the home.
    #[error("runtime home already has a mutating authority")]
    Busy,
    /// The lock entry is not a private, single-linked regular file.
    #[error("runtime-home writer lock has an unsafe filesystem shape")]
    UnsafeLock,
    /// Enrollment or retained home identity verification failed.
    #[error(transparent)]
    Home(#[from] ApplicationError),
    /// A stable lock operation failed without revealing the home path.
    #[error("writer authority operation failed during {operation}")]
    Operation {
        /// Non-sensitive operation label.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
}

impl WriterAuthorityError {
    fn operation(operation: &'static str, source: impl Into<std::io::Error>) -> Self {
        Self::Operation {
            operation,
            source: source.into(),
        }
    }
}

/// The sole Rust mutation authority for one enrolled production home.
///
/// Every returned [`ProductionBoundary`] retains the same kernel lease, so
/// dropping this wrapper cannot leave a live mutation capability unlocked.
pub struct ProductionWriterAuthority {
    boundary: Arc<ProductionBoundary>,
}

impl ProductionWriterAuthority {
    pub(crate) fn acquire_rust_pilot(
        path: PathBuf,
        directory: Dir,
        binary_version: String,
        root_identity: FileIdentity,
    ) -> Result<Self, WriterAuthorityError> {
        if !valid_diagnostic_atom(&binary_version, MAX_VERSION_BYTES) {
            return Err(WriterAuthorityError::InvalidMetadata);
        }
        let seal = Path::new("orchestrator.rust-pilot.seal");
        let expected = format!("nanika-rust-pilot-v1\n{binary_version}\n");
        let initial_seal =
            crate::fs_util::read_bounded_nofollow(&directory, seal, 512).map_err(|source| {
                WriterAuthorityError::operation("inspect pilot ownership seal", source)
            })?;
        if initial_seal
            .as_deref()
            .is_some_and(|bytes| bytes != expected.as_bytes())
        {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        let lock = open_writer_lock(&directory)?;
        validate_writer_lock(&lock)?;
        acquire_kernel_lock(&lock)?;
        validate_writer_lock(&lock)?;
        if identity(&directory.dir_metadata().map_err(|source| {
            WriterAuthorityError::operation("inspect retained pilot root", source)
        })?) != root_identity
        {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        let entries = directory
            .entries()
            .map_err(|source| {
                WriterAuthorityError::operation("inspect pilot root contents", source)
            })?
            .map(|entry| {
                entry.map(|entry| entry.file_name()).map_err(|source| {
                    WriterAuthorityError::operation("inspect pilot root entry", source)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let allowed = if initial_seal.is_some() {
            entries.iter().all(|name| {
                directory
                    .symlink_metadata(Path::new(name))
                    .map(|metadata| metadata.file_type().is_file() || metadata.file_type().is_dir())
                    .unwrap_or(false)
            }) && entries.iter().any(|name| name == seal.as_os_str())
                && entries
                    .iter()
                    .any(|name| name == Path::new(WRITER_LOCK_FILE).as_os_str())
        } else {
            entries.len() == 1 && entries[0] == Path::new(WRITER_LOCK_FILE).as_os_str()
        };
        if !allowed {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        let seal_bytes =
            crate::fs_util::read_bounded_nofollow(&directory, seal, 512).map_err(|source| {
                WriterAuthorityError::operation("recheck pilot ownership seal", source)
            })?;
        if seal_bytes
            .as_deref()
            .is_some_and(|bytes| bytes != expected.as_bytes())
        {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        if seal_bytes.is_some() {
            validate_pilot_seal(&directory, seal)?;
        }
        if seal_bytes.is_none() {
            crate::fs_util::create_private_file(&directory, seal, expected.as_bytes(), false)
                .map_err(|source| {
                    WriterAuthorityError::operation("create pilot ownership seal", source)
                })?;
            sync_dir(&directory).map_err(|source| {
                WriterAuthorityError::operation("synchronize pilot seal directory", source)
            })?;
        }
        let lock_metadata = lock.metadata().map_err(|source| {
            WriterAuthorityError::operation("inspect retained writer lock", source)
        })?;
        let lock_identity = identity(&lock_metadata);
        let lease = Arc::new(WriterLeaseGuard {
            lock,
            identity: lock_identity,
        });
        lease.verify(&directory).map_err(|source| {
            WriterAuthorityError::operation("verify retained writer lock", source)
        })?;
        let boundary = Arc::new(ProductionBoundary::from_pilot_lease(
            path, directory, lease,
        )?);
        boundary.verify().map_err(|e| {
            WriterAuthorityError::operation("verify pilot boundary", std::io::Error::other(e))
        })?;
        Ok(Self { boundary })
    }
    /// Acquires the whole-home lease before sealing a production boundary.
    pub fn acquire(
        authorized: &AuthorizedProductionRuntimeHome,
        binary_version: impl Into<String>,
        quiescence: LegacyQuiescenceProof,
    ) -> Result<Self, WriterAuthorityError> {
        if quiescence.canonical_path != authorized.path() {
            return Err(WriterAuthorityError::QuiescenceMismatch);
        }
        let binary_version = binary_version.into();
        if !valid_diagnostic_atom(&binary_version, MAX_VERSION_BYTES) {
            return Err(WriterAuthorityError::InvalidMetadata);
        }
        let process_start_identity = current_process_start_identity()?;
        let preparation = authorized.begin_writer_preparation()?;
        let root_identity = quiescence.root_identity;
        let (directory, candidate_identity) = preparation
            .existing_candidate(root_identity)
            .map_err(legacy_candidate_error)?;
        if candidate_identity != root_identity {
            return Err(WriterAuthorityError::LegacyStateChanged);
        }
        quiescence.revalidate(authorized, &directory)?;
        let mut lock = open_writer_lock(&directory)?;
        validate_writer_lock(&lock)?;
        acquire_kernel_lock(&lock)?;
        validate_writer_lock(&lock)?;
        let metadata =
            WriterLeaseMetadata::new(binary_version, process_start_identity, SystemTime::now())?;
        write_lock_record(&mut lock, &metadata)?;
        let lock_identity = identity(&lock.metadata().map_err(|source| {
            WriterAuthorityError::operation("inspect retained writer lock", source)
        })?);
        let lease = Arc::new(WriterLeaseGuard {
            lock,
            identity: lock_identity,
        });
        lease.verify(&directory).map_err(|source| {
            WriterAuthorityError::operation("verify retained writer lock", source)
        })?;
        sync_dir(&directory).map_err(|source| {
            WriterAuthorityError::operation("synchronize writer-lock directory", source)
        })?;
        preparation.commit(directory, root_identity)?;

        let boundary = Arc::new(ProductionBoundary::from_writer_lease(
            authorized,
            Arc::clone(&lease),
        )?);
        boundary.verify().map_err(|source| {
            WriterAuthorityError::operation(
                "verify writer-owned production boundary",
                std::io::Error::other(source),
            )
        })?;
        Ok(Self { boundary })
    }

    /// Clones the retained production boundary without releasing its lease.
    #[must_use]
    pub fn boundary(&self) -> Arc<ProductionBoundary> {
        Arc::clone(&self.boundary)
    }
}

fn validate_pilot_seal(directory: &Dir, name: &Path) -> Result<(), WriterAuthorityError> {
    let file = open_file_nofollow(directory, name).map_err(|_| WriterAuthorityError::UnsafeLock)?;
    let metadata = file
        .metadata()
        .map_err(|_| WriterAuthorityError::UnsafeLock)?;
    if !metadata.is_file() || link_count(&metadata) != 1 || mode(&metadata) != 0o600 {
        return Err(WriterAuthorityError::UnsafeLock);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(WriterAuthorityError::UnsafeLock);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_process_start_identity() -> Result<ProcessStartIdentity, WriterAuthorityError> {
    const MAX_PROC_STAT_BYTES: usize = 4 * 1024;

    let bytes = std::fs::read("/proc/self/stat").map_err(|source| {
        WriterAuthorityError::operation("read current process identity", source)
    })?;
    if bytes.len() > MAX_PROC_STAT_BYTES {
        return Err(WriterAuthorityError::InvalidMetadata);
    }
    let text = std::str::from_utf8(&bytes).map_err(|source| {
        WriterAuthorityError::operation(
            "decode current process identity",
            std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        )
    })?;
    let close = text
        .rfind(')')
        .ok_or(WriterAuthorityError::InvalidMetadata)?;
    let fields = text[close + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let start_ticks = fields
        .get(19)
        .ok_or(WriterAuthorityError::InvalidMetadata)?;
    start_ticks
        .parse::<u64>()
        .map_err(|_| WriterAuthorityError::InvalidMetadata)?;
    ProcessStartIdentity::new(format!("linux:{start_ticks}"))
}

#[cfg(target_os = "macos")]
fn current_process_start_identity() -> Result<ProcessStartIdentity, WriterAuthorityError> {
    let pid = std::process::id().to_string();
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|source| {
            WriterAuthorityError::operation("probe current process identity", source)
        })?;
    if !output.status.success() || output.stdout.len() > MAX_START_IDENTITY_BYTES {
        return Err(WriterAuthorityError::InvalidMetadata);
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| WriterAuthorityError::InvalidMetadata)?;
    let tokens = text.split_ascii_whitespace().collect::<Vec<_>>();
    if tokens.len() != 5 {
        return Err(WriterAuthorityError::InvalidMetadata);
    }
    ProcessStartIdentity::new(format!("darwin:{}", tokens.join("-")))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_process_start_identity() -> Result<ProcessStartIdentity, WriterAuthorityError> {
    Err(WriterAuthorityError::UnsupportedPlatform)
}

impl fmt::Debug for ProductionWriterAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionWriterAuthority")
            .field("kind", &"exclusive-whole-home-writer")
            .finish()
    }
}

pub(crate) struct WriterLeaseGuard {
    lock: cap_std::fs::File,
    identity: FileIdentity,
}

impl WriterLeaseGuard {
    pub(crate) fn verify(&self, directory: &Dir) -> std::io::Result<()> {
        validate_writer_lock_io(&self.lock)?;
        if identity(&self.lock.metadata()?) != self.identity {
            return Err(std::io::Error::other(
                "retained writer-lock identity changed",
            ));
        }
        let mapped = open_file_nofollow(directory, Path::new(WRITER_LOCK_FILE))?;
        validate_writer_lock_io(&mapped)?;
        if identity(&mapped.metadata()?) != self.identity {
            return Err(std::io::Error::other(
                "writer-lock path no longer maps to the retained lease",
            ));
        }
        Ok(())
    }
}

fn valid_diagnostic_atom(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

fn stable_legacy_writer_snapshot(
    directory: &Dir,
    expected_root: FileIdentity,
) -> Result<LegacyWriterSnapshot, WriterAuthorityError> {
    let first = legacy_writer_snapshot(directory, expected_root)?;
    let second = legacy_writer_snapshot(directory, expected_root)?;
    if first == second {
        Ok(second)
    } else {
        Err(WriterAuthorityError::LegacyStateChanged)
    }
}

fn reopen_legacy_root(
    authorized: &AuthorizedProductionRuntimeHome,
) -> Result<(Dir, FileIdentity), WriterAuthorityError> {
    authorized
        .inspect_existing_root()
        .map_err(legacy_candidate_error)
}

fn legacy_candidate_error(error: ApplicationError) -> WriterAuthorityError {
    match error {
        ApplicationError::RuntimeHomeIdentityChanged => WriterAuthorityError::LegacyStateChanged,
        other => WriterAuthorityError::Home(other),
    }
}

fn legacy_writer_snapshot(
    directory: &Dir,
    expected_root: FileIdentity,
) -> Result<LegacyWriterSnapshot, WriterAuthorityError> {
    validate_legacy_directory(directory, expected_root)?;
    let daemon_pid = inspect_legacy_pid(directory, Path::new(LEGACY_DAEMON_PID_FILE))?;
    let workspaces = inspect_legacy_workspaces(directory)?;
    validate_legacy_directory(directory, expected_root)?;
    Ok(LegacyWriterSnapshot {
        daemon_pid,
        workspaces,
    })
}

fn inspect_legacy_workspaces(
    root: &Dir,
) -> Result<Option<LegacyWorkspacesSnapshot>, WriterAuthorityError> {
    let workspaces = match crate::fs_util::open_dir_path_nofollow(
        root,
        Path::new(LEGACY_WORKSPACES_DIRECTORY),
    ) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(WriterAuthorityError::UnsafeLegacyState),
    };
    let metadata = workspaces
        .dir_metadata()
        .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
    let workspaces_identity = identity(&metadata);
    validate_legacy_directory(&workspaces, workspaces_identity)?;

    let mut snapshots = Vec::new();
    let entries = workspaces
        .entries()
        .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
    let mut entry_count = 0_usize;
    for entry in entries {
        let entry = entry.map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
        entry_count = entry_count
            .checked_add(1)
            .ok_or(WriterAuthorityError::UnsafeLegacyState)?;
        if entry_count > MAX_LEGACY_WORKSPACES {
            return Err(WriterAuthorityError::UnsafeLegacyState);
        }
        let name = entry.file_name();
        if name.as_encoded_bytes().is_empty()
            || name.as_encoded_bytes().len() > MAX_LEGACY_WORKSPACE_NAME_BYTES
        {
            return Err(WriterAuthorityError::UnsafeLegacyState);
        }
        let file_type = entry
            .file_type()
            .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
        if file_type.is_file() {
            // Go's workspace discovery ignores benign regular entries. They
            // carry no writer PID and are left byte-for-byte untouched.
            continue;
        }
        if !file_type.is_dir() {
            return Err(WriterAuthorityError::UnsafeLegacyState);
        }
        let workspace = crate::fs_util::open_dir_path_nofollow(&workspaces, Path::new(&name))
            .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
        let workspace_metadata = workspace
            .dir_metadata()
            .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
        let workspace_identity = identity(&workspace_metadata);
        validate_legacy_directory(&workspace, workspace_identity)?;
        let pid = inspect_legacy_pid(&workspace, Path::new(LEGACY_WORKSPACE_PID_FILE))?;
        let remapped = crate::fs_util::open_dir_path_nofollow(&workspaces, Path::new(&name))
            .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
        validate_legacy_directory(&remapped, workspace_identity)
            .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
        snapshots.push(LegacyWorkspaceSnapshot {
            name,
            identity: workspace_identity,
            pid,
        });
    }
    snapshots.sort_by(|left, right| left.name.cmp(&right.name));

    let remapped =
        crate::fs_util::open_dir_path_nofollow(root, Path::new(LEGACY_WORKSPACES_DIRECTORY))
            .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    validate_legacy_directory(&remapped, workspaces_identity)
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    Ok(Some(LegacyWorkspacesSnapshot {
        identity: workspaces_identity,
        workspaces: snapshots,
    }))
}

fn inspect_legacy_pid(
    directory: &Dir,
    name: &Path,
) -> Result<Option<LegacyPidSnapshot>, WriterAuthorityError> {
    let mut file = match open_file_nofollow(directory, name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(WriterAuthorityError::UnsafeLegacyState),
    };
    let before = file
        .metadata()
        .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
    validate_legacy_pid_metadata(&before)?;
    let file_identity = identity(&before);
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_LEGACY_PID_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    if bytes.len() > MAX_LEGACY_PID_BYTES {
        return Err(WriterAuthorityError::InvalidLegacyPid);
    }
    let after = file
        .metadata()
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    validate_legacy_pid_metadata(&after).map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    if identity(&after) != file_identity || after.len() != bytes.len() as u64 {
        return Err(WriterAuthorityError::LegacyStateChanged);
    }
    let remapped = open_file_nofollow(directory, name)
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    let remapped_metadata = remapped
        .metadata()
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    validate_legacy_pid_metadata(&remapped_metadata)
        .map_err(|_| WriterAuthorityError::LegacyStateChanged)?;
    if identity(&remapped_metadata) != file_identity {
        return Err(WriterAuthorityError::LegacyStateChanged);
    }

    let pid = parse_legacy_pid(&bytes)?;
    probe_legacy_pid(pid)?;
    Ok(Some(LegacyPidSnapshot {
        identity: file_identity,
        bytes,
    }))
}

fn parse_legacy_pid(bytes: &[u8]) -> Result<rustix::process::Pid, WriterAuthorityError> {
    let first = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .ok_or(WriterAuthorityError::InvalidLegacyPid)?;
    let last = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .ok_or(WriterAuthorityError::InvalidLegacyPid)?;
    let digits = &bytes[first..=last];
    if !digits.iter().all(u8::is_ascii_digit) {
        return Err(WriterAuthorityError::InvalidLegacyPid);
    }
    let text = std::str::from_utf8(digits).map_err(|_| WriterAuthorityError::InvalidLegacyPid)?;
    let raw = text
        .parse::<i32>()
        .map_err(|_| WriterAuthorityError::InvalidLegacyPid)?;
    if raw <= 0 {
        return Err(WriterAuthorityError::InvalidLegacyPid);
    }
    rustix::process::Pid::from_raw(raw).ok_or(WriterAuthorityError::InvalidLegacyPid)
}

#[cfg(target_os = "linux")]
fn probe_legacy_pid(pid: rustix::process::Pid) -> Result<(), WriterAuthorityError> {
    classify_legacy_pid_probe(
        rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).map(drop),
    )
}

#[cfg(target_os = "macos")]
fn probe_legacy_pid(pid: rustix::process::Pid) -> Result<(), WriterAuthorityError> {
    classify_legacy_pid_probe(rustix::process::test_kill_process(pid))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify_legacy_pid_probe(
    observation: Result<(), rustix::io::Errno>,
) -> Result<(), WriterAuthorityError> {
    match observation {
        Ok(()) | Err(rustix::io::Errno::PERM) => Err(WriterAuthorityError::Busy),
        Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(_) => Err(WriterAuthorityError::LegacyStateChanged),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_legacy_pid(_pid: rustix::process::Pid) -> Result<(), WriterAuthorityError> {
    Err(WriterAuthorityError::UnsupportedPlatform)
}

fn validate_legacy_pid_metadata(
    metadata: &cap_std::fs::Metadata,
) -> Result<(), WriterAuthorityError> {
    if !metadata.is_file()
        || link_count(metadata) != 1
        || metadata.len() > MAX_LEGACY_PID_BYTES as u64
    {
        return Err(WriterAuthorityError::UnsafeLegacyState);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.mode() & 0o7777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(WriterAuthorityError::UnsafeLegacyState);
        }
    }
    Ok(())
}

fn validate_legacy_directory(
    directory: &Dir,
    expected: FileIdentity,
) -> Result<(), WriterAuthorityError> {
    let metadata = directory
        .dir_metadata()
        .map_err(|_| WriterAuthorityError::UnsafeLegacyState)?;
    if !metadata.is_dir() || identity(&metadata) != expected {
        return Err(WriterAuthorityError::LegacyStateChanged);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        let mode = metadata.mode() & 0o7777;
        if mode & 0o7022 != 0 || metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(WriterAuthorityError::UnsafeLegacyState);
        }
    }
    Ok(())
}

fn open_writer_lock(directory: &Dir) -> Result<cap_std::fs::File, WriterAuthorityError> {
    let name = Path::new(WRITER_LOCK_FILE);
    let mut create = OpenOptions::new();
    create.read(true).write(true).create_new(true);
    create._cap_fs_ext_follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    match directory.open_with(name, &create) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use cap_std::fs::{Permissions, PermissionsExt};
                file.set_permissions(Permissions::from_mode(0o600))
                    .map_err(|source| {
                        WriterAuthorityError::operation("seal new writer lock", source)
                    })?;
            }
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = OpenOptions::new();
            existing.read(true).write(true);
            existing._cap_fs_ext_follow(FollowSymlinks::No);
            directory.open_with(name, &existing).map_err(|source| {
                WriterAuthorityError::operation("open existing writer lock", source)
            })
        }
        Err(source) => Err(WriterAuthorityError::operation(
            "create writer lock",
            source,
        )),
    }
}

fn validate_writer_lock(file: &cap_std::fs::File) -> Result<(), WriterAuthorityError> {
    validate_writer_lock_io(file).map_err(|_| WriterAuthorityError::UnsafeLock)
}

fn validate_writer_lock_io(file: &cap_std::fs::File) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || link_count(&metadata) != 1 {
        return Err(std::io::Error::other(
            "writer lock is not a single-linked regular file",
        ));
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.mode() & 0o7777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "writer lock mode or owner is unsafe",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn acquire_kernel_lock(file: &cap_std::fs::File) -> Result<(), WriterAuthorityError> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        let source = std::io::Error::from(error);
        if source.kind() == std::io::ErrorKind::WouldBlock {
            WriterAuthorityError::Busy
        } else {
            WriterAuthorityError::operation("acquire kernel writer lock", source)
        }
    })
}

#[cfg(not(unix))]
fn acquire_kernel_lock(_file: &cap_std::fs::File) -> Result<(), WriterAuthorityError> {
    Err(WriterAuthorityError::operation(
        "acquire kernel writer lock",
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "writer authority requires an operating-system file lock",
        ),
    ))
}

fn write_lock_record(
    file: &mut cap_std::fs::File,
    metadata: &WriterLeaseMetadata,
) -> Result<(), WriterAuthorityError> {
    let bytes = encode_lock_record(&metadata.record())?;
    file.set_len(0)
        .map_err(|source| WriterAuthorityError::operation("truncate writer diagnostics", source))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| WriterAuthorityError::operation("rewind writer diagnostics", source))?;
    file.write_all(&bytes)
        .map_err(|source| WriterAuthorityError::operation("write writer diagnostics", source))?;
    file.sync_all()
        .map_err(|source| WriterAuthorityError::operation("synchronize writer diagnostics", source))
}

fn encode_lock_record(record: &WriterLockRecord<'_>) -> Result<Vec<u8>, WriterAuthorityError> {
    let mut bytes = serde_json::to_vec(record).map_err(|source| {
        WriterAuthorityError::operation(
            "encode writer diagnostics",
            std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs::{self, File, OpenOptions},
        io::{BufRead, BufReader, Read, Seek, Write},
        path::PathBuf,
        process::{Child, ChildStdin, Command, ExitStatus, Stdio},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc::{Receiver, sync_channel},
        },
        time::{Duration, Instant},
    };

    use std::os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    };

    use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

    use super::*;
    use crate::{
        DirectoryProbe, HomeInputs, LiveHomeCanaryCapability, RuntimeHomeResolver,
        RustPilotProcessAttempt, RustPilotProcessError, RustPilotProcessExecutable,
        RustPilotProcessSession, WorkspaceAuthority, WorkspaceSeed, runtime_home::HomeSelection,
    };
    use orchestrator_core::{CheckpointProjection, MissionId};
    use orchestrator_exec::{ProcessPurpose, ProcessRequest};

    static CASE: AtomicU64 = AtomicU64::new(1);
    const HELPER_TIMEOUT: Duration = Duration::from_secs(5);
    const HELPER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
    const HELPER_HANDSHAKE_MAXIMUM: u64 = 128;
    const HELPER_STDERR_MAXIMUM: u64 = 64 * 1024;
    const SHARED_LOCK_RECORD_GOLDENS: &str =
        include_str!("../tests/fixtures/canonical-lock-records.json");

    #[derive(serde::Deserialize)]
    struct LockRecordGoldenSet {
        fixture_schema_version: u32,
        cases: Vec<LockRecordGolden>,
    }

    #[derive(serde::Deserialize)]
    struct LockRecordGolden {
        name: String,
        schema_version: u32,
        implementation: String,
        binary_version: String,
        pid: u32,
        process_start_identity: String,
        acquired_at_unix_ms: u64,
        canonical: String,
    }

    #[derive(Default)]
    struct Probe {
        directories: BTreeSet<PathBuf>,
    }

    impl DirectoryProbe for Probe {
        fn exists(&self, path: &Path) -> bool {
            self.directories.contains(path)
        }
    }

    struct TestHome {
        parent: PathBuf,
        path: PathBuf,
        authorized: AuthorizedProductionRuntimeHome,
    }

    impl TestHome {
        fn create(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
            let parent = temp.join(format!(
                "orchestrator-writer-{label}-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&parent)?;
            let path = parent.join("runtime");
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            let mut inputs = HomeInputs::from_user_home(parent.join("user"));
            inputs.orchestrator_config_dir = Some(path.clone());
            let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
            assert_eq!(resolved.selection(), HomeSelection::OrchestratorConfigDir);
            let enrollment = LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("1"))
                .ok_or("test enrollment was unavailable")?;
            let authorized = resolved.authorize_production(&enrollment)?;
            Ok(Self {
                parent,
                path,
                authorized,
            })
        }

        fn acquire(&self) -> Result<ProductionWriterAuthority, WriterAuthorityError> {
            let proof = LegacyQuiescenceProof::inspect(&self.authorized)?;
            ProductionWriterAuthority::acquire(&self.authorized, "0.1.0-test", proof)
        }

        fn cleanup(self) -> std::io::Result<()> {
            fs::remove_dir_all(self.parent)
        }
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HelperHandshake {
        schema_version: u32,
        state: String,
    }

    struct RustWriterHelper {
        child: Child,
        stdin: Option<ChildStdin>,
        handshake_file: File,
        stderr: Receiver<std::io::Result<Vec<u8>>>,
        reaped: bool,
        group_gone: bool,
    }

    impl RustWriterHelper {
        fn spawn(home: &TestHome, mode: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let helper_home = home.parent.join("helper-home");
            let helper_tmp = home.parent.join("helper-tmp");
            let handshake = home.parent.join(format!(
                "rust-writer-handshake-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&helper_home)?;
            fs::create_dir_all(&helper_tmp)?;
            let handshake_file = create_helper_handshake(&handshake)?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([
                    "--exact",
                    "writer_authority::tests::writer_lock_helper_process",
                    "--nocapture",
                ])
                .env_clear()
                .env("HOME", &helper_home)
                .env("TMPDIR", &helper_tmp)
                .env("LC_ALL", "C")
                .env("NANIKA_RUST_WRITER_HELPER", "1")
                .env("NANIKA_RUST_WRITER_MODE", mode)
                .env("NANIKA_RUST_WRITER_HOME", &home.path)
                .env("NANIKA_RUST_WRITER_PARENT", &home.parent)
                .env("NANIKA_RUST_WRITER_HANDSHAKE", &handshake)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .process_group(0);
            let mut child = command.spawn()?;
            let stdin = child
                .stdin
                .take()
                .ok_or("writer helper stdin was unavailable")?;
            let stderr = child
                .stderr
                .take()
                .ok_or("writer helper stderr was unavailable")?;
            let (stderr_sender, stderr_receiver) = sync_channel(1);
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = BufReader::new(stderr)
                    .take(HELPER_STDERR_MAXIMUM + 1)
                    .read_to_end(&mut bytes)
                    .and_then(|_| {
                        if bytes.len() as u64 > HELPER_STDERR_MAXIMUM {
                            Err(std::io::Error::other(
                                "writer helper stderr exceeded its bound",
                            ))
                        } else {
                            Ok(bytes)
                        }
                    });
                let _ = stderr_sender.send(result);
            });

            Ok(Self {
                child,
                stdin: Some(stdin),
                handshake_file,
                stderr: stderr_receiver,
                reaped: false,
                group_gone: false,
            })
        }

        fn expect_state(&mut self, expected: &str) -> Result<(), Box<dyn std::error::Error>> {
            let deadline = Instant::now() + HELPER_TIMEOUT;
            let actual = loop {
                if let Some(actual) = read_helper_handshake(&self.handshake_file)? {
                    break actual;
                }
                if let Some(status) = self.child.try_wait()? {
                    self.reaped = true;
                    return Err(std::io::Error::other(format!(
                        "writer helper exited before readiness with {status}"
                    ))
                    .into());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("writer helper readiness exceeded {HELPER_TIMEOUT:?}"),
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if actual != expected {
                return Err(std::io::Error::other(format!(
                    "writer helper readiness state was {actual:?}, expected {expected:?}"
                ))
                .into());
            }
            Ok(())
        }

        fn release_and_wait(mut self) -> Result<(), Box<dyn std::error::Error>> {
            let mut stdin = self
                .stdin
                .take()
                .ok_or("writer helper stdin was already released")?;
            stdin.write_all(b"release\n")?;
            stdin.flush()?;
            drop(stdin);
            self.wait_for_success()
        }

        fn wait_for_success(mut self) -> Result<(), Box<dyn std::error::Error>> {
            let status = self.wait_bounded(HELPER_TIMEOUT)?;
            let stderr = self.stderr.recv_timeout(HELPER_TIMEOUT).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("writer helper stderr did not close: {error}"),
                )
            })??;
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "writer helper failed with {status}: {}",
                    String::from_utf8_lossy(&stderr)
                ))
                .into());
            }
            Ok(())
        }

        fn wait_bounded(&mut self, timeout: Duration) -> std::io::Result<ExitStatus> {
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(status) = self.child.try_wait()? {
                    self.reaped = true;
                    self.terminate_group_and_reap(HELPER_CLEANUP_TIMEOUT)?;
                    return Ok(status);
                }
                if Instant::now() >= deadline {
                    self.terminate_group_and_reap(HELPER_CLEANUP_TIMEOUT)?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "writer helper exceeded {timeout:?}; its process group was terminated"
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn terminate_group_and_reap(&mut self, timeout: Duration) -> std::io::Result<()> {
            if self.group_gone && self.reaped {
                return Ok(());
            }
            self.stdin.take();
            let raw = i32::try_from(self.child.id())
                .ok()
                .and_then(Pid::from_raw)
                .ok_or_else(|| std::io::Error::other("writer helper PID is invalid"))?;

            // Address the whole group even if try_wait already observed and
            // reaped the leader; inherited descriptors can otherwise keep a
            // descendant alive after a seemingly successful test.
            signal_helper_group(raw, Signal::TERM)?;
            let term_deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < term_deadline {
                if !self.reaped && self.child.try_wait()?.is_some() {
                    self.reaped = true;
                }
                if helper_group_absent(raw, self.reaped)? {
                    self.group_gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            // The KILL call is intentionally unconditional. A leader can exit
            // during the TERM grace while descendants remain in its PGID.
            signal_helper_group(raw, Signal::KILL)?;
            let deadline = Instant::now() + timeout;
            loop {
                if !self.reaped && self.child.try_wait()?.is_some() {
                    self.reaped = true;
                }
                if helper_group_absent(raw, self.reaped)? {
                    self.group_gone = true;
                }
                if self.reaped && self.group_gone {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "writer helper leader or process group survived cleanup deadline",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn signal_helper_group(group: Pid, signal: Signal) -> std::io::Result<()> {
        match kill_process_group(group, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(std::io::Error::other(format!(
                "cannot signal writer helper process group: {error}"
            ))),
        }
    }

    fn helper_group_absent(group: Pid, _leader_reaped: bool) -> std::io::Result<bool> {
        match test_kill_process_group(group) {
            Err(rustix::io::Errno::SRCH) => Ok(true),
            Ok(()) => Ok(false),
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM)
                if _leader_reaped
                    && matches!(
                        rustix::process::getpgid(Some(group)),
                        Err(rustix::io::Errno::SRCH)
                    ) =>
            {
                Ok(true)
            }
            Err(rustix::io::Errno::PERM) => Ok(false),
            Err(error) => Err(std::io::Error::other(format!(
                "cannot probe writer helper process group: {error}"
            ))),
        }
    }

    fn create_helper_handshake(path: &Path) -> std::io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        validate_helper_handshake(&file)?;
        Ok(file)
    }

    fn validate_helper_handshake(file: &File) -> std::io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.len() > HELPER_HANDSHAKE_MAXIMUM
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "writer helper handshake is not a private bounded regular file",
            ));
        }
        Ok(())
    }

    fn read_helper_handshake(file: &File) -> std::io::Result<Option<String>> {
        validate_helper_handshake(file)?;
        if file.metadata()?.len() == 0 {
            return Ok(None);
        }
        let mut reader = file.try_clone()?;
        reader.rewind()?;
        let mut bytes = Vec::new();
        reader
            .take(HELPER_HANDSHAKE_MAXIMUM + 1)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty()
            || bytes.len() as u64 > HELPER_HANDSHAKE_MAXIMUM
            || !bytes.ends_with(b"\n")
            || bytes.iter().filter(|byte| **byte == b'\n').count() != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "writer helper handshake is incomplete or oversized",
            ));
        }
        let handshake: HelperHandshake = serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if handshake.schema_version != 1 || !matches!(handshake.state.as_str(), "ready" | "busy") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "writer helper handshake schema is invalid",
            ));
        }
        Ok(Some(handshake.state))
    }

    fn write_helper_handshake(path: &Path, state: &str) -> std::io::Result<()> {
        let payload: &[u8] = match state {
            "ready" => b"{\"schema_version\":1,\"state\":\"ready\"}\n",
            "busy" => b"{\"schema_version\":1,\"state\":\"busy\"}\n",
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "writer helper handshake state is invalid",
                ));
            }
        };
        let mut file = OpenOptions::new()
            .write(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)?;
        validate_helper_handshake(&file)?;
        file.set_len(0)?;
        file.write_all(payload)?;
        file.sync_all()?;
        validate_helper_handshake(&file)
    }

    // A surviving helper group must fail its owning test; Drop cannot return the proof error.
    #[expect(
        clippy::panic,
        reason = "test helper cleanup failure must fail the test"
    )]
    impl Drop for RustWriterHelper {
        fn drop(&mut self) {
            if self
                .terminate_group_and_reap(HELPER_CLEANUP_TIMEOUT)
                .is_err()
                && !std::thread::panicking()
            {
                panic!("writer helper process-group cleanup could not be proven");
            }
        }
    }

    fn write_legacy_pid(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    #[test]
    fn quiescence_inspection_is_read_only_for_an_empty_existing_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-empty")?;

        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;

        assert!(home.path.read_dir()?.next().is_none());
        assert_eq!(
            format!("{proof:?}"),
            "LegacyQuiescenceProof { home: \"REDACTED\" }"
        );
        drop(proof);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn quiescence_inspection_fails_closed_for_a_missing_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-missing")?;
        fs::remove_dir(&home.path)?;

        assert!(matches!(
            LegacyQuiescenceProof::inspect(&home.authorized),
            Err(WriterAuthorityError::Home(_))
        ));
        assert!(!home.path.exists());

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn quiescence_inspection_preserves_regular_non_workspace_entries()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-benign-workspace-entry")?;
        let workspaces = home.path.join(LEGACY_WORKSPACES_DIRECTORY);
        fs::create_dir(&workspaces)?;
        fs::set_permissions(&workspaces, fs::Permissions::from_mode(0o700))?;
        let ignored = workspaces.join(".DS_Store");
        fs::write(&ignored, b"preserve me")?;
        let original = fs::read(&ignored)?;

        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;
        assert_eq!(fs::read(&ignored)?, original);
        let authority = ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof)?;
        assert_eq!(fs::read(&ignored)?, original);

        drop(authority);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn quiescence_inspection_accepts_and_preserves_a_dead_daemon_pid()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-dead")?;
        let raw_pid = i32::MAX;
        let pid = Pid::from_raw(raw_pid).ok_or("dead fixture PID was invalid")?;
        assert!(matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        ));
        let bytes = format!("{raw_pid}\n").into_bytes();
        let pid_path = home.path.join(LEGACY_DAEMON_PID_FILE);
        write_legacy_pid(&pid_path, &bytes)?;

        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;
        assert_eq!(fs::read(&pid_path)?, bytes);
        let authority = ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof)?;
        assert_eq!(fs::read(&pid_path)?, bytes);

        drop(authority);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn quiescence_inspection_rejects_a_live_daemon_pid() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-live")?;
        write_legacy_pid(
            &home.path.join(LEGACY_DAEMON_PID_FILE),
            format!("{}\n", std::process::id()).as_bytes(),
        )?;

        assert!(matches!(
            LegacyQuiescenceProof::inspect(&home.authorized),
            Err(WriterAuthorityError::Busy)
        ));
        assert!(!home.path.join(WRITER_LOCK_FILE).exists());

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn legacy_pid_probe_classifier_is_conservative_and_exhaustive() {
        assert!(matches!(
            classify_legacy_pid_probe(Ok(())),
            Err(WriterAuthorityError::Busy)
        ));
        assert!(matches!(
            classify_legacy_pid_probe(Err(rustix::io::Errno::PERM)),
            Err(WriterAuthorityError::Busy)
        ));
        assert!(classify_legacy_pid_probe(Err(rustix::io::Errno::SRCH)).is_ok());
        assert!(matches!(
            classify_legacy_pid_probe(Err(rustix::io::Errno::INVAL)),
            Err(WriterAuthorityError::LegacyStateChanged)
        ));
    }

    #[test]
    fn quiescence_inspection_rejects_malformed_pid_content()
    -> Result<(), Box<dyn std::error::Error>> {
        for (label, bytes) in [
            ("empty", b"".as_slice()),
            ("zero", b"0".as_slice()),
            ("negative", b"-1".as_slice()),
            ("text", b"not-a-pid".as_slice()),
            ("overflow", b"2147483648".as_slice()),
            ("non-utf8", [0xff].as_slice()),
        ] {
            let home = TestHome::create(label)?;
            write_legacy_pid(&home.path.join(LEGACY_DAEMON_PID_FILE), bytes)?;
            assert!(matches!(
                LegacyQuiescenceProof::inspect(&home.authorized),
                Err(WriterAuthorityError::InvalidLegacyPid)
            ));
            home.cleanup()?;
        }

        let oversized = TestHome::create("oversized")?;
        write_legacy_pid(
            &oversized.path.join(LEGACY_DAEMON_PID_FILE),
            b"111111111111111111111111111111111",
        )?;
        assert!(matches!(
            LegacyQuiescenceProof::inspect(&oversized.authorized),
            Err(WriterAuthorityError::UnsafeLegacyState)
        ));
        oversized.cleanup()?;
        Ok(())
    }

    #[test]
    fn quiescence_inspection_rejects_symlinked_and_hardlinked_pid_files()
    -> Result<(), Box<dyn std::error::Error>> {
        for hardlink in [false, true] {
            let home = TestHome::create(if hardlink {
                "quiescence-hardlink"
            } else {
                "quiescence-symlink"
            })?;
            let outside = home.parent.join("outside-pid");
            write_legacy_pid(&outside, b"2147483647\n")?;
            if hardlink {
                fs::hard_link(&outside, home.path.join(LEGACY_DAEMON_PID_FILE))?;
            } else {
                std::os::unix::fs::symlink(&outside, home.path.join(LEGACY_DAEMON_PID_FILE))?;
            }

            assert!(matches!(
                LegacyQuiescenceProof::inspect(&home.authorized),
                Err(WriterAuthorityError::UnsafeLegacyState)
            ));
            home.cleanup()?;
        }
        Ok(())
    }

    #[test]
    fn acquisition_rejects_a_root_swap_after_quiescence_inspection()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-root-swap")?;
        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;
        let displaced = home.parent.join("displaced-runtime");
        fs::rename(&home.path, &displaced)?;
        fs::create_dir(&home.path)?;
        fs::set_permissions(&home.path, fs::Permissions::from_mode(0o700))?;

        let acquisition = ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof);
        assert!(
            matches!(acquisition, Err(WriterAuthorityError::LegacyStateChanged)),
            "unexpected root-swap result: {acquisition:?}"
        );
        assert!(!home.path.join(WRITER_LOCK_FILE).exists());
        assert!(!displaced.join(WRITER_LOCK_FILE).exists());

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn acquisition_rejects_a_new_live_workspace_pid_after_inspection()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("quiescence-new-workspace-pid")?;
        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;
        let workspace = home
            .path
            .join(LEGACY_WORKSPACES_DIRECTORY)
            .join("mission-one");
        fs::create_dir_all(&workspace)?;
        fs::set_permissions(
            home.path.join(LEGACY_WORKSPACES_DIRECTORY),
            fs::Permissions::from_mode(0o700),
        )?;
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
        write_legacy_pid(
            &workspace.join(LEGACY_WORKSPACE_PID_FILE),
            std::process::id().to_string().as_bytes(),
        )?;

        assert!(matches!(
            ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof),
            Err(WriterAuthorityError::Busy)
        ));
        assert!(!home.path.join(WRITER_LOCK_FILE).exists());

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn diagnostics_match_shared_canonical_wire_goldens() -> Result<(), Box<dyn std::error::Error>> {
        let goldens: LockRecordGoldenSet = serde_json::from_str(SHARED_LOCK_RECORD_GOLDENS)?;
        assert_eq!(goldens.fixture_schema_version, 1);
        for golden in goldens.cases {
            let record = WriterLockRecord {
                schema_version: golden.schema_version,
                implementation: &golden.implementation,
                binary_version: &golden.binary_version,
                pid: golden.pid,
                process_start_identity: &golden.process_start_identity,
                acquired_at_unix_ms: golden.acquired_at_unix_ms,
            };
            let actual = encode_lock_record(&record)?;
            assert_eq!(
                actual,
                golden.canonical.as_bytes(),
                "shared canonical lock-record case {} differed",
                golden.name
            );
        }
        Ok(())
    }

    #[test]
    fn acquisition_seals_home_and_writes_bounded_diagnostics()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("diagnostics")?;
        let authority = home.acquire()?;
        let boundary = authority.boundary();

        boundary.verify()?;
        let bytes = fs::read(home.path.join(WRITER_LOCK_FILE))?;
        let record: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["implementation"], "rust");
        assert_eq!(record["binary_version"], "0.1.0-test");
        assert_eq!(record["pid"], std::process::id());
        assert!(
            record["process_start_identity"]
                .as_str()
                .is_some_and(|value| value.starts_with("darwin:") || value.starts_with("linux:"))
        );
        assert!(
            record["acquired_at_unix_ms"]
                .as_u64()
                .is_some_and(|value| value > 0)
        );
        let root_metadata = fs::metadata(&home.path)?;
        let lock_metadata = fs::metadata(home.path.join(WRITER_LOCK_FILE))?;
        assert_eq!(root_metadata.mode() & 0o777, 0o700);
        assert_eq!(lock_metadata.mode() & 0o777, 0o600);
        assert_eq!(lock_metadata.nlink(), 1);

        drop(boundary);
        drop(authority);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn boundary_retains_lease_and_detects_lock_path_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let home = TestHome::create("replace")?;
        let authority = home.acquire()?;
        let boundary = authority.boundary();
        drop(authority);

        fs::rename(
            home.path.join(WRITER_LOCK_FILE),
            home.path.join("displaced-writer.lock"),
        )?;
        fs::write(home.path.join(WRITER_LOCK_FILE), b"replacement\n")?;
        fs::set_permissions(
            home.path.join(WRITER_LOCK_FILE),
            fs::Permissions::from_mode(0o600),
        )?;

        assert!(boundary.verify().is_err());
        drop(boundary);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn retained_writer_authority_creates_and_reserves_production_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("production-workspace")?;
        let authority = home.acquire()?;
        let boundary = authority.boundary();
        let mission = MissionId::new("production-workspace-mission")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission.to_string(),
            status: "pending".to_owned(),
            ..CheckpointProjection::default()
        };
        let workspace = WorkspaceAuthority::create_production(
            Arc::clone(&boundary),
            mission.clone(),
            WorkspaceSeed::new(
                b"production canary mission\n".to_vec(),
                &checkpoint,
                b"{}".to_vec(),
            )?,
        )?;
        let reservation = workspace.into_production_projection_writer()?;
        assert_eq!(reservation.mission_id(), &mission);
        assert!(boundary.verify().is_ok());

        drop(reservation);
        drop(boundary);
        drop(authority);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn acquisition_rejects_a_hardlinked_lock_entry() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let home = TestHome::create("hardlink")?;
        fs::set_permissions(&home.path, fs::Permissions::from_mode(0o700))?;
        let source = home.parent.join("outside-lock");
        fs::write(&source, b"outside\n")?;
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600))?;
        fs::hard_link(&source, home.path.join(WRITER_LOCK_FILE))?;

        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;
        assert!(matches!(
            ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof),
            Err(WriterAuthorityError::UnsafeLock)
        ));
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn acquisition_rejects_special_bits_on_an_existing_lock()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let home = TestHome::create("special-lock-mode")?;
        fs::set_permissions(&home.path, fs::Permissions::from_mode(0o700))?;
        let lock_path = home.path.join(WRITER_LOCK_FILE);
        fs::write(&lock_path, b"preserve\n")?;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o4600))?;
        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;

        assert!(matches!(
            ProductionWriterAuthority::acquire(&home.authorized, "0.1.0-test", proof),
            Err(WriterAuthorityError::UnsafeLock)
        ));
        assert_eq!(fs::read(&lock_path)?, b"preserve\n");

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn metadata_rejects_control_characters_and_unbounded_values() {
        assert!(ProcessStartIdentity::new("line\nbreak").is_err());
        assert!(ProcessStartIdentity::new("x".repeat(MAX_START_IDENTITY_BYTES + 1)).is_err());
        let identity = ProcessStartIdentity::new("fixture:one");
        assert!(matches!(
            identity.and_then(|identity| WriterLeaseMetadata::new(
                "x".repeat(MAX_VERSION_BYTES + 1),
                identity,
                UNIX_EPOCH,
            )),
            Err(WriterAuthorityError::InvalidMetadata)
        ));
    }

    #[test]
    fn acquisition_rejects_a_quiescence_proof_for_another_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = TestHome::create("proof-first")?;
        let second = TestHome::create("proof-second")?;
        let proof = LegacyQuiescenceProof::inspect(&first.authorized)?;

        assert!(matches!(
            ProductionWriterAuthority::acquire(&second.authorized, "0.1.0-test", proof),
            Err(WriterAuthorityError::QuiescenceMismatch)
        ));
        assert!(!second.path.join(WRITER_LOCK_FILE).exists());

        first.cleanup()?;
        second.cleanup()?;
        Ok(())
    }

    #[test]
    fn acquisition_rejects_invalid_version_before_provisioning()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("bad-version")?;
        let proof = LegacyQuiescenceProof::inspect(&home.authorized)?;

        assert!(matches!(
            ProductionWriterAuthority::acquire(&home.authorized, "line\nbreak", proof),
            Err(WriterAuthorityError::InvalidMetadata)
        ));
        assert!(!home.path.join(WRITER_LOCK_FILE).exists());

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn boundary_rejects_post_acquisition_root_mode_tampering()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let home = TestHome::create("root-mode-tamper")?;
        let authority = home.acquire()?;
        let boundary = authority.boundary();

        fs::set_permissions(&home.path, fs::Permissions::from_mode(0o777))?;
        assert!(boundary.verify().is_err());

        fs::set_permissions(&home.path, fs::Permissions::from_mode(0o700))?;
        drop(boundary);
        drop(authority);
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn writer_lease_contends_across_processes() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("cross-process")?;
        let mut helper = RustWriterHelper::spawn(&home, "hold")?;
        helper.expect_state("ready")?;

        let contention = home.acquire();
        assert!(matches!(contention, Err(WriterAuthorityError::Busy)));
        helper.release_and_wait()?;

        let sequential = home.acquire()?;
        drop(sequential);

        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn private_pilot_process_rejects_a_nonpilot_production_writer_before_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::create("nonpilot-process-origin")?;
        let writer = home.acquire()?;
        let cwd = home.path.join("private-cwd");
        fs::create_dir(&cwd)?;
        fs::set_permissions(&cwd, fs::Permissions::from_mode(0o700))?;
        let executable_path = home.parent.join("external-target.sh");
        let mut executable_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&executable_path)?;
        executable_file.write_all(b"#!/bin/sh\nexit 99\n")?;
        executable_file.sync_all()?;
        drop(executable_file);
        let executable = RustPilotProcessExecutable::open(&fs::canonicalize(executable_path)?)?;
        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            executable.logical_id(),
            fs::canonicalize(&cwd)?,
        )?;
        assert!(matches!(
            RustPilotProcessSession::open(
                &writer,
                RustPilotProcessAttempt::new(
                    MissionId::new("wrong-writer-origin")?,
                    "provider",
                    1,
                )?,
                &request,
                executable,
                PathBuf::from("private-cwd"),
            ),
            Err(RustPilotProcessError::WrongWriterOrigin)
        ));
        home.cleanup()?;
        Ok(())
    }

    #[test]
    fn writer_lock_helper_process() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("NANIKA_RUST_WRITER_HELPER").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return Ok(());
        }
        let path = PathBuf::from(
            std::env::var_os("NANIKA_RUST_WRITER_HOME")
                .ok_or("writer helper home was unavailable")?,
        );
        let parent = PathBuf::from(
            std::env::var_os("NANIKA_RUST_WRITER_PARENT")
                .ok_or("writer helper parent was unavailable")?,
        );
        let mode = std::env::var("NANIKA_RUST_WRITER_MODE")?;
        let handshake = PathBuf::from(
            std::env::var_os("NANIKA_RUST_WRITER_HANDSHAKE")
                .ok_or("writer helper handshake path was unavailable")?,
        );
        let mut inputs = HomeInputs::from_user_home(parent.join("helper-user"));
        inputs.orchestrator_config_dir = Some(path);
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("1"))
            .ok_or("writer helper enrollment was unavailable")?;
        let authorized = resolved.authorize_production(&enrollment)?;
        let proof = LegacyQuiescenceProof::inspect(&authorized)?;
        let acquisition = ProductionWriterAuthority::acquire(&authorized, "helper-test", proof);
        match mode.as_str() {
            "hold" => {
                let authority = acquisition?;
                write_helper_handshake(&handshake, "ready")?;

                let mut release = String::new();
                BufReader::new(std::io::stdin()).read_line(&mut release)?;
                if release != "release\n" {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "writer helper received an invalid release handshake",
                    )
                    .into());
                }
                drop(authority);
                Ok(())
            }
            "expect-busy" => match acquisition {
                Err(WriterAuthorityError::Busy) => {
                    write_helper_handshake(&handshake, "busy")?;
                    Ok(())
                }
                Ok(authority) => {
                    drop(authority);
                    Err(std::io::Error::other(
                        "writer helper unexpectedly acquired a contended lease",
                    )
                    .into())
                }
                Err(error) => Err(error.into()),
            },
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "writer helper mode is invalid",
            )
            .into()),
        }
    }
}
