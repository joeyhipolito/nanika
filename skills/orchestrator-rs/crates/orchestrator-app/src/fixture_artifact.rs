//! Exact, fixture-only output artifact publication and attestation.
//!
//! An authority binds one normalized artifact name and its exact expected bytes
//! while a [`WorkspaceAuthority`] is still available. The workspace may then be
//! consumed by another authority. Publication remains bounded and no-clobber,
//! and attestation independently reopens the published file without trusting a
//! provider-supplied path, size, or digest.
//!
//! Publication consumes the only writable authority, so an attestor cannot
//! coexist with a reusable artifact publisher:
//!
//! ```compile_fail,E0382
//! use orchestrator_app::{FixtureArtifactAuthority, FixtureArtifactError};
//!
//! fn cannot_reuse_writer(
//!     authority: FixtureArtifactAuthority,
//! ) -> Result<(), FixtureArtifactError> {
//!     let _attestor = authority.publish()?;
//!     let _second = authority.publish()?;
//!     Ok(())
//! }
//! ```
//!
//! The resulting authority is read-only:
//!
//! ```compile_fail,E0599
//! use orchestrator_app::FixtureArtifactAttestor;
//!
//! fn attestor_cannot_publish(attestor: FixtureArtifactAttestor) {
//!     let _ = attestor.publish();
//! }
//! ```

use crate::{
    capability::SharedCapabilityRoot,
    fs_util::{
        FileIdentity, create_dir_private, create_private_file, identity, mode,
        open_dir_path_nofollow, open_file_nofollow, sync_dir,
    },
    workspace::{WorkspaceAuthority, WorkspaceError},
};
use cap_std::fs::Dir;
use orchestrator_core::{MissionId, PhaseId};
use std::{
    error::Error,
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const MAX_FIXTURE_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARTIFACT_NAME_BYTES: usize = 255;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const READ_BUFFER_BYTES: usize = 8 * 1024;
const FIXTURE_FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FIXTURE_FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

static STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Stable classification for a redacted fixture-artifact failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureArtifactErrorKind {
    /// Attempt zero is not a valid execution-attempt identity.
    InvalidAttempt,
    /// The requested artifact label is not one safe filename component.
    InvalidName,
    /// The exact expected content exceeds the fixture-only size bound.
    TooLarge,
    /// The workspace does not carry fixture authority.
    FixtureOnly,
    /// An expected namespace or file identity changed.
    IdentityChanged,
    /// A required file was absent.
    Missing,
    /// A concurrent entry appeared where publication required absence.
    Conflict,
    /// An entry had the wrong type or private mode.
    InvalidEntry,
    /// Independently observed bytes differed from the bound expectation.
    ContentMismatch,
    /// A capability-bounded filesystem operation failed.
    Filesystem,
}

/// Redacted failure from exact fixture-artifact handling.
///
/// Neither display nor debug formatting includes filesystem paths, artifact
/// names, expected bytes, or digests.
pub struct FixtureArtifactError {
    kind: FixtureArtifactErrorKind,
    operation: &'static str,
    os_kind: Option<std::io::ErrorKind>,
}

impl FixtureArtifactError {
    fn new(kind: FixtureArtifactErrorKind, operation: &'static str) -> Self {
        Self {
            kind,
            operation,
            os_kind: None,
        }
    }

    fn filesystem(operation: &'static str, source: &std::io::Error) -> Self {
        Self {
            kind: FixtureArtifactErrorKind::Filesystem,
            operation,
            os_kind: Some(source.kind()),
        }
    }

    /// Returns the stable, non-sensitive failure classification.
    #[must_use]
    pub const fn kind(&self) -> FixtureArtifactErrorKind {
        self.kind
    }
}

impl fmt::Debug for FixtureArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureArtifactError")
            .field("kind", &self.kind)
            .field("operation", &self.operation)
            .field("os_kind", &self.os_kind)
            .finish()
    }
}

impl fmt::Display for FixtureArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fixture artifact {} failed ({:?})",
            self.operation, self.kind
        )
    }
}

impl Error for FixtureArtifactError {}

struct ArtifactBinding {
    boundary: SharedCapabilityRoot,
    workspace_directory: Dir,
    workspace_relative: PathBuf,
    workspace_identity: FileIdentity,
    phase_directory: Dir,
    phase_relative: PathBuf,
    phase_identity: FileIdentity,
    artifact_directory: Dir,
    artifact_relative: PathBuf,
    artifact_identity: FileIdentity,
    mission_id: MissionId,
    phase_id: PhaseId,
    attempt: u32,
    file_name: String,
    receipt_path: PathBuf,
    expected: Arc<[u8]>,
    digest: String,
}

impl ArtifactBinding {
    fn verify_namespace(&self) -> Result<(), FixtureArtifactError> {
        self.boundary.verify().map_err(|_| {
            FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify fixture root",
            )
        })?;

        let held_workspace = self.workspace_directory.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect held workspace", &source)
        })?;
        if !held_workspace.is_dir()
            || mode(&held_workspace) != PRIVATE_DIRECTORY_MODE
            || identity(&held_workspace) != self.workspace_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify held workspace",
            ));
        }
        let current_workspace =
            open_dir_path_nofollow(self.boundary.directory(), &self.workspace_relative).map_err(
                |_| {
                    FixtureArtifactError::new(
                        FixtureArtifactErrorKind::IdentityChanged,
                        "reopen workspace",
                    )
                },
            )?;
        let current_workspace = current_workspace.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect reopened workspace", &source)
        })?;
        if !current_workspace.is_dir()
            || mode(&current_workspace) != PRIVATE_DIRECTORY_MODE
            || identity(&current_workspace) != self.workspace_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify reopened workspace",
            ));
        }

        let held_phase = self.phase_directory.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect held phase artifact directory", &source)
        })?;
        if !held_phase.is_dir()
            || mode(&held_phase) != PRIVATE_DIRECTORY_MODE
            || identity(&held_phase) != self.phase_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify held phase artifact directory",
            ));
        }
        let current_phase = open_dir_path_nofollow(self.boundary.directory(), &self.phase_relative)
            .map_err(|_| {
                FixtureArtifactError::new(
                    FixtureArtifactErrorKind::IdentityChanged,
                    "reopen phase artifact directory",
                )
            })?;
        let current_phase = current_phase.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect reopened phase artifact directory", &source)
        })?;
        if !current_phase.is_dir()
            || mode(&current_phase) != PRIVATE_DIRECTORY_MODE
            || identity(&current_phase) != self.phase_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify reopened phase artifact directory",
            ));
        }

        let held_artifact = self.artifact_directory.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect held artifact directory", &source)
        })?;
        if !held_artifact.is_dir()
            || mode(&held_artifact) != PRIVATE_DIRECTORY_MODE
            || identity(&held_artifact) != self.artifact_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify held artifact directory",
            ));
        }
        let current_artifact =
            open_dir_path_nofollow(self.boundary.directory(), &self.artifact_relative).map_err(
                |_| {
                    FixtureArtifactError::new(
                        FixtureArtifactErrorKind::IdentityChanged,
                        "reopen artifact directory",
                    )
                },
            )?;
        let current_artifact = current_artifact.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect reopened artifact directory", &source)
        })?;
        if !current_artifact.is_dir()
            || mode(&current_artifact) != PRIVATE_DIRECTORY_MODE
            || identity(&current_artifact) != self.artifact_identity
        {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "verify reopened artifact directory",
            ));
        }
        Ok(())
    }

    fn receipt(&self, file_identity: FileIdentity) -> FixtureArtifactReceipt {
        FixtureArtifactReceipt {
            mission_id: self.mission_id.clone(),
            phase_id: self.phase_id.clone(),
            attempt: self.attempt,
            relative_path: self.receipt_path.clone(),
            file_identity: FixtureArtifactFileIdentity(file_identity),
            digest: self.digest.clone(),
            bytes: self.expected.len() as u64,
        }
    }
}

#[derive(Clone, Copy)]
enum PublicationState {
    Absent,
    Existing(FileIdentity),
    Published(FileIdentity),
}

/// Fixture-only authority to publish one exact artifact without clobbering.
///
/// The expected bytes and all namespace identities are bound during
/// [`WorkspaceAuthority::bind_fixture_artifact`]. `publish` accepts no bytes or
/// path, so later execution cannot redirect or redefine the admitted artifact.
/// This type has no production constructor or enrollment path.
pub struct FixtureArtifactAuthority {
    binding: Arc<ArtifactBinding>,
    state: PublicationState,
}

impl fmt::Debug for FixtureArtifactAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureArtifactAuthority")
            .field("kind", &"exact-fixture-artifact-publisher")
            .finish()
    }
}

impl FixtureArtifactAuthority {
    pub(crate) fn accepts_input(&self, bytes: &[u8]) -> bool {
        bytes == self.binding.expected.as_ref()
    }

    pub(crate) fn expected_input(&self) -> Arc<[u8]> {
        Arc::clone(&self.binding.expected)
    }

    pub(crate) fn mission_id(&self) -> &MissionId {
        &self.binding.mission_id
    }

    pub(crate) fn phase_id(&self) -> &PhaseId {
        &self.binding.phase_id
    }

    pub(crate) fn attempt(&self) -> u32 {
        self.binding.attempt
    }

    pub(crate) fn resource_path(&self) -> &Path {
        &self.binding.receipt_path
    }

    pub(crate) fn workspace_path(&self) -> PathBuf {
        self.binding
            .boundary
            .canonical_path()
            .join(&self.binding.workspace_relative)
    }

    pub(crate) fn publication_exists(&self) -> bool {
        matches!(
            self.state,
            PublicationState::Existing(_) | PublicationState::Published(_)
        )
    }

    /// Atomically publishes the bound bytes or re-admits the exact file that
    /// was already present when this authority was minted.
    ///
    /// Publication stages a mode-0600 file and adds the final name with an
    /// atomic, no-clobber hard link. A competing file, directory, or symlink is
    /// rejected. The returned attestor pins the published file identity and
    /// can only perform bounded, exact reads.
    pub fn publish(self) -> Result<FixtureArtifactAttestor, FixtureArtifactError> {
        self.publish_recoverable()
            .map_err(|(_authority, error)| error)
    }

    pub(crate) fn publish_recoverable(
        mut self,
    ) -> Result<FixtureArtifactAttestor, (Self, FixtureArtifactError)> {
        if let Err(error) = self.binding.verify_namespace() {
            return Err((self, error));
        }
        let file_identity = match self.state {
            PublicationState::Existing(expected) | PublicationState::Published(expected) => {
                let observed = match observe_exact_file(
                    &self.binding.artifact_directory,
                    Path::new(&self.binding.file_name),
                    &self.binding.expected,
                    Some(expected),
                ) {
                    Ok(observed) => observed,
                    Err(error) => return Err((self, error)),
                };
                if let Err(source) = sync_dir(&self.binding.artifact_directory) {
                    let error = FixtureArtifactError::filesystem("sync admitted artifact", &source);
                    return Err((self, error));
                }
                observed
            }
            PublicationState::Absent => match self.publish_absent() {
                Ok(identity) => identity,
                Err(error) => return Err((self, error)),
            },
        };
        if let Err(error) = self.binding.verify_namespace() {
            return Err((self, error));
        }
        Ok(FixtureArtifactAttestor {
            binding: Arc::clone(&self.binding),
            file_identity,
        })
    }

    fn publish_absent(&mut self) -> Result<FileIdentity, FixtureArtifactError> {
        ensure_absent(
            &self.binding.artifact_directory,
            Path::new(&self.binding.file_name),
        )?;
        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let stage_name = format!(".fixture-artifact-stage-{}-{nonce}", std::process::id());
        let stage = Path::new(&stage_name);
        create_private_file(
            &self.binding.artifact_directory,
            stage,
            &self.binding.expected,
            false,
        )
        .map_err(|source| FixtureArtifactError::filesystem("stage exact artifact", &source))?;

        let staged_identity = match observe_exact_file(
            &self.binding.artifact_directory,
            stage,
            &self.binding.expected,
            None,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                cleanup_stage(&self.binding.artifact_directory, stage);
                return Err(error);
            }
        };
        let final_name = Path::new(&self.binding.file_name);
        if let Err(source) = self.binding.artifact_directory.hard_link(
            stage,
            &self.binding.artifact_directory,
            final_name,
        ) {
            cleanup_stage(&self.binding.artifact_directory, stage);
            return if source.kind() == std::io::ErrorKind::AlreadyExists {
                Err(FixtureArtifactError::new(
                    FixtureArtifactErrorKind::Conflict,
                    "publish no-clobber artifact",
                ))
            } else {
                Err(FixtureArtifactError::filesystem(
                    "publish no-clobber artifact",
                    &source,
                ))
            };
        }

        // The final directory entry now names the staged inode. Retain that
        // identity even if a later durability step fails so a retry can verify
        // the exact publication without rewriting it.
        self.state = PublicationState::Published(staged_identity);
        if let Err(source) = self.binding.artifact_directory.remove_file(stage) {
            let _ = sync_dir(&self.binding.artifact_directory);
            return Err(FixtureArtifactError::filesystem(
                "remove artifact staging link",
                &source,
            ));
        }
        sync_dir(&self.binding.artifact_directory).map_err(|source| {
            FixtureArtifactError::filesystem("sync published artifact", &source)
        })?;
        observe_exact_file(
            &self.binding.artifact_directory,
            final_name,
            &self.binding.expected,
            Some(staged_identity),
        )
    }
}

/// Read-only authority for one already-published exact fixture artifact.
pub struct FixtureArtifactAttestor {
    binding: Arc<ArtifactBinding>,
    file_identity: FileIdentity,
}

impl fmt::Debug for FixtureArtifactAttestor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureArtifactAttestor")
            .field("kind", &"exact-fixture-artifact-attestor")
            .finish()
    }
}

impl FixtureArtifactAttestor {
    /// Independently reopens and exactly reads the admitted file before
    /// returning a receipt. Provider-supplied receipt fields are not inputs.
    pub fn attest(&self) -> Result<FixtureArtifactReceipt, FixtureArtifactError> {
        self.binding.verify_namespace()?;
        observe_exact_file(
            &self.binding.artifact_directory,
            Path::new(&self.binding.file_name),
            &self.binding.expected,
            Some(self.file_identity),
        )?;
        self.binding.verify_namespace()?;
        Ok(self.binding.receipt(self.file_identity))
    }
}

/// Opaque identity of the exact file independently observed by an attestor.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FixtureArtifactFileIdentity(FileIdentity);

impl fmt::Debug for FixtureArtifactFileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FixtureArtifactFileIdentity([REDACTED])")
    }
}

/// Receipt produced only after exact fixture-artifact attestation.
///
/// Its `fixture-fnv1a64-v1` digest is deterministic for fixture comparisons,
/// but is deliberately not a cryptographic integrity primitive. Exact bounded
/// byte comparison and pinned filesystem identity are the authority proof.
#[derive(Clone, Eq, PartialEq)]
pub struct FixtureArtifactReceipt {
    mission_id: MissionId,
    phase_id: PhaseId,
    attempt: u32,
    relative_path: PathBuf,
    file_identity: FixtureArtifactFileIdentity,
    digest: String,
    bytes: u64,
}

impl FixtureArtifactReceipt {
    /// Returns the mission identity bound before workspace consumption.
    #[must_use]
    pub const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// Returns the phase identity bound before workspace consumption.
    #[must_use]
    pub const fn phase_id(&self) -> &PhaseId {
        &self.phase_id
    }

    /// Returns the execution attempt bound before workspace consumption.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the normalized path relative to the workspace.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the opaque identity of the exact attested file.
    #[must_use]
    pub const fn file_identity(&self) -> FixtureArtifactFileIdentity {
        self.file_identity
    }

    /// Returns the deterministic fixture-only digest label.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns the exact observed byte length.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl fmt::Debug for FixtureArtifactReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureArtifactReceipt")
            .field("kind", &"attested-fixture-artifact")
            .finish()
    }
}

impl WorkspaceAuthority {
    /// Binds one phase-attempt artifact and its exact expected bytes before
    /// this workspace is consumed by another authority.
    ///
    /// `attempt` must be non-zero and `name` must be one non-hidden ASCII
    /// filename component. Existing files are admitted only when their private
    /// mode and exact bytes already match; their identity is then pinned.
    /// Production-backed workspaces are always rejected.
    pub fn bind_fixture_artifact(
        &self,
        phase: &PhaseId,
        attempt: u32,
        name: &str,
        expected: &[u8],
    ) -> Result<FixtureArtifactAuthority, FixtureArtifactError> {
        if attempt == 0 {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::InvalidAttempt,
                "bind execution attempt",
            ));
        }
        validate_artifact_name(name)?;
        if expected.len() > MAX_FIXTURE_ARTIFACT_BYTES {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::TooLarge,
                "bind expected bytes",
            ));
        }
        let boundary = self
            .verified_fixture_boundary()
            .map_err(|error| map_workspace_error("bind fixture authority", error))?;
        let (workspace_directory, workspace_identity, workspace_relative) = self
            .process_cwd_binding()
            .map_err(|error| map_workspace_error("bind workspace", error))?;
        let artifacts = open_dir_path_nofollow(&workspace_directory, Path::new("artifacts"))
            .map_err(|source| {
                FixtureArtifactError::filesystem("open artifacts directory", &source)
            })?;
        let phase_name = Path::new(phase.as_str());
        match create_dir_private(&artifacts, phase_name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(FixtureArtifactError::filesystem(
                    "create phase artifact directory",
                    &source,
                ));
            }
        }
        let phase_directory = open_dir_path_nofollow(&artifacts, phase_name).map_err(|_| {
            FixtureArtifactError::new(
                FixtureArtifactErrorKind::InvalidEntry,
                "open phase artifact directory",
            )
        })?;
        let phase_metadata = phase_directory.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect phase artifact directory", &source)
        })?;
        if !phase_metadata.is_dir() || mode(&phase_metadata) != PRIVATE_DIRECTORY_MODE {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::InvalidEntry,
                "validate phase artifact directory",
            ));
        }
        sync_dir(&artifacts).map_err(|source| {
            FixtureArtifactError::filesystem("sync phase artifact directory", &source)
        })?;

        let attempt_name = format!("attempt-{attempt}");
        let attempt_name = Path::new(&attempt_name);
        match create_dir_private(&phase_directory, attempt_name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(FixtureArtifactError::filesystem(
                    "create attempt artifact directory",
                    &source,
                ));
            }
        }
        let artifact_directory =
            open_dir_path_nofollow(&phase_directory, attempt_name).map_err(|_| {
                FixtureArtifactError::new(
                    FixtureArtifactErrorKind::InvalidEntry,
                    "open attempt artifact directory",
                )
            })?;
        let artifact_metadata = artifact_directory.dir_metadata().map_err(|source| {
            FixtureArtifactError::filesystem("inspect attempt artifact directory", &source)
        })?;
        if !artifact_metadata.is_dir() || mode(&artifact_metadata) != PRIVATE_DIRECTORY_MODE {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::InvalidEntry,
                "validate attempt artifact directory",
            ));
        }
        sync_dir(&phase_directory).map_err(|source| {
            FixtureArtifactError::filesystem("sync attempt artifact directory", &source)
        })?;

        let phase_relative = workspace_relative.join("artifacts").join(phase.as_str());
        let artifact_relative = phase_relative.join(attempt_name);
        let binding = Arc::new(ArtifactBinding {
            boundary: Arc::clone(boundary),
            workspace_directory,
            workspace_relative,
            workspace_identity,
            phase_identity: identity(&phase_metadata),
            phase_directory,
            phase_relative,
            artifact_identity: identity(&artifact_metadata),
            mission_id: self.mission_id().clone(),
            phase_id: phase.clone(),
            attempt,
            artifact_directory,
            artifact_relative,
            file_name: name.to_owned(),
            receipt_path: Path::new("artifacts")
                .join(phase.as_str())
                .join(attempt_name)
                .join(name),
            expected: Arc::from(expected),
            digest: fixture_digest(expected),
        });
        binding.verify_namespace()?;

        let final_name = Path::new(name);
        let state = match binding.artifact_directory.symlink_metadata(final_name) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || mode(&metadata) != PRIVATE_FILE_MODE
                {
                    return Err(FixtureArtifactError::new(
                        FixtureArtifactErrorKind::InvalidEntry,
                        "admit existing artifact",
                    ));
                }
                PublicationState::Existing(observe_exact_file(
                    &binding.artifact_directory,
                    final_name,
                    &binding.expected,
                    Some(identity(&metadata)),
                )?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PublicationState::Absent,
            Err(source) => {
                return Err(FixtureArtifactError::filesystem(
                    "inspect artifact publication name",
                    &source,
                ));
            }
        };
        binding.verify_namespace()?;
        Ok(FixtureArtifactAuthority { binding, state })
    }
}

fn validate_artifact_name(name: &str) -> Result<(), FixtureArtifactError> {
    let path = Path::new(name);
    let mut components = path.components();
    let one_normal_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    let valid_bytes = !name.is_empty()
        && name.len() <= MAX_ARTIFACT_NAME_BYTES
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if one_normal_component && valid_bytes {
        Ok(())
    } else {
        Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::InvalidName,
            "validate artifact name",
        ))
    }
}

fn ensure_absent(directory: &Dir, name: &Path) -> Result<(), FixtureArtifactError> {
    match directory.symlink_metadata(name) {
        Ok(_) => Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::Conflict,
            "verify absent publication name",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(FixtureArtifactError::filesystem(
            "inspect publication name",
            &source,
        )),
    }
}

fn observe_exact_file(
    directory: &Dir,
    name: &Path,
    expected: &[u8],
    expected_identity: Option<FileIdentity>,
) -> Result<FileIdentity, FixtureArtifactError> {
    let mut file = open_file_nofollow(directory, name).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            FixtureArtifactError::new(FixtureArtifactErrorKind::Missing, "open exact artifact")
        } else if expected_identity.is_some() {
            FixtureArtifactError::new(
                FixtureArtifactErrorKind::IdentityChanged,
                "open exact artifact",
            )
        } else {
            FixtureArtifactError::filesystem("open exact artifact", &source)
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| FixtureArtifactError::filesystem("inspect exact artifact", &source))?;
    if !metadata.is_file() || mode(&metadata) != PRIVATE_FILE_MODE {
        return Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::InvalidEntry,
            "validate exact artifact entry",
        ));
    }
    let observed_identity = identity(&metadata);
    if expected_identity.is_some_and(|expected| expected != observed_identity) {
        return Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::IdentityChanged,
            "verify exact artifact identity",
        ));
    }
    if metadata.len() > MAX_FIXTURE_ARTIFACT_BYTES as u64 || metadata.len() != expected.len() as u64
    {
        return Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::ContentMismatch,
            "verify exact artifact length",
        ));
    }

    let mut offset = 0usize;
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    while offset < expected.len() {
        let chunk = (expected.len() - offset).min(buffer.len());
        let read = file
            .read(&mut buffer[..chunk])
            .map_err(|source| FixtureArtifactError::filesystem("read exact artifact", &source))?;
        if read == 0 || buffer[..read] != expected[offset..offset + read] {
            return Err(FixtureArtifactError::new(
                FixtureArtifactErrorKind::ContentMismatch,
                "compare exact artifact bytes",
            ));
        }
        offset += read;
    }
    let mut trailing = [0u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|source| FixtureArtifactError::filesystem("finish exact artifact read", &source))?
        != 0
    {
        return Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::ContentMismatch,
            "verify exact artifact boundary",
        ));
    }

    let current = open_file_nofollow(directory, name).map_err(|_| {
        FixtureArtifactError::new(
            FixtureArtifactErrorKind::IdentityChanged,
            "reopen exact artifact",
        )
    })?;
    let current = current
        .metadata()
        .map_err(|source| FixtureArtifactError::filesystem("inspect reopened artifact", &source))?;
    if !current.is_file()
        || mode(&current) != PRIVATE_FILE_MODE
        || identity(&current) != observed_identity
        || current.len() != expected.len() as u64
    {
        return Err(FixtureArtifactError::new(
            FixtureArtifactErrorKind::IdentityChanged,
            "verify reopened artifact",
        ));
    }
    Ok(observed_identity)
}

fn cleanup_stage(directory: &Dir, stage: &Path) {
    let _ = directory.remove_file(stage);
    let _ = sync_dir(directory);
}

fn fixture_digest(bytes: &[u8]) -> String {
    let mut hash = FIXTURE_FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FIXTURE_FNV_PRIME);
    }
    format!("fixture-fnv1a64-v1:{hash:016x}:{:016x}", bytes.len())
}

fn map_workspace_error(operation: &'static str, error: WorkspaceError) -> FixtureArtifactError {
    match error {
        WorkspaceError::IdentityChanged => {
            FixtureArtifactError::new(FixtureArtifactErrorKind::IdentityChanged, operation)
        }
        WorkspaceError::FixtureCapabilityUnavailable => {
            FixtureArtifactError::new(FixtureArtifactErrorKind::FixtureOnly, operation)
        }
        WorkspaceError::Io { source, .. } => FixtureArtifactError::filesystem(operation, &source),
        _ => FixtureArtifactError::new(FixtureArtifactErrorKind::IdentityChanged, operation),
    }
}

#[cfg(test)]
mod tests {
    use super::{fixture_digest, validate_artifact_name};

    #[test]
    fn fixture_digest_is_named_and_deterministic() {
        assert_eq!(
            fixture_digest(b"fixture output\n"),
            "fixture-fnv1a64-v1:b8a597e96207d04f:000000000000000f"
        );
    }

    #[test]
    fn artifact_name_rejects_parent_traversal() {
        assert!(validate_artifact_name("../output.md").is_err());
    }

    #[test]
    fn artifact_name_accepts_one_visible_ascii_component() {
        assert!(validate_artifact_name("output-1_final.md").is_ok());
    }
}
