//! Root-locked fixture authority and derived filesystem capabilities.
//!
//! This layer does not currently reconcile macOS `.orchestrator-launch-*`
//! crash residue. A future recovery hook may remove such staging only while
//! holding this root's authority lock and only from process-broker metadata
//! that independently proves the directory was created and remains owned by
//! that broker. Names or prefixes alone are never sufficient deletion proof.

use crate::{
    capability::{CapabilityError, CapabilityRoot, SharedCapabilityRoot, private::Sealed},
    fs_util::{
        FileIdentity, create_dir_private, create_private_file, identity, link_count, mode,
        open_dir_path_nofollow, open_file_nofollow, sync_dir,
    },
    runtime_home::IsolatedFixtureRoot,
};
use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
#[cfg(all(unix, feature = "verification-process-canary"))]
use std::ffi::OsString;
use std::{
    collections::BTreeSet,
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub(crate) const AUTHORITY_MARKER: &str = ".orchestrator-fixture-authority";
// Protocol v2 adds the root-directory lock to the existing named-file lock.
// Recovery deliberately rejects v1 instead of migrating its single-lock roots:
// fixture roots are disposable, and an in-place lock-protocol migration could
// admit split authority.
pub(crate) const AUTHORITY_MARKER_BYTES: &[u8] = b"orchestrator-rs-fixture-v2\n";
pub(crate) const AUTHORITY_LOCK: &str = ".orchestrator-fixture-lock";
const MAX_FIXTURE_EXECUTABLE_BYTES: usize = 64 * 1024 * 1024;
#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) const HERMETIC_CANARY_COMPAT_TARGET: &str = "compat";
#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) const HERMETIC_CANARY_PRIVATE_LEDGER_TARGET: &str = "private-ledger";
#[cfg(all(unix, feature = "verification-process-canary"))]
const HERMETIC_CANARY_LAYOUT_MANIFEST: &str = ".orchestrator-hermetic-canary-layout-v1";
#[cfg(all(unix, feature = "verification-process-canary"))]
const HERMETIC_CANARY_LAYOUT_DOMAIN: &[u8] = b"orchestrator-rs-hermetic-canary-layout";
#[cfg(all(unix, feature = "verification-process-canary"))]
const HERMETIC_CANARY_LAYOUT_VERSION: u8 = 1;

/// Explicit ambient roots that a fresh fixture must neither overlap nor contain.
pub struct FixtureAdmissionPolicy {
    user_home: PathBuf,
    repository_checkout: PathBuf,
    runtime_home_candidates: Vec<PathBuf>,
    forbidden_roots: Vec<PathBuf>,
    temporary_directory: PathBuf,
    expected_fixture_helper: Option<Arc<[u8]>>,
}

impl fmt::Debug for FixtureAdmissionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureAdmissionPolicy")
            .field(
                "expected_fixture_helper",
                &self.expected_fixture_helper.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl FixtureAdmissionPolicy {
    #[must_use]
    pub fn new(
        user_home: impl Into<PathBuf>,
        repository_checkout: impl Into<PathBuf>,
        temporary_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            user_home: user_home.into(),
            repository_checkout: repository_checkout.into(),
            runtime_home_candidates: Vec::new(),
            forbidden_roots: Vec::new(),
            temporary_directory: temporary_directory.into(),
            expected_fixture_helper: None,
        }
    }

    #[must_use]
    pub fn with_runtime_home_candidate(mut self, path: impl Into<PathBuf>) -> Self {
        self.runtime_home_candidates.push(path.into());
        self
    }

    #[must_use]
    pub fn with_forbidden_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.forbidden_roots.push(path.into());
        self
    }

    /// Pins the only native helper bytes this fixture may install and execute.
    #[must_use]
    pub fn with_expected_fixture_helper(mut self, bytes: &[u8]) -> Self {
        self.expected_fixture_helper = Some(Arc::from(bytes));
        self
    }

    /// Returns the canonical temporary directory this policy was constructed
    /// with. Exposed crate-private so the sealed exact-leaf authority can open
    /// the publishing parent and compute the canonical leaf path without
    /// re-reading private inputs.
    #[must_use]
    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn temporary_directory(&self) -> &std::path::Path {
        &self.temporary_directory
    }
}

#[derive(Debug, Error)]
pub enum FixtureAuthorityError {
    #[error("fixture root is not an exact mode-0700 directory")]
    InvalidRootMode,
    #[error("fixture root is not fresh and empty")]
    RootNotFresh,
    #[error("fixture root was not atomically created by the fixture harness")]
    RootNotHarnessCreated,
    #[error("fixture root overlaps forbidden root class {class}")]
    ForbiddenOverlap { class: &'static str },
    #[error("fixture capability filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("fixture authority identity changed during use")]
    IdentityChanged,
    #[error("target identifier is invalid")]
    InvalidTargetId,
    #[error("target settings are already leased")]
    TargetAlreadyLeased,
    #[error("fixture executable label or bytes are invalid")]
    InvalidExecutable,
}

pub(crate) struct FixtureBoundary {
    pub(crate) canonical_path: PathBuf,
    pub(crate) directory: Dir,
    identity: FileIdentity,
    marker_identity: FileIdentity,
    marker_file: cap_std::fs::File,
    lock_identity: FileIdentity,
    lock_file: cap_std::fs::File,
    root_lock: std::fs::File,
}

/// Prepared fixture authority whose root has not yet been published at its
/// caller-visible name.
///
/// This typestate deliberately does not implement [`CapabilityRoot`]. It owns
/// the same root, marker, and lock handles that will be moved into the sole
/// [`FixtureBoundary`] after exact-name publication succeeds.
pub(crate) struct UnpublishedFixtureAuthority {
    intended_final_path: PathBuf,
    directory: Dir,
    identity: FileIdentity,
    marker_identity: FileIdentity,
    marker_file: cap_std::fs::File,
    lock_identity: FileIdentity,
    lock_file: cap_std::fs::File,
    root_lock: std::fs::File,
    expected_fixture_helper: Option<Arc<[u8]>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixturePreparationPoint {
    MarkerCreatedBeforeIdentityRetention,
    LockCreatedBeforeIdentityRetention,
}

impl fmt::Debug for UnpublishedFixtureAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnpublishedFixtureAuthority")
            .field("kind", &"unpublished-isolated-fixture")
            .finish()
    }
}

impl fmt::Debug for FixtureBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureBoundary")
            .field("kind", &"isolated-fixture")
            .finish_non_exhaustive()
    }
}

impl Sealed for FixtureBoundary {}

impl CapabilityRoot for FixtureBoundary {
    fn verify(&self) -> Result<(), CapabilityError> {
        let reopened = crate::runtime_home::open_canonical_directory(&self.canonical_path)?;
        let root_metadata = reopened.dir_metadata()?;
        if !is_private_fixture_directory(&root_metadata)
            || identity(&root_metadata) != self.identity
            || std_file_identity(&self.root_lock)? != self.identity
        {
            return Err(CapabilityError::IdentityChanged);
        }
        let (_, marker_identity) =
            inspect_fixture_control_file(&reopened, AUTHORITY_MARKER, AUTHORITY_MARKER_BYTES)
                .map_err(capability_error_from_fixture)?;
        let retained_marker_metadata = self.marker_file.metadata()?;
        if !is_private_control_file(&retained_marker_metadata)
            || identity(&retained_marker_metadata) != self.marker_identity
            || marker_identity != self.marker_identity
        {
            return Err(CapabilityError::IdentityChanged);
        }
        let (_, lock_identity) = inspect_fixture_control_file(&reopened, AUTHORITY_LOCK, b"")
            .map_err(capability_error_from_fixture)?;
        let retained_lock_metadata = self.lock_file.metadata()?;
        if !is_private_control_file(&retained_lock_metadata)
            || identity(&retained_lock_metadata) != self.lock_identity
            || lock_identity != self.lock_identity
        {
            return Err(CapabilityError::IdentityChanged);
        }
        Ok(())
    }

    fn directory(&self) -> &Dir {
        &self.directory
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

/// Fixture-only sidecar holding leasing state shared by every authority
/// admitted under one fixture root. Production boundaries carry no extras.
pub(crate) struct FixtureExtras {
    pub(crate) target_leases: Mutex<BTreeSet<FileIdentity>>,
    pub(crate) projection_leases: Mutex<BTreeSet<FileIdentity>>,
}

impl FixtureExtras {
    pub(crate) fn new() -> Self {
        Self {
            target_leases: Mutex::new(BTreeSet::new()),
            projection_leases: Mutex::new(BTreeSet::new()),
        }
    }
}

impl fmt::Debug for FixtureExtras {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureExtras")
            .finish_non_exhaustive()
    }
}

impl Default for FixtureExtras {
    fn default() -> Self {
        Self::new()
    }
}

impl From<CapabilityError> for FixtureAuthorityError {
    fn from(error: CapabilityError) -> Self {
        match error {
            CapabilityError::IdentityChanged => Self::IdentityChanged,
            CapabilityError::RootNotFreshlyCreated => Self::RootNotHarnessCreated,
            // A fixture authority is never recovered through the Git-effect
            // door, so this arm is unreachable here; it reports the freshness
            // fault it is rather than being widened into this enum.
            CapabilityError::RootFreshlyCreated => Self::RootNotFresh,
            CapabilityError::Io(source) => Self::Io(source),
        }
    }
}

impl UnpublishedFixtureAuthority {
    /// Validates a newly created, still-hidden root, installs the stable v2
    /// fixture controls, synchronizes them, and acquires both authority locks.
    pub(crate) fn prepare(
        staging_canonical_path: &Path,
        intended_final_path: &Path,
        directory: Dir,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        Self::prepare_with_hook(
            staging_canonical_path,
            intended_final_path,
            directory,
            policy,
            |_| {},
        )
    }

    pub(crate) fn prepare_with_hook(
        staging_canonical_path: &Path,
        intended_final_path: &Path,
        directory: Dir,
        policy: &FixtureAdmissionPolicy,
        mut hook: impl FnMut(FixturePreparationPoint),
    ) -> Result<Self, FixtureAuthorityError> {
        let metadata = directory.dir_metadata()?;
        validate_fresh_fixture_root(staging_canonical_path, &directory, &metadata, policy)?;
        validate_policy_boundary(intended_final_path, policy)?;

        let root_lock = directory.try_clone()?.into_std_file();
        let (marker_file, marker_identity) = create_private_control_file(
            &directory,
            Path::new(AUTHORITY_MARKER),
            AUTHORITY_MARKER_BYTES,
            || hook(FixturePreparationPoint::MarkerCreatedBeforeIdentityRetention),
        )?;
        let (lock_file, lock_identity) =
            create_private_control_file(&directory, Path::new(AUTHORITY_LOCK), b"", || {
                hook(FixturePreparationPoint::LockCreatedBeforeIdentityRetention)
            })?;
        sync_dir(&directory)?;

        // LOCK ORDER: take the root lock first, then the exact named-lock file
        // description returned by create-new. Neither control is reopened to
        // obtain the retained publication authority.
        rustix::fs::flock(
            &root_lock,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(std::io::Error::from)?;
        rustix::fs::flock(
            &lock_file,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(std::io::Error::from)?;

        let prepared = Self {
            intended_final_path: intended_final_path.to_path_buf(),
            directory,
            identity: identity(&metadata),
            marker_identity,
            marker_file,
            lock_identity,
            lock_file,
            root_lock,
            expected_fixture_helper: policy.expected_fixture_helper.clone(),
        };
        verify_retained_control_files(&prepared, &prepared.directory)?;
        sync_dir(&prepared.directory)?;
        Ok(prepared)
    }

    /// Verifies that the published name and both control names still resolve
    /// to the retained objects, then moves the retained handles into the sole
    /// fixture capability.
    pub(crate) fn into_published(self) -> Result<FreshFixtureAuthority, FixtureAuthorityError> {
        verify_prepared_fixture_at(&self, &self.intended_final_path)?;
        let boundary: SharedCapabilityRoot = Arc::new(FixtureBoundary {
            canonical_path: self.intended_final_path,
            directory: self.directory,
            identity: self.identity,
            marker_identity: self.marker_identity,
            marker_file: self.marker_file,
            lock_identity: self.lock_identity,
            lock_file: self.lock_file,
            root_lock: self.root_lock,
        });
        boundary.verify()?;
        Ok(FreshFixtureAuthority {
            boundary,
            extras: Arc::new(FixtureExtras::new()),
            expected_fixture_helper: self.expected_fixture_helper,
        })
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn verify_named_in_parent(
        &self,
        parent: &Dir,
        name: &Path,
    ) -> Result<(), FixtureAuthorityError> {
        let named = open_dir_path_nofollow(parent, name)?;
        let metadata = named.dir_metadata()?;
        if !is_private_fixture_directory(&metadata) || identity(&metadata) != self.identity {
            return Err(FixtureAuthorityError::IdentityChanged);
        }
        verify_retained_control_files(self, &named)
    }
}

/// Opaque, fixture-only mutation authority. It has no production constructor or enrollment path.
pub struct FreshFixtureAuthority {
    pub(crate) boundary: SharedCapabilityRoot,
    pub(crate) extras: Arc<FixtureExtras>,
    expected_fixture_helper: Option<Arc<[u8]>>,
}

impl fmt::Debug for FreshFixtureAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FreshFixtureAuthority")
            .field("kind", &"fresh-isolated-fixture")
            .finish()
    }
}

impl FreshFixtureAuthority {
    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) fn verify(&self) -> Result<(), FixtureAuthorityError> {
        self.boundary.verify().map_err(Into::into)
    }

    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) fn canonical_path(&self) -> &Path {
        self.boundary.canonical_path()
    }

    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) fn directory(&self) -> &Dir {
        self.boundary.directory()
    }

    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) fn identity(&self) -> Result<FileIdentity, FixtureAuthorityError> {
        self.verify()?;
        Ok(identity(&self.boundary.directory().dir_metadata()?))
    }

    pub fn admit(
        fixture: IsolatedFixtureRoot,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let (path, directory, freshly_created) = fixture.into_boundary();
        if !freshly_created {
            return Err(FixtureAuthorityError::RootNotHarnessCreated);
        }
        UnpublishedFixtureAuthority::prepare(&path, &path, directory, policy)?.into_published()
    }

    /// Reopens only a previously marked v2 fixture root for durable recovery.
    ///
    /// V1 roots are rejected without mutation because the dual-lock protocol is
    /// not migrated in place; fixture roots are disposable.
    pub fn recover(
        fixture: IsolatedFixtureRoot,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let (path, directory, freshly_created) = fixture.into_boundary();
        if freshly_created {
            return Err(FixtureAuthorityError::RootNotFresh);
        }
        Self::recover_existing(path, directory, policy)
    }

    pub(crate) fn recover_existing(
        path: PathBuf,
        directory: Dir,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let metadata = directory.dir_metadata()?;
        if !is_private_fixture_directory(&metadata) {
            return Err(FixtureAuthorityError::InvalidRootMode);
        }
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt as CapMetadataExt;
            use std::os::unix::fs::MetadataExt as StdMetadataExt;
            let temporary_owner = std::fs::metadata(&policy.temporary_directory)?.uid();
            if metadata.uid() != temporary_owner
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(FixtureAuthorityError::InvalidRootMode);
            }
        }
        validate_policy_boundary(&path, policy)?;
        let (root_lock, lock_file, lock_identity) = acquire_fixture_lock(&directory)?;
        let (marker_file, marker_identity) =
            inspect_fixture_control_file(&directory, AUTHORITY_MARKER, AUTHORITY_MARKER_BYTES)?;
        let boundary: SharedCapabilityRoot = Arc::new(FixtureBoundary {
            canonical_path: path,
            identity: identity(&metadata),
            directory,
            marker_identity,
            marker_file,
            lock_identity,
            lock_file,
            root_lock,
        });
        boundary.verify()?;
        Ok(Self {
            boundary,
            extras: Arc::new(FixtureExtras::new()),
            expected_fixture_helper: policy.expected_fixture_helper.clone(),
        })
    }

    /// Admits the runtime home B5-DESIGN §7's isolated door already vetted.
    ///
    /// This is **not** a widening of the fixture policy. `prepare` and
    /// `recover_existing` both call the unchanged
    /// [`validate_policy_boundary`], so the home must still be inside both the
    /// policy temporary directory and the process temporary directory, and
    /// must still overlap none of `$HOME/.alluka`, `$HOME/.via`, the
    /// repository checkout, or any declared runtime-home candidate. What §7
    /// adds is only that the vetted path may be named by the operator
    /// (`ALLUKA_HOME`) instead of being a random `create_fresh` leaf — which is
    /// what makes it usable as a *runtime home* rather than as a fixture root
    /// beneath one.
    ///
    /// Two prior states are accepted, and they are the same two
    /// `FreshFixtureAuthority` has always accepted, reached through the same
    /// two functions: an empty private directory is prepared, and one already
    /// carrying this crate's v2 authority marker is recovered. Anything else
    /// is D6's `IsolatedHomeNotFresh` and is refused by the caller before this
    /// function is reached.
    pub(crate) fn admit_isolated_home(
        canonical_path: &Path,
        directory: Dir,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let marked = open_file_nofollow(&directory, Path::new(AUTHORITY_MARKER)).is_ok();
        if marked {
            return Self::recover_existing(canonical_path.to_path_buf(), directory, policy);
        }
        UnpublishedFixtureAuthority::prepare(canonical_path, canonical_path, directory, policy)?
            .into_published()
    }

    pub fn create_target(
        &self,
        target_id: &str,
    ) -> Result<TargetRootAuthority, FixtureAuthorityError> {
        if !crate::fs_util::validate_component(target_id) {
            return Err(FixtureAuthorityError::InvalidTargetId);
        }
        self.boundary.verify()?;
        let targets = ensure_private_child(self.boundary.directory(), Path::new("targets"))?;
        create_dir_private(&targets, Path::new(target_id))?;
        let directory = open_dir_path_nofollow(&targets, Path::new(target_id))?;
        if !is_private_fixture_directory(&directory.dir_metadata()?) {
            return Err(FixtureAuthorityError::InvalidRootMode);
        }
        sync_dir(&directory)?;
        sync_dir(&targets)?;
        let target = TargetRootAuthority::new(
            Arc::clone(&self.boundary),
            Arc::clone(&self.extras),
            target_id.to_owned(),
            targets,
            directory,
        )?;
        target.verify()?;
        Ok(target)
    }

    pub fn open_target(
        &self,
        target_id: &str,
    ) -> Result<TargetRootAuthority, FixtureAuthorityError> {
        if !crate::fs_util::validate_component(target_id) {
            return Err(FixtureAuthorityError::InvalidTargetId);
        }
        self.boundary.verify()?;
        let targets = open_dir_path_nofollow(self.boundary.directory(), Path::new("targets"))?;
        let directory = open_dir_path_nofollow(&targets, Path::new(target_id))?;
        TargetRootAuthority::new(
            Arc::clone(&self.boundary),
            Arc::clone(&self.extras),
            target_id.to_owned(),
            targets,
            directory,
        )
    }

    /// Installs one native test helper below the admitted fixture root and returns its capability.
    pub fn install_fixture_executable(
        &self,
        label: &str,
        bytes: &[u8],
    ) -> Result<ExecutableCapability, FixtureAuthorityError> {
        if !crate::fs_util::validate_component(label)
            || bytes.is_empty()
            || bytes.len() > MAX_FIXTURE_EXECUTABLE_BYTES
            || !is_native_executable(bytes)
            || self
                .expected_fixture_helper
                .as_deref()
                .is_none_or(|expected| expected != bytes)
        {
            return Err(FixtureAuthorityError::InvalidExecutable);
        }
        self.boundary.verify()?;
        let _home = ensure_private_child(self.boundary.directory(), Path::new("home"))?;
        let _tmp = ensure_private_child(self.boundary.directory(), Path::new("tmp"))?;
        let bin = ensure_private_child(self.boundary.directory(), Path::new("bin"))?;
        let relative = Path::new(label);
        create_private_file(&bin, relative, bytes, false)?;
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            bin.set_permissions(relative, cap_std::fs::Permissions::from_mode(0o700))?;
        }
        let file = crate::fs_util::open_file_nofollow(&bin, relative)?;
        file.sync_all()?;
        sync_dir(&bin)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || mode(&metadata) != 0o700 {
            return Err(FixtureAuthorityError::InvalidExecutable);
        }
        Ok(ExecutableCapability {
            boundary: Arc::clone(&self.boundary),
            label: label.to_owned(),
            identity: identity(&metadata),
            expected_bytes: Arc::from(bytes),
        })
    }

    /// Reconstructs the exact fixture-executable capability after a fixture
    /// process restart.
    ///
    /// Fixture-only, gated exactly like [`crate::open_fixture_runtime_store`]
    /// and [`crate::fixture_production_boundary`]: it requires policy-pinned
    /// bytes plus an admitted fixture authority, never a path, so live provider
    /// enrollment cannot reach it, and it is compiled out of every production
    /// build.
    ///
    /// **Lease amendment (M4a-r2, B5).** This was `pub(crate)`. B5's
    /// composition root installs the attested helper in seal 5, and a second
    /// seal over the same durable home — every crash-resume case in
    /// `production_composition_crash_e2e` — must *re-admit* the installed
    /// helper rather than reinstall it, because reinstalling changes the file
    /// identity the previous authority pinned.
    /// [`Self::install_fixture_executable`] fails closed with `AlreadyExists`
    /// on an existing file, and this is the only door that reconstructs the
    /// capability from one. Widening it to a `cfg`-gated `pub` adds no
    /// production-reachable constructor.
    ///
    /// # Errors
    /// Returns [`FixtureAuthorityError::InvalidExecutable`] when the label,
    /// the pinned bytes, or the installed file's mode does not match.
    #[cfg(any(test, feature = "test-support"))]
    pub fn recover_fixture_executable(
        &self,
        label: &str,
        bytes: &[u8],
    ) -> Result<ExecutableCapability, FixtureAuthorityError> {
        self.recover_fixture_executable_inner(label, bytes)
    }

    pub(crate) fn recover_fixture_executable_inner(
        &self,
        label: &str,
        bytes: &[u8],
    ) -> Result<ExecutableCapability, FixtureAuthorityError> {
        if !crate::fs_util::validate_component(label)
            || bytes.is_empty()
            || bytes.len() > MAX_FIXTURE_EXECUTABLE_BYTES
            || !is_native_executable(bytes)
            || self
                .expected_fixture_helper
                .as_deref()
                .is_none_or(|expected| expected != bytes)
        {
            return Err(FixtureAuthorityError::InvalidExecutable);
        }
        self.boundary.verify()?;
        let relative = Path::new("bin").join(label);
        let file = crate::fs_util::open_file_nofollow(self.boundary.directory(), &relative)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || mode(&metadata) != 0o700 {
            return Err(FixtureAuthorityError::InvalidExecutable);
        }
        let capability = ExecutableCapability {
            boundary: Arc::clone(&self.boundary),
            label: label.to_owned(),
            identity: identity(&metadata),
            expected_bytes: Arc::from(bytes),
        };
        let _verified = capability.open_verified()?;
        Ok(capability)
    }
}

fn acquire_fixture_lock(
    directory: &Dir,
) -> Result<(std::fs::File, cap_std::fs::File, FileIdentity), FixtureAuthorityError> {
    // LOCK ORDER: v2 always takes the root-directory lock before the named-file
    // lock. The named lock preserves exclusion with v1 binaries; the root lock
    // prevents two v2 authorities after lock-path replacement.
    let root_lock = directory.try_clone()?.into_std_file();
    rustix::fs::flock(
        &root_lock,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(std::io::Error::from)?;
    let (lock_file, lock_identity) = inspect_fixture_control_file(directory, AUTHORITY_LOCK, b"")?;
    rustix::fs::flock(
        &lock_file,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(std::io::Error::from)?;
    Ok((root_lock, lock_file, lock_identity))
}

/// Creates one fixture control and returns the exact create-new file
/// description plus its retained identity. A failure deliberately leaves any
/// created residue in place: pathname cleanup cannot prove it still names the
/// object created by this call.
fn create_private_control_file(
    directory: &Dir,
    name: &Path,
    bytes: &[u8],
    after_create: impl FnOnce(),
) -> Result<(cap_std::fs::File, FileIdentity), FixtureAuthorityError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = directory.open_with(name, &options)?;
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt};
        file.set_permissions(Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    after_create();
    let metadata = file.metadata()?;
    if !is_private_control_file(&metadata) {
        return Err(FixtureAuthorityError::InvalidRootMode);
    }
    Ok((file, identity(&metadata)))
}

fn inspect_fixture_control_file(
    directory: &Dir,
    name: &str,
    expected_bytes: &[u8],
) -> Result<(cap_std::fs::File, FileIdentity), FixtureAuthorityError> {
    let file = open_file_nofollow(directory, Path::new(name))?;
    let metadata = file.metadata()?;
    if !is_private_control_file(&metadata) {
        return Err(FixtureAuthorityError::InvalidRootMode);
    }
    let mut bytes = Vec::with_capacity(expected_bytes.len().saturating_add(1));
    Read::by_ref(&mut &file)
        .take(
            u64::try_from(expected_bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)?;
    if bytes != expected_bytes {
        return Err(FixtureAuthorityError::RootNotHarnessCreated);
    }
    Ok((file, identity(&metadata)))
}

fn validate_fresh_fixture_root(
    canonical_path: &Path,
    directory: &Dir,
    metadata: &cap_std::fs::Metadata,
    policy: &FixtureAdmissionPolicy,
) -> Result<(), FixtureAuthorityError> {
    if !is_private_fixture_directory(metadata) {
        return Err(FixtureAuthorityError::InvalidRootMode);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as CapMetadataExt;
        use std::os::unix::fs::MetadataExt as StdMetadataExt;

        let temporary_owner = std::fs::metadata(&policy.temporary_directory)?.uid();
        if metadata.uid() != temporary_owner
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(FixtureAuthorityError::InvalidRootMode);
        }
    }
    if directory.entries()?.next().is_some() {
        return Err(FixtureAuthorityError::RootNotFresh);
    }
    validate_policy_boundary(canonical_path, policy)
}

fn verify_prepared_fixture_at(
    prepared: &UnpublishedFixtureAuthority,
    canonical_path: &Path,
) -> Result<(), FixtureAuthorityError> {
    let retained_metadata = prepared.directory.dir_metadata()?;
    if !is_private_fixture_directory(&retained_metadata)
        || identity(&retained_metadata) != prepared.identity
        || std_file_identity(&prepared.root_lock)? != prepared.identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    verify_retained_control_files(prepared, &prepared.directory)?;

    let reopened = crate::runtime_home::open_canonical_directory(canonical_path)?;
    let reopened_metadata = reopened.dir_metadata()?;
    if !is_private_fixture_directory(&reopened_metadata)
        || identity(&reopened_metadata) != prepared.identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    verify_retained_control_files(prepared, &reopened)
}

fn verify_retained_control_files(
    prepared: &UnpublishedFixtureAuthority,
    directory: &Dir,
) -> Result<(), FixtureAuthorityError> {
    let retained_marker_metadata = prepared.marker_file.metadata()?;
    let retained_lock_metadata = prepared.lock_file.metadata()?;
    if !is_private_control_file(&retained_marker_metadata)
        || identity(&retained_marker_metadata) != prepared.marker_identity
        || !is_private_control_file(&retained_lock_metadata)
        || identity(&retained_lock_metadata) != prepared.lock_identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    let (_, marker_identity) =
        inspect_fixture_control_file(directory, AUTHORITY_MARKER, AUTHORITY_MARKER_BYTES)?;
    let (_, lock_identity) = inspect_fixture_control_file(directory, AUTHORITY_LOCK, b"")?;
    if marker_identity != prepared.marker_identity || lock_identity != prepared.lock_identity {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    Ok(())
}

fn is_private_control_file(metadata: &cap_std::fs::Metadata) -> bool {
    if !metadata.is_file() || mode(metadata) != 0o600 || link_count(metadata) != 1 {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        metadata.mode() & 0o7777 == 0o600 && metadata.uid() == rustix::process::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Exactly a mode-0700, euid-owned directory.
///
/// B5-DESIGN §7.3's D5 reuses this predicate verbatim rather than restating
/// it, so the isolated-home door and fixture admission cannot drift on what
/// "private" means. Only its visibility changed for §7; the body is the one
/// fixture admission has always used.
pub(crate) fn is_private_fixture_directory(metadata: &cap_std::fs::Metadata) -> bool {
    if !metadata.is_dir() || mode(metadata) != 0o700 {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        metadata.mode() & 0o7777 == 0o700 && metadata.uid() == rustix::process::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn std_file_identity(file: &std::fs::File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "retained fixture root descriptor is unsafe",
        ));
    }
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn capability_error_from_fixture(error: FixtureAuthorityError) -> CapabilityError {
    match error {
        FixtureAuthorityError::Io(source) => CapabilityError::Io(source),
        _ => CapabilityError::IdentityChanged,
    }
}

fn ensure_private_child(parent: &Dir, name: &Path) -> std::io::Result<Dir> {
    let created = match create_dir_private(parent, name) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error),
    };
    let directory = open_dir_path_nofollow(parent, name)?;
    if !is_private_fixture_directory(&directory.dir_metadata()?) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "fixture directory mode, type, or owner is unsafe",
        ));
    }
    if created {
        sync_dir(&directory)?;
        sync_dir(parent)?;
    }
    Ok(directory)
}

fn reject_overlap(
    fixture: &Path,
    forbidden: &Path,
    class: &'static str,
) -> Result<(), FixtureAuthorityError> {
    let forbidden = resolve_for_policy(forbidden)?;
    if fixture == forbidden || fixture.starts_with(&forbidden) || forbidden.starts_with(fixture) {
        return Err(FixtureAuthorityError::ForbiddenOverlap { class });
    }
    Ok(())
}

pub(crate) fn validate_policy_boundary(
    path: &Path,
    policy: &FixtureAdmissionPolicy,
) -> Result<(), FixtureAuthorityError> {
    reject_overlap(path, &policy.user_home.join(".alluka"), "live-alluka")?;
    reject_overlap(path, &policy.user_home.join(".via"), "live-via")?;
    reject_overlap(path, &policy.repository_checkout, "repository-checkout")?;
    for candidate in &policy.runtime_home_candidates {
        reject_overlap(path, candidate, "runtime-home-candidate")?;
    }
    for forbidden in &policy.forbidden_roots {
        reject_overlap(path, forbidden, "explicit-forbidden-root")?;
    }
    let temporary = canonical_existing(&policy.temporary_directory)?;
    if !path.starts_with(&temporary) {
        return Err(FixtureAuthorityError::ForbiddenOverlap {
            class: "outside-test-tmpdir",
        });
    }
    let process_temporary = canonical_existing(&std::env::temp_dir())?;
    if !path.starts_with(&process_temporary) {
        return Err(FixtureAuthorityError::ForbiddenOverlap {
            class: "outside-process-tmpdir",
        });
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        reject_overlap(path, &home.join(".alluka"), "process-live-alluka")?;
        reject_overlap(path, &home.join(".via"), "process-live-via")?;
    }
    Ok(())
}

fn canonical_existing(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

fn resolve_for_policy(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| std::io::Error::other("path has no existing ancestor"))?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    }
    let mut result = std::fs::canonicalize(existing)?;
    for name in missing.iter().rev() {
        result.push(name);
    }
    Ok(result)
}

/// Capability for exactly one fixture-local target root.
pub struct TargetRootAuthority {
    pub(crate) boundary: SharedCapabilityRoot,
    pub(crate) extras: Arc<FixtureExtras>,
    pub(crate) id: String,
    pub(crate) targets_directory: Dir,
    pub(crate) targets_identity: FileIdentity,
    pub(crate) directory: Dir,
    pub(crate) identity: FileIdentity,
}

impl fmt::Debug for TargetRootAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetRootAuthority")
            .field("kind", &"fixture-target")
            .finish()
    }
}

impl TargetRootAuthority {
    fn new(
        boundary: SharedCapabilityRoot,
        extras: Arc<FixtureExtras>,
        id: String,
        targets_directory: Dir,
        directory: Dir,
    ) -> Result<Self, FixtureAuthorityError> {
        let targets_metadata = targets_directory.dir_metadata()?;
        let metadata = directory.dir_metadata()?;
        if !is_private_fixture_directory(&targets_metadata)
            || !is_private_fixture_directory(&metadata)
        {
            return Err(FixtureAuthorityError::InvalidRootMode);
        }
        Ok(Self {
            boundary,
            extras,
            id,
            targets_identity: identity(&targets_metadata),
            targets_directory,
            identity: identity(&metadata),
            directory,
        })
    }

    pub(crate) fn verify(&self) -> Result<(), FixtureAuthorityError> {
        self.boundary.verify()?;
        let retained_targets_metadata = self.targets_directory.dir_metadata()?;
        let retained_target_metadata = self.directory.dir_metadata()?;
        if !is_private_fixture_directory(&retained_targets_metadata)
            || !is_private_fixture_directory(&retained_target_metadata)
            || identity(&retained_targets_metadata) != self.targets_identity
            || identity(&retained_target_metadata) != self.identity
        {
            return Err(FixtureAuthorityError::IdentityChanged);
        }
        let targets = open_dir_path_nofollow(self.boundary.directory(), Path::new("targets"))?;
        let current = open_dir_path_nofollow(&targets, Path::new(&self.id))?;
        let targets_metadata = targets.dir_metadata()?;
        let current_metadata = current.dir_metadata()?;
        if !is_private_fixture_directory(&targets_metadata)
            || !is_private_fixture_directory(&current_metadata)
            || identity(&targets_metadata) != self.targets_identity
            || identity(&current_metadata) != self.identity
        {
            return Err(FixtureAuthorityError::IdentityChanged);
        }
        Ok(())
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) struct HermeticCanaryLayoutManifest {
    manifest_identity: FileIdentity,
    manifest_file: cap_std::fs::File,
    root_identity: FileIdentity,
    targets_identity: FileIdentity,
    targets_directory: Dir,
    compatibility_identity: FileIdentity,
    compatibility_directory: Dir,
    private_ledger_identity: FileIdentity,
    private_ledger_directory: Dir,
    exact_bytes: Vec<u8>,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
impl fmt::Debug for HermeticCanaryLayoutManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticCanaryLayoutManifest")
            .field("kind", &"versioned-identity-record")
            .finish()
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) fn publish_hermetic_canary_layout_manifest(
    fixture: &FreshFixtureAuthority,
    compatibility: &TargetRootAuthority,
    private_ledger: &TargetRootAuthority,
) -> Result<(), FixtureAuthorityError> {
    if !Arc::ptr_eq(&fixture.boundary, &compatibility.boundary)
        || !Arc::ptr_eq(&fixture.boundary, &private_ledger.boundary)
        || !Arc::ptr_eq(&fixture.extras, &compatibility.extras)
        || !Arc::ptr_eq(&fixture.extras, &private_ledger.extras)
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    let identities = hermetic_canary_layout_identities(compatibility, private_ledger)?;
    verify_exact_directory_entries(
        fixture.boundary.directory(),
        &[AUTHORITY_MARKER, AUTHORITY_LOCK, "targets"],
    )?;
    verify_exact_directory_entries(
        &compatibility.targets_directory,
        &[
            HERMETIC_CANARY_COMPAT_TARGET,
            HERMETIC_CANARY_PRIVATE_LEDGER_TARGET,
        ],
    )?;

    sync_dir(&compatibility.directory)?;
    sync_dir(&private_ledger.directory)?;
    sync_dir(&compatibility.targets_directory)?;
    sync_dir(fixture.boundary.directory())?;
    let exact_bytes = hermetic_canary_layout_manifest_bytes(identities);
    create_private_file(
        fixture.boundary.directory(),
        Path::new(HERMETIC_CANARY_LAYOUT_MANIFEST),
        &exact_bytes,
        false,
    )?;
    sync_dir(fixture.boundary.directory())?;
    let _manifest = admit_hermetic_canary_layout_manifest(compatibility, private_ledger)?;
    Ok(())
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) fn admit_hermetic_canary_layout_manifest(
    compatibility: &TargetRootAuthority,
    private_ledger: &TargetRootAuthority,
) -> Result<HermeticCanaryLayoutManifest, FixtureAuthorityError> {
    let identities = hermetic_canary_layout_identities(compatibility, private_ledger)?;
    verify_exact_hermetic_canary_layout(&compatibility.boundary, &compatibility.targets_directory)?;
    let exact_bytes = hermetic_canary_layout_manifest_bytes(identities);
    let (manifest_file, manifest_identity) = inspect_fixture_control_file(
        compatibility.boundary.directory(),
        HERMETIC_CANARY_LAYOUT_MANIFEST,
        &exact_bytes,
    )?;
    Ok(HermeticCanaryLayoutManifest {
        manifest_identity,
        manifest_file,
        root_identity: identities.root,
        targets_identity: identities.targets,
        targets_directory: compatibility.targets_directory.try_clone()?,
        compatibility_identity: identities.compatibility,
        compatibility_directory: compatibility.directory.try_clone()?,
        private_ledger_identity: identities.private_ledger,
        private_ledger_directory: private_ledger.directory.try_clone()?,
        exact_bytes,
    })
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) fn verify_hermetic_canary_layout_manifest(
    fixture_parent: &SharedCapabilityRoot,
    manifest: &HermeticCanaryLayoutManifest,
) -> Result<(), FixtureAuthorityError> {
    fixture_parent.verify()?;
    let root_metadata = fixture_parent.directory().dir_metadata()?;
    if !is_private_fixture_directory(&root_metadata)
        || identity(&root_metadata) != manifest.root_identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }

    let retained_manifest_metadata = manifest.manifest_file.metadata()?;
    let retained_targets_metadata = manifest.targets_directory.dir_metadata()?;
    let retained_compatibility_metadata = manifest.compatibility_directory.dir_metadata()?;
    let retained_private_ledger_metadata = manifest.private_ledger_directory.dir_metadata()?;
    if !is_private_control_file(&retained_manifest_metadata)
        || identity(&retained_manifest_metadata) != manifest.manifest_identity
        || !is_private_fixture_directory(&retained_targets_metadata)
        || identity(&retained_targets_metadata) != manifest.targets_identity
        || !is_private_fixture_directory(&retained_compatibility_metadata)
        || identity(&retained_compatibility_metadata) != manifest.compatibility_identity
        || !is_private_fixture_directory(&retained_private_ledger_metadata)
        || identity(&retained_private_ledger_metadata) != manifest.private_ledger_identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }

    let retained_named_compatibility = open_dir_path_nofollow(
        &manifest.targets_directory,
        Path::new(HERMETIC_CANARY_COMPAT_TARGET),
    )?;
    let retained_named_private_ledger = open_dir_path_nofollow(
        &manifest.targets_directory,
        Path::new(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
    )?;
    let retained_named_compatibility_metadata = retained_named_compatibility.dir_metadata()?;
    let retained_named_private_ledger_metadata = retained_named_private_ledger.dir_metadata()?;
    if !is_private_fixture_directory(&retained_named_compatibility_metadata)
        || identity(&retained_named_compatibility_metadata) != manifest.compatibility_identity
        || !is_private_fixture_directory(&retained_named_private_ledger_metadata)
        || identity(&retained_named_private_ledger_metadata) != manifest.private_ledger_identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }

    let targets = open_dir_path_nofollow(fixture_parent.directory(), Path::new("targets"))?;
    let compatibility = open_dir_path_nofollow(&targets, Path::new(HERMETIC_CANARY_COMPAT_TARGET))?;
    let private_ledger =
        open_dir_path_nofollow(&targets, Path::new(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET))?;
    let targets_metadata = targets.dir_metadata()?;
    let compatibility_metadata = compatibility.dir_metadata()?;
    let private_ledger_metadata = private_ledger.dir_metadata()?;
    if !is_private_fixture_directory(&targets_metadata)
        || !is_private_fixture_directory(&compatibility_metadata)
        || !is_private_fixture_directory(&private_ledger_metadata)
        || identity(&targets_metadata) != manifest.targets_identity
        || identity(&compatibility_metadata) != manifest.compatibility_identity
        || identity(&private_ledger_metadata) != manifest.private_ledger_identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    verify_exact_hermetic_canary_layout(fixture_parent, &targets)?;
    let expected_bytes = hermetic_canary_layout_manifest_bytes(HermeticCanaryLayoutIdentities {
        root: manifest.root_identity,
        targets: manifest.targets_identity,
        compatibility: manifest.compatibility_identity,
        private_ledger: manifest.private_ledger_identity,
    });
    if expected_bytes != manifest.exact_bytes {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    let (_, manifest_identity) = inspect_fixture_control_file(
        fixture_parent.directory(),
        HERMETIC_CANARY_LAYOUT_MANIFEST,
        &manifest.exact_bytes,
    )?;
    if manifest_identity != manifest.manifest_identity {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    Ok(())
}

#[cfg(all(unix, feature = "verification-process-canary"))]
#[derive(Clone, Copy)]
struct HermeticCanaryLayoutIdentities {
    root: FileIdentity,
    targets: FileIdentity,
    compatibility: FileIdentity,
    private_ledger: FileIdentity,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn hermetic_canary_layout_identities(
    compatibility: &TargetRootAuthority,
    private_ledger: &TargetRootAuthority,
) -> Result<HermeticCanaryLayoutIdentities, FixtureAuthorityError> {
    if compatibility.id != HERMETIC_CANARY_COMPAT_TARGET
        || private_ledger.id != HERMETIC_CANARY_PRIVATE_LEDGER_TARGET
    {
        return Err(FixtureAuthorityError::InvalidTargetId);
    }
    if !Arc::ptr_eq(&compatibility.boundary, &private_ledger.boundary)
        || !Arc::ptr_eq(&compatibility.extras, &private_ledger.extras)
        || compatibility.targets_identity != private_ledger.targets_identity
        || compatibility.identity == private_ledger.identity
    {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    compatibility.verify()?;
    private_ledger.verify()?;
    let root_metadata = compatibility.boundary.directory().dir_metadata()?;
    if !is_private_fixture_directory(&root_metadata) {
        return Err(FixtureAuthorityError::InvalidRootMode);
    }
    Ok(HermeticCanaryLayoutIdentities {
        root: identity(&root_metadata),
        targets: compatibility.targets_identity,
        compatibility: compatibility.identity,
        private_ledger: private_ledger.identity,
    })
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn hermetic_canary_layout_manifest_bytes(identities: HermeticCanaryLayoutIdentities) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HERMETIC_CANARY_LAYOUT_DOMAIN.len() + 2 + 64);
    bytes.extend_from_slice(HERMETIC_CANARY_LAYOUT_DOMAIN);
    bytes.push(0);
    bytes.push(HERMETIC_CANARY_LAYOUT_VERSION);
    for identity in [
        identities.root,
        identities.targets,
        identities.compatibility,
        identities.private_ledger,
    ] {
        bytes.extend_from_slice(&identity.device.to_be_bytes());
        bytes.extend_from_slice(&identity.inode.to_be_bytes());
    }
    bytes
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn verify_exact_hermetic_canary_layout(
    fixture_parent: &SharedCapabilityRoot,
    targets: &Dir,
) -> Result<(), FixtureAuthorityError> {
    verify_exact_directory_entries(
        fixture_parent.directory(),
        &[
            AUTHORITY_MARKER,
            AUTHORITY_LOCK,
            HERMETIC_CANARY_LAYOUT_MANIFEST,
            "targets",
        ],
    )?;
    verify_exact_directory_entries(
        targets,
        &[
            HERMETIC_CANARY_COMPAT_TARGET,
            HERMETIC_CANARY_PRIVATE_LEDGER_TARGET,
        ],
    )
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn verify_exact_directory_entries(
    directory: &Dir,
    expected: &[&str],
) -> Result<(), FixtureAuthorityError> {
    let mut actual = BTreeSet::new();
    for entry in directory.entries()? {
        actual.insert(entry?.file_name());
    }
    let expected = expected.iter().map(OsString::from).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(FixtureAuthorityError::IdentityChanged);
    }
    Ok(())
}

/// Capability for one fixture-installed native executable. It cannot be cloned or path-converted.
pub struct ExecutableCapability {
    pub(crate) boundary: SharedCapabilityRoot,
    pub(crate) label: String,
    pub(crate) identity: FileIdentity,
    expected_bytes: Arc<[u8]>,
}

impl fmt::Debug for ExecutableCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutableCapability")
            .field("kind", &"fixture-executable")
            .finish()
    }
}

impl ExecutableCapability {
    pub(crate) fn verify(&self) -> Result<PathBuf, FixtureAuthorityError> {
        let (_file, _image) = self.open_verified()?;
        let relative = Path::new("bin").join(&self.label);
        Ok(self.boundary.canonical_path().join(relative))
    }

    pub(crate) fn open_verified(
        &self,
    ) -> Result<(std::fs::File, Arc<[u8]>), FixtureAuthorityError> {
        use std::os::unix::fs::FileExt;

        self.boundary.verify()?;
        let relative = Path::new("bin").join(&self.label);
        let file = crate::fs_util::open_file_nofollow(self.boundary.directory(), &relative)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || mode(&metadata) != 0o700 || identity(&metadata) != self.identity {
            return Err(FixtureAuthorityError::IdentityChanged);
        }
        let file = file.into_std();
        if file.metadata()?.len() != self.expected_bytes.len() as u64 {
            return Err(FixtureAuthorityError::IdentityChanged);
        }
        let mut offset = 0usize;
        let mut buffer = [0u8; 8192];
        while offset < self.expected_bytes.len() {
            let remaining = self.expected_bytes.len() - offset;
            let chunk_len = remaining.min(buffer.len());
            let read = file.read_at(&mut buffer[..chunk_len], offset as u64)?;
            if read == 0
                || buffer[..read] != self.expected_bytes[offset..offset.saturating_add(read)]
            {
                return Err(FixtureAuthorityError::IdentityChanged);
            }
            offset = offset.saturating_add(read);
        }
        Ok((file, Arc::clone(&self.expected_bytes)))
    }
}

fn is_native_executable(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
        || bytes.starts_with(b"\x7fELF")
}
