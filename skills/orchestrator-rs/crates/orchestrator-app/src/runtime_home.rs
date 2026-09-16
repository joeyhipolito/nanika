//! Runtime-home resolution and mutation authority.
//!
//! Resolution is deliberately read-only: [`HomeInputs`] may contain caller-
//! supplied compatibility values, but those values never grant mutation
//! authority. Fixture and production authorization produce distinct types.
//!
//! External callers cannot mint production enrollment:
//!
//! ```compile_fail,E0451
//! let _ = orchestrator_app::LiveHomeCanaryCapability { _private: () };
//! ```
//!
//! Nor can they call the pure selector used by this crate's tests:
//!
//! ```compile_fail,E0624
//! let _ = orchestrator_app::LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("1"));
//! ```
//!
//! A fixture-authorized home cannot be promoted into a production boundary:
//!
//! ```compile_fail,E0308
//! use orchestrator_app::{
//!     AuthorizedFixtureRuntimeHome, LegacyQuiescenceProof, ProductionWriterAuthority,
//! };
//!
//! fn cannot_promote(
//!     home: &AuthorizedFixtureRuntimeHome,
//!     proof: LegacyQuiescenceProof,
//! ) {
//!     let _ = ProductionWriterAuthority::acquire(home, "test", proof);
//! }
//! ```
//!
//! Production authorization cannot be requested without enrollment proof:
//!
//! ```compile_fail,E0061
//! use orchestrator_app::ResolvedRuntimeHome;
//!
//! fn cannot_authorize(resolved: ResolvedRuntimeHome) {
//!     let _ = resolved.authorize_production();
//! }
//! ```
//!
//! Identifying an existing root is recovery input, not mutation authority:
//!
//! ```compile_fail,E0308
//! use orchestrator_app::{IsolatedFixtureRoot, ResolvedRuntimeHome};
//!
//! fn cannot_authorize(resolved: ResolvedRuntimeHome, raw: &IsolatedFixtureRoot) {
//!     let _ = resolved.authorize_fixture(raw);
//! }
//! ```

#[cfg(all(unix, feature = "verification-process-canary"))]
use crate::fixture_authority::{
    FixtureAuthorityError, HERMETIC_CANARY_COMPAT_TARGET, HERMETIC_CANARY_PRIVATE_LEDGER_TARGET,
    HermeticCanaryLayoutManifest, admit_hermetic_canary_layout_manifest,
    verify_hermetic_canary_layout_manifest,
};
use crate::{
    ApplicationError, FreshFixtureAuthority,
    capability::{CapabilityError, CapabilityRoot, SharedCapabilityRoot, private::Sealed},
    fixture_authority::TargetRootAuthority,
    fs_util::{
        FileIdentity, identity, open_dir_path_nofollow, open_file_nofollow, probe_file_nofollow,
    },
    writer_authority::WriterLeaseGuard,
};
use cap_std::{ambient_authority, fs::Dir};
use std::{
    collections::BTreeSet,
    collections::hash_map::RandomState,
    ffi::OsString,
    fmt,
    hash::BuildHasher,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static FIXTURE_NONCE: AtomicU64 = AtomicU64::new(1);

/// Filesystem query boundary used only for legacy path precedence.
pub trait DirectoryProbe {
    /// Returns whether any filesystem entry exists at `path`.
    fn exists(&self, path: &Path) -> bool;
}

/// Explicit ambient read authority for event-log inspection.
///
/// Acquisition opens the host filesystem root and attempts to open the current
/// directory exactly once, retaining the handle or its acquisition error.
/// Absolute paths are subsequently opened from the root; relative paths are
/// opened from the retained current-directory inode. Every component uses
/// no-follow semantics, and callers never receive either handle or any
/// mutation operation. A current-directory acquisition error is replayed for
/// relative paths and does not disable absolute-path inspection.
pub struct ReadOnlyEventLogAuthority {
    root: Dir,
    current_directory: Result<Dir, RetainedIoError>,
}

struct RetainedIoError {
    kind: std::io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl RetainedIoError {
    fn capture(source: std::io::Error) -> Self {
        Self {
            kind: source.kind(),
            raw_os_error: source.raw_os_error(),
            message: source.to_string(),
        }
    }

    fn recreate(&self) -> std::io::Error {
        self.raw_os_error.map_or_else(
            || std::io::Error::new(self.kind, self.message.clone()),
            std::io::Error::from_raw_os_error,
        )
    }
}

impl fmt::Debug for ReadOnlyEventLogAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadOnlyEventLogAuthority")
            .field("kind", &"ambient-read-only")
            .finish()
    }
}

impl ReadOnlyEventLogAuthority {
    /// Explicitly acquires the host root for read-only event-log inspection.
    ///
    /// This is the only public event-log facade operation that mints ambient
    /// authority. The retained token is intentionally neither cloneable nor
    /// convertible into a raw directory handle.
    pub fn acquire_ambient() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            let root = Dir::open_ambient_dir(Path::new("/"), ambient_authority())?;
            let current_directory = Dir::open_ambient_dir(Path::new("."), ambient_authority())
                .map_err(RetainedIoError::capture);
            Ok(Self {
                root,
                current_directory,
            })
        }
        #[cfg(not(unix))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "event-log ambient read authority is not implemented on this platform",
            ))
        }
    }

    /// Binds an existing absolute or current-directory-relative runtime home.
    pub fn bind_runtime_home(&self, home: &Path) -> std::io::Result<ReadOnlyEventLogRoot> {
        let (base, relative) = self.clean_acquisition_path(home)?;
        Ok(ReadOnlyEventLogRoot {
            directory: base.open_clean_directory(&relative)?,
        })
    }

    /// Binds an absolute or current-directory-relative event-log path.
    ///
    /// Unlike runtime-home compatibility resolution, direct paths are not
    /// lexically cleaned. Every named component is opened in order so a
    /// missing, non-directory, or symbolic-link entry cannot be canceled by a
    /// later `..` before no-follow validation observes it.
    pub fn bind_direct(&self, path: &Path) -> std::io::Result<ReadOnlyEventLogTarget> {
        let (base, relative) = self.direct_acquisition_path(path)?;
        let name = file_name(&relative)?;
        let parent_path = relative.parent().unwrap_or_else(|| Path::new(""));
        let parent = base.open_direct_directory(parent_path)?;
        Ok(ReadOnlyEventLogTarget { parent, name })
    }

    fn clean_acquisition_path(
        &self,
        path: &Path,
    ) -> std::io::Result<(EventLogAcquisitionBase<'_>, PathBuf)> {
        if path.is_absolute() {
            Ok((
                EventLogAcquisitionBase::Root(&self.root),
                trusted_root_relative_path(&normalize_lexically(path))?,
            ))
        } else {
            Ok((
                EventLogAcquisitionBase::CurrentDirectory(self.retained_current_directory()?),
                normalize_lexically(path),
            ))
        }
    }

    fn direct_acquisition_path(
        &self,
        path: &Path,
    ) -> std::io::Result<(EventLogAcquisitionBase<'_>, PathBuf)> {
        if path.is_absolute() {
            Ok((
                EventLogAcquisitionBase::Root(&self.root),
                trusted_root_relative_path(path)?,
            ))
        } else {
            Ok((
                EventLogAcquisitionBase::CurrentDirectory(self.retained_current_directory()?),
                path.to_path_buf(),
            ))
        }
    }

    fn retained_current_directory(&self) -> std::io::Result<&Dir> {
        self.current_directory
            .as_ref()
            .map_err(RetainedIoError::recreate)
    }

    #[cfg(test)]
    fn from_retained_directories(
        root: Dir,
        current_directory: Result<Dir, std::io::Error>,
    ) -> Self {
        Self {
            root,
            current_directory: current_directory.map_err(RetainedIoError::capture),
        }
    }
}

#[derive(Clone, Copy)]
enum EventLogAcquisitionBase<'a> {
    Root(&'a Dir),
    CurrentDirectory(&'a Dir),
}

impl EventLogAcquisitionBase<'_> {
    fn open_clean_directory(self, path: &Path) -> std::io::Result<Dir> {
        match self {
            Self::Root(root) => open_dir_path_nofollow(root, path),
            Self::CurrentDirectory(current_directory) => {
                open_direct_path_nofollow(current_directory, path)
            }
        }
    }

    fn open_direct_directory(self, path: &Path) -> std::io::Result<Dir> {
        match self {
            Self::Root(root) | Self::CurrentDirectory(root) => {
                open_direct_path_nofollow(root, path)
            }
        }
    }
}

#[cfg(unix)]
fn open_direct_path_nofollow(start: &Dir, path: &Path) -> std::io::Result<Dir> {
    use cap_primitives::fs::{open_dir_nofollow, open_parent_dir};

    let mut directory = start.try_clone()?.into_std_file();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                directory = open_parent_dir(&directory, ambient_authority())?;
            }
            Component::Normal(name) => {
                directory = open_dir_nofollow(&directory, Path::new(name))?;
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "event-log traversal path contains an absolute component",
                ));
            }
        }
    }
    Ok(Dir::from_std_file(directory))
}

#[cfg(not(unix))]
fn open_direct_path_nofollow(_start: &Dir, _path: &Path) -> std::io::Result<Dir> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "direct event-log path acquisition is not implemented on this platform",
    ))
}

/// Retained, read-only capability for resolving event logs beneath one runtime home.
///
/// Event-log descendants are always traversed relative to this retained handle.
pub struct ReadOnlyEventLogRoot {
    directory: Dir,
}

impl ReadOnlyEventLogRoot {
    /// Binds a file path relative to the retained runtime home.
    ///
    /// All parent components and the final entry are no-follow. The returned
    /// target can be reopened repeatedly, which supports bounded tail retries
    /// without reacquiring an ambient path.
    pub fn event_log(&self, relative: &Path) -> std::io::Result<ReadOnlyEventLogTarget> {
        bind_relative_read_target(&self.directory, relative)
    }
}

/// Reopenable read-only capability for one event-log entry.
///
/// This type exposes no directory handle or mutation operation. Each [`Self::open`]
/// is descriptor-relative and refuses to follow a final symbolic link.
pub struct ReadOnlyEventLogTarget {
    parent: Dir,
    name: OsString,
}

impl ReadOnlyEventLogTarget {
    /// Checks that the retained leaf currently exists and is not a symlink.
    pub fn probe(&self) -> std::io::Result<()> {
        probe_file_nofollow(&self.parent, Path::new(&self.name))
    }

    /// Opens the retained leaf read-only without following a symbolic link.
    pub fn open(&self) -> std::io::Result<std::fs::File> {
        open_file_nofollow(&self.parent, Path::new(&self.name)).map(cap_std::fs::File::into_std)
    }
}

fn bind_relative_read_target(
    root: &Dir,
    relative: &Path,
) -> std::io::Result<ReadOnlyEventLogTarget> {
    if relative.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "event-log path must be relative to its retained root",
        ));
    }
    let name = file_name(relative)?;
    let parent_path = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent = open_dir_path_nofollow(root, parent_path)?;
    Ok(ReadOnlyEventLogTarget { parent, name })
}

fn file_name(path: &Path) -> std::io::Result<OsString> {
    path.file_name().map(OsString::from).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "event-log path has no final file component",
        )
    })
}

#[cfg(unix)]
/// Converts one absolute path into a path beneath the retained `/` capability.
/// On macOS only, the root-owned compatibility aliases `/tmp` and `/var` are
/// mapped to `/private/tmp` and `/private/var` without cleaning the remaining
/// components; no other symbolic-link parent is authorized or followed.
fn trusted_root_relative_path(path: &Path) -> std::io::Result<PathBuf> {
    let relative = path.strip_prefix(Path::new("/")).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "event-log acquisition path is not absolute",
        )
    })?;
    #[cfg(target_os = "macos")]
    {
        let first = relative.components().next();
        if matches!(first, Some(Component::Normal(name)) if name == "tmp" || name == "var") {
            return Ok(Path::new("private").join(relative));
        }
    }
    Ok(relative.to_path_buf())
}

#[cfg(not(unix))]
fn trusted_root_relative_path(_path: &Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "event-log path acquisition is not implemented on this platform",
    ))
}

/// Explicit inputs needed to resolve a runtime home without reading process-global state.
#[derive(Clone)]
pub struct HomeInputs {
    /// User home used for legacy and fallback paths.
    pub user_home: PathBuf,
    /// `ORCHESTRATOR_CONFIG_DIR`.
    pub orchestrator_config_dir: Option<PathBuf>,
    /// `ALLUKA_HOME`.
    pub alluka_home: Option<PathBuf>,
    /// `VIA_HOME`.
    pub via_home: Option<PathBuf>,
}

impl fmt::Debug for HomeInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HomeInputs")
            .field(
                "orchestrator_config_dir_present",
                &self.orchestrator_config_dir.is_some(),
            )
            .field("alluka_home_present", &self.alluka_home.is_some())
            .field("via_home_present", &self.via_home.is_some())
            .finish()
    }
}

impl HomeInputs {
    /// Creates resolver inputs with no environment overrides.
    #[must_use]
    pub fn from_user_home(user_home: impl Into<PathBuf>) -> Self {
        Self {
            user_home: user_home.into(),
            orchestrator_config_dir: None,
            alluka_home: None,
            via_home: None,
        }
    }
}

/// Which compatibility precedence rule selected a runtime home.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeSelection {
    /// Selected by `ORCHESTRATOR_CONFIG_DIR`.
    OrchestratorConfigDir,
    /// Selected by `ALLUKA_HOME`.
    AllukaHome,
    /// Selected as `<VIA_HOME>/orchestrator`.
    ViaHome,
    /// Selected because the legacy `~/.alluka` path exists.
    ExistingAlluka,
    /// Selected as the `~/.via` compatibility fallback.
    ViaFallback,
}

/// Explicit proof that production runtime-home writes were enrolled.
///
/// During the Rust pre-default period, every production mutation requires this
/// capability, including mutations outside the live default home. It has a
/// single operator-controlled selector
/// ([`LiveHomeCanaryCapability::for_explicit_enrollment`]) gated on the
/// `NANIKA_LIVE_HOME_ENROLL=1` environment variable. Caller-supplied resolver
/// inputs therefore cannot weaken the mutation policy.
pub struct LiveHomeCanaryCapability {
    _private: (),
}

impl fmt::Debug for LiveHomeCanaryCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveHomeCanaryCapability")
            .field("kind", &"operator-enrollment")
            .finish()
    }
}

impl LiveHomeCanaryCapability {
    /// The single explicit enrollment selector. Returns a capability only when
    /// the operator has set `NANIKA_LIVE_HOME_ENROLL=1`. This is the sole point
    /// that unseals production live-home writes.
    #[must_use]
    pub fn for_explicit_enrollment() -> Option<Self> {
        Self::for_explicit_enrollment_if(std::env::var("NANIKA_LIVE_HOME_ENROLL").ok().as_deref())
    }

    /// Pure variant of the selector for deterministic callers and tests: pass
    /// the observed environment value explicitly. Only the exact value `"1"`
    /// enrolls; `"0"`, `"true"`, and missing are all refused.
    ///
    /// Crate-private so external callers cannot mint enrollment from a
    /// caller-supplied string; the sole unsealing point for external code is
    /// [`LiveHomeCanaryCapability::for_explicit_enrollment`], which reads the
    /// operator-controlled environment itself.
    #[must_use]
    pub(crate) fn for_explicit_enrollment_if(value: Option<&str>) -> Option<Self> {
        (value == Some("1")).then_some(Self { _private: () })
    }
}

// ---------------------------------------------------------------------------
// B5-DESIGN §7 — the opt-in isolated-home door
// ---------------------------------------------------------------------------

/// Explicit proof that this process was enrolled through B5-DESIGN §7's
/// isolated-home door.
///
/// It is a *third* authority, disjoint from both of its siblings, and it exists
/// because neither of them can do what M5's campaign needs. Widening
/// [`crate::FixtureAdmissionPolicy`]'s boundary check (§7.1 candidate A) would
/// delete the TMPDIR confinement for every caller, and reusing
/// [`LiveHomeCanaryCapability`] (candidate B) would redefine the one selector
/// the owner audits as "any home at all". This capability instead admits
/// exactly one shape — an explicit, private, non-live, fresh-or-self-marked
/// runtime home — and it refuses the live home by construction rather than by
/// a policy check downstream.
///
/// Unlike every fixture door in this crate it is **not** compiled out of a
/// production build, because the campaign's claim is that the *shipped* binary
/// enrolled. What keeps that safe is that the only production constructor,
/// [`Self::for_isolated_enrollment`], reads the operator-controlled
/// environment itself and accepts nothing from its caller.
pub struct IsolatedHomeCanaryCapability {
    _private: (),
}

impl fmt::Debug for IsolatedHomeCanaryCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedHomeCanaryCapability")
            .field("kind", &"isolated-home-enrollment")
            .finish()
    }
}

impl IsolatedHomeCanaryCapability {
    /// The single explicit selector, mirroring
    /// [`LiveHomeCanaryCapability::for_explicit_enrollment`]'s shape exactly.
    ///
    /// Refusal D1: only the exact string `"1"` in `NANIKA_ISOLATED_HOME_ENROLL`
    /// enrolls. Absent, empty, `"0"` and `"true"` are all `None`, and a `None`
    /// here leaves the run `Unenrolled` and `orchestrator run` exiting
    /// `ExecutionNotEnrolled` exactly as it does today.
    ///
    /// Refusal D10: the two doors are mutually exclusive, never additive. If
    /// the live-home selector is set *at all* — whatever its value — this
    /// returns `None` rather than enrolling, so an operator can never end up
    /// holding both. The check lives here rather than in the CLI so that
    /// `crates/orchestrator-cli/src/` still names the live selector nowhere.
    #[must_use]
    pub fn for_isolated_enrollment() -> Option<Self> {
        if std::env::var_os("NANIKA_LIVE_HOME_ENROLL").is_some() {
            return None;
        }
        Self::for_isolated_enrollment_if(
            std::env::var("NANIKA_ISOLATED_HOME_ENROLL").ok().as_deref(),
        )
    }

    /// Pure variant of the selector for deterministic callers and tests, the
    /// crate-private sibling of
    /// [`LiveHomeCanaryCapability::for_explicit_enrollment_if`].
    ///
    /// It carries D1 only. D10 belongs to the reader above, because a caller
    /// that passes its own string is by definition not the process
    /// environment and has nothing to be mutually exclusive with.
    #[must_use]
    pub(crate) fn for_isolated_enrollment_if(value: Option<&str>) -> Option<Self> {
        (value == Some("1")).then_some(Self { _private: () })
    }
}

/// The ambient roots B5-DESIGN §7.3's D3 and D4 refuse overlap with.
///
/// The caller supplies them because they are seal 1's own inputs and re-reading
/// them here would be a second, divergent source of truth. A caller that lies
/// gains nothing: admission runs the unchanged
/// `fixture_authority::validate_policy_boundary`, which re-derives `$HOME` from
/// the process environment and requires the home to sit inside the process
/// temporary directory regardless of anything named here.
#[derive(Clone, Debug)]
pub struct IsolatedHomeGuards {
    /// The user home whose `.alluka` and `.via` the door refuses (D3).
    pub user_home: PathBuf,
    /// The repository checkout the door refuses (D4).
    pub repository_checkout: PathBuf,
}

/// A runtime home authorized for isolated-canary execution (B5-DESIGN §7).
///
/// It produces the `Arc<ProductionBoundary>` that seals 2-4, 9, 10 and 12 need
/// **without** `test-support`, which is the whole point of the door: the
/// fixture doors that produce the same value are compiled out of the shipped
/// binary, so a shipped binary could previously enroll nothing at all.
pub struct AuthorizedIsolatedRuntimeHome {
    canonical_path: PathBuf,
    directory: Dir,
}

/// A private, explicitly named Rust first-use pilot run home.
pub struct RustPilotRuntimeHome {
    _private: (),
}

impl fmt::Debug for RustPilotRuntimeHome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RustPilotRuntimeHome")
            .field("kind", &"rust-pilot")
            .finish()
    }
}

impl RustPilotRuntimeHome {
    /// Admit a fresh or exactly sealed private run directory and acquire its writer lease.
    pub fn acquire(
        path: &Path,
        guards: &IsolatedHomeGuards,
        binary_version: impl Into<String>,
    ) -> Result<crate::ProductionWriterAuthority, ApplicationError> {
        if std::env::var("NANIKA_RUST_FIRST_USE_PILOT").ok().as_deref() != Some("1")
            || std::env::var_os("NANIKA_LIVE_HOME_ENROLL").is_some()
            || std::env::var_os("NANIKA_ISOLATED_HOME_ENROLL").is_some()
        {
            return Err(ApplicationError::RustPilotNotEnabled);
        }
        let actual_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(ApplicationError::RustPilotAdmissionRefused)?;
        let target = resolve_for_policy(path)?;
        for root in [
            actual_home.join(".alluka"),
            actual_home.join(".via"),
            guards.user_home.join(".alluka"),
            guards.user_home.join(".via"),
            guards.repository_checkout.clone(),
        ] {
            if isolated_overlaps(&target, &resolve_for_policy(&root)?) {
                return Err(ApplicationError::RustPilotAdmissionRefused);
            }
        }
        let requested_metadata = std::fs::symlink_metadata(path).ok();
        if requested_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(ApplicationError::RustPilotAdmissionRefused);
        }
        let parent = target
            .parent()
            .ok_or(ApplicationError::RustPilotAdmissionRefused)?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
        let pd = open_canonical_directory(&parent)
            .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
        let directory = match std::fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = target
                    .file_name()
                    .ok_or(ApplicationError::RustPilotAdmissionRefused)?;
                crate::fs_util::create_dir_private(&pd, Path::new(name))
                    .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
                crate::fs_util::sync_dir(&pd)
                    .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
                open_dir_path_nofollow(&pd, Path::new(name))
                    .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?
            }
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                    return Err(ApplicationError::RustPilotAdmissionRefused);
                }
                open_canonical_directory(&target)
                    .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?
            }
            Err(_) => return Err(ApplicationError::RustPilotAdmissionRefused),
        };
        let root_identity = identity(
            &directory
                .dir_metadata()
                .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?,
        );
        let meta = directory
            .dir_metadata()
            .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
        if !crate::fixture_authority::is_private_fixture_directory(&meta) {
            return Err(ApplicationError::RustPilotAdmissionRefused);
        }
        let seal = Path::new("orchestrator.rust-pilot.seal");
        if crate::fs_util::read_bounded_nofollow(&directory, seal, 512)
            .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?
            .is_none()
        {
            let mut entries = directory
                .entries()
                .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?;
            if entries
                .next()
                .transpose()
                .map_err(|_| ApplicationError::RustPilotAdmissionRefused)?
                .is_some()
            {
                return Err(ApplicationError::RustPilotAdmissionRefused);
            }
        }
        crate::writer_authority::ProductionWriterAuthority::acquire_rust_pilot(
            target,
            directory,
            binary_version.into(),
            root_identity,
        )
        .map_err(|_| ApplicationError::RustPilotAdmissionRefused)
    }
}

impl fmt::Debug for AuthorizedIsolatedRuntimeHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted like every other authority in this module: the operator
        // chose this path, but a diagnostic is not the place to repeat it.
        formatter
            .debug_struct("AuthorizedIsolatedRuntimeHome")
            .field("kind", &"isolated-home-canary")
            .finish()
    }
}

impl AuthorizedIsolatedRuntimeHome {
    /// The authorized runtime home.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    /// Admits the vetted home and returns the authority seals 4, 5, 6 and 12
    /// are built from.
    ///
    /// `helper_bytes` are the bundled helper's exact bytes, already checked
    /// against the bundle manifest's digest by the caller (D7). Pinning them
    /// into the admission policy is what makes seal 5's
    /// `install_fixture_executable` refuse anything else.
    ///
    /// # Errors
    /// Returns [`ApplicationError::IsolatedHomeAdmissionRefused`] when the
    /// unchanged fixture admission policy refuses the home — which is what
    /// happens, deliberately, for any home outside the process temporary
    /// directory. Widening that is an owner decision (B5-DESIGN §7.5), not
    /// this door's.
    pub fn admit(
        &self,
        helper_bytes: &[u8],
        guards: &IsolatedHomeGuards,
    ) -> Result<FreshFixtureAuthority, ApplicationError> {
        let directory =
            self.directory
                .try_clone()
                .map_err(|source| ApplicationError::InspectRuntimePath {
                    path: self.canonical_path.clone(),
                    source,
                })?;
        let policy = crate::fixture_authority::FixtureAdmissionPolicy::new(
            guards.user_home.clone(),
            guards.repository_checkout.clone(),
            std::env::temp_dir(),
        )
        .with_expected_fixture_helper(helper_bytes);
        FreshFixtureAuthority::admit_isolated_home(&self.canonical_path, directory, &policy)
            .map_err(|_| ApplicationError::IsolatedHomeAdmissionRefused)
    }

    /// Seal 2's boundary, from the admitted authority.
    ///
    /// # Errors
    /// Returns [`ApplicationError::IsolatedHomeAdmissionRefused`] when the
    /// authority's own boundary no longer verifies.
    pub fn boundary(
        &self,
        authority: &FreshFixtureAuthority,
    ) -> Result<std::sync::Arc<ProductionBoundary>, ApplicationError> {
        let boundary = ProductionBoundary::from_fixture_authority(authority)
            .map_err(|_| ApplicationError::IsolatedHomeAdmissionRefused)?;
        Ok(std::sync::Arc::new(boundary))
    }

    /// Seal 3's store, opened under seal 2's boundary.
    ///
    /// [`StorageActorAuthority`](crate::StorageActorAuthority)'s constructor is
    /// crate-private, so this is the mint; it takes a boundary, never a path.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError`](crate::RuntimeStoreError) when the store
    /// cannot be opened or its schema validation fails.
    pub fn open_runtime_store(
        &self,
        boundary: std::sync::Arc<ProductionBoundary>,
    ) -> Result<crate::RuntimeStore, crate::RuntimeStoreError> {
        crate::RuntimeStore::open(boundary, crate::runtime_store::StorageActorAuthority::new())
    }

    /// Seal 5's resume path: re-admits the helper a previous process installed.
    ///
    /// `install_fixture_executable` is the atomic direction and stays the
    /// caller's first move; this is the recovery, never a pre-test. It exists
    /// as a production door because `recover_fixture_executable`'s `pub`
    /// wrapper is `test-support`-gated and the shipped binary must be able to
    /// resume a home it left behind. The pin is unchanged — the bytes must
    /// still equal the ones the admission policy recorded.
    ///
    /// # Errors
    /// Returns [`ApplicationError::IsolatedHomeAdmissionRefused`] when the
    /// installed helper is absent, altered, or not the pinned bytes.
    pub fn recover_helper(
        &self,
        authority: &FreshFixtureAuthority,
        label: &str,
        bytes: &[u8],
    ) -> Result<crate::ExecutableCapability, ApplicationError> {
        authority
            .recover_fixture_executable_inner(label, bytes)
            .map_err(|_| ApplicationError::IsolatedHomeAdmissionRefused)
    }

    /// Seal 10's metrics capability, under seal 2's boundary.
    ///
    /// # Errors
    /// Returns [`OwnerLeaseError`](crate::OwnerLeaseError) when another owner
    /// holds the lease.
    pub fn metrics_owner_capability(
        &self,
        boundary: std::sync::Arc<ProductionBoundary>,
    ) -> Result<crate::MetricsOwnerCapability, crate::OwnerLeaseError> {
        crate::MetricsOwnerCapability::in_isolated_boundary(boundary)
    }
}

/// B5-DESIGN §7.3's D7: the bundled helper's bytes must be the ones the bundle
/// manifest pinned at build time.
///
/// **The manifest, not the environment, is the source of truth.** An
/// env-supplied digest is an operator-controlled value, and an operator who
/// mistypes it would *silence* the check rather than fail it. So
/// `NANIKA_BUNDLED_HELPER_SHA256` is a cross-check only: when set it must be 64
/// lowercase hex and must equal the manifest's digest, which lets an operator
/// notice a bad bundle without giving them any way to disable the pin. A
/// missing or malformed manifest digest is a refusal, never a fallback to the
/// environment. B5-DESIGN §7.2 records the env-as-source alternative as
/// explicitly **not taken**; it is a real weakening and would need to be a
/// recorded owner decision.
///
/// # Errors
/// Returns [`ApplicationError::BundledHelperDigestMismatch`] naming both
/// digests whenever the manifest digest is absent, malformed, unequal to the
/// helper's bytes, or contradicted by the cross-check.
pub fn attest_bundled_helper(
    manifest_digest: &str,
    helper_bytes: &[u8],
    environment_cross_check: Option<&str>,
) -> Result<(), ApplicationError> {
    let observed = sha256_hex(helper_bytes);
    let refuse = |expected: &str| ApplicationError::BundledHelperDigestMismatch {
        expected: expected.to_owned(),
        observed: observed.clone(),
    };
    if !is_lowercase_sha256_hex(manifest_digest) {
        return Err(refuse(manifest_digest));
    }
    if manifest_digest != observed {
        return Err(refuse(manifest_digest));
    }
    // `Option::filter` rather than a `let`-chain: let-chains are unstable on the
    // 1.85 MSRV floor the evidence lane builds against.
    if let Some(cross_check) = environment_cross_check
        .filter(|candidate| !is_lowercase_sha256_hex(candidate) || *candidate != manifest_digest)
    {
        return Err(refuse(cross_check));
    }
    Ok(())
}

/// The digest `scripts/build-production-bundle.sh` records for the bundled
/// helper, and the one [`attest_bundled_helper`] compares against.
///
/// Public so the bundle builder and its gate compute the pin with the same
/// function the door checks it with; a second implementation would be a second
/// definition of "the helper's digest".
#[must_use]
pub fn bundled_helper_digest(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

fn is_lowercase_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(bytes);
    let rendered = digest.finalize();
    let mut hex = String::with_capacity(rendered.len() * 2);
    for byte in rendered {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    hex
}

/// Creates an absent isolated home as an exact mode-0700 directory.
///
/// The parent must already exist: this door creates the home, never the tree
/// above it, so a mistyped `ALLUKA_HOME` cannot scatter directories across the
/// filesystem.
fn create_isolated_home(target: &Path) -> Result<Dir, ApplicationError> {
    let parent = target
        .parent()
        .ok_or_else(|| ApplicationError::InspectRuntimePath {
            path: target.to_path_buf(),
            source: std::io::Error::other("the isolated home has no parent directory"),
        })?;
    let name = target
        .file_name()
        .ok_or_else(|| ApplicationError::InspectRuntimePath {
            path: target.to_path_buf(),
            source: std::io::Error::other("the isolated home has no final component"),
        })?;
    let parent_directory = open_canonical_directory(parent).map_err(|source| {
        ApplicationError::InspectRuntimePath {
            path: parent.to_path_buf(),
            source,
        }
    })?;
    crate::fs_util::create_dir_private(&parent_directory, Path::new(name)).map_err(|source| {
        ApplicationError::PrepareRuntimeHome {
            path: target.to_path_buf(),
            source,
        }
    })?;
    crate::fs_util::sync_dir(&parent_directory).map_err(|source| {
        ApplicationError::PrepareRuntimeHome {
            path: target.to_path_buf(),
            source,
        }
    })?;
    crate::fs_util::open_dir_path_nofollow(&parent_directory, Path::new(name)).map_err(|source| {
        ApplicationError::InspectRuntimePath {
            path: target.to_path_buf(),
            source,
        }
    })
}

/// D6's predicate: empty, or already this crate's own admitted root.
///
/// The marker is the v2 authority marker fixture admission writes, so "a prior
/// isolated run" is proven by the same artifact `recover_existing` verifies
/// rigorously a moment later — not by a name this door invented.
fn isolated_home_is_adoptable(directory: &Dir) -> std::io::Result<bool> {
    let mut empty = true;
    for entry in directory.entries()? {
        let entry = entry?;
        empty = false;
        if entry.file_name().as_os_str()
            == std::ffi::OsStr::new(crate::fixture_authority::AUTHORITY_MARKER)
        {
            return Ok(true);
        }
    }
    Ok(empty)
}

/// D3/D4's overlap relation, the same one
/// `fixture_authority::validate_policy_boundary` uses: equality, containment,
/// and reverse containment all count.
fn isolated_overlaps(home: &Path, forbidden: &Path) -> bool {
    home == forbidden || home.starts_with(forbidden) || forbidden.starts_with(home)
}

/// Fixture-root input for fresh admission or recovery.
///
/// This type carries no mutation authority. Only [`FreshFixtureAuthority`]
/// returned by fixture admission can authorize a runtime home.
pub struct IsolatedFixtureRoot {
    canonical_path: PathBuf,
    directory: Dir,
    freshly_created: bool,
}

impl fmt::Debug for IsolatedFixtureRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IsolatedFixtureRoot")
            .field("kind", &"isolated-fixture-root")
            .finish()
    }
}

impl IsolatedFixtureRoot {
    /// Identifies an existing fixture root without creating or modifying it.
    pub fn identify(path: &Path) -> Result<Self, ApplicationError> {
        let canonical_path =
            std::fs::canonicalize(path).map_err(|source| ApplicationError::InspectRuntimePath {
                path: path.to_path_buf(),
                source,
            })?;
        let directory = open_canonical_directory(&canonical_path).map_err(|source| {
            ApplicationError::InspectRuntimePath {
                path: canonical_path.clone(),
                source,
            }
        })?;
        Ok(Self {
            canonical_path,
            directory,
            freshly_created: false,
        })
    }

    /// Atomically creates a mode-0700 fixture root beneath an existing harness directory.
    pub fn create_fresh(parent: &Path) -> Result<Self, ApplicationError> {
        let canonical_parent = std::fs::canonicalize(parent).map_err(|source| {
            ApplicationError::InspectRuntimePath {
                path: parent.to_path_buf(),
                source,
            }
        })?;
        let parent_directory = open_canonical_directory(&canonical_parent).map_err(|source| {
            ApplicationError::InspectRuntimePath {
                path: canonical_parent.clone(),
                source,
            }
        })?;
        let random = RandomState::new();
        let counter = FIXTURE_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "fixture-{:016x}",
            random.hash_one((std::process::id(), counter))
        );
        crate::fs_util::create_dir_private(&parent_directory, Path::new(&name)).map_err(
            |source| ApplicationError::PrepareRuntimeHome {
                path: canonical_parent.join(&name),
                source,
            },
        )?;
        crate::fs_util::sync_dir(&parent_directory).map_err(|source| {
            ApplicationError::PrepareRuntimeHome {
                path: canonical_parent.clone(),
                source,
            }
        })?;
        let directory = crate::fs_util::open_dir_path_nofollow(&parent_directory, Path::new(&name))
            .map_err(|source| ApplicationError::InspectRuntimePath {
                path: canonical_parent.join(&name),
                source,
            })?;
        Ok(Self {
            canonical_path: canonical_parent.join(name),
            directory,
            freshly_created: true,
        })
    }

    /// Returns the canonical fixture root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    /// Whether this root was atomically created by [`Self::create_fresh`]
    /// rather than adopted by [`Self::identify`].
    ///
    /// Crate-private: provenance is an admission fact, not something an
    /// out-of-crate caller should be able to read or assert. Capability mints
    /// that may only run against a disposable root consult it — `identify`
    /// names *any* existing directory, a live home included, so the path alone
    /// carries no proof the caller owns what it points at.
    pub(crate) const fn freshly_created(&self) -> bool {
        self.freshly_created
    }

    pub(crate) fn into_boundary(self) -> (PathBuf, Dir, bool) {
        (self.canonical_path, self.directory, self.freshly_created)
    }
}

/// Resolved runtime home. Resolution itself grants no write access.
#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedRuntimeHome {
    path: PathBuf,
    selection: HomeSelection,
}

impl fmt::Debug for ResolvedRuntimeHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeHome")
            .field("selection", &self.selection)
            .finish()
    }
}

impl ResolvedRuntimeHome {
    /// Returns the selected path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the precedence rule that selected this path.
    #[must_use]
    pub const fn selection(&self) -> HomeSelection {
        self.selection
    }

    /// Authorizes a fixture-bounded mutation under an admitted fixture authority.
    ///
    /// A raw [`IsolatedFixtureRoot`] is deliberately insufficient. Admission
    /// creates the sealed boundary retained by the returned capability.
    pub fn authorize_fixture(
        self,
        fixture: &FreshFixtureAuthority,
    ) -> Result<AuthorizedFixtureRuntimeHome, ApplicationError> {
        fixture
            .boundary
            .verify()
            .map_err(|_| ApplicationError::FixtureRuntimeHomeAuthorityUnavailable)?;
        let fixture_path = fixture.boundary.canonical_path();
        let target = resolve_for_policy(&self.path)?;
        if !target.starts_with(fixture_path) {
            return Err(ApplicationError::RuntimeHomeOutsideFixture {
                path: target,
                fixture_root: fixture_path.to_path_buf(),
            });
        }
        let relative = target
            .strip_prefix(fixture_path)
            .map_err(|_| ApplicationError::RuntimeHomeOutsideFixture {
                path: target.clone(),
                fixture_root: fixture_path.to_path_buf(),
            })?
            .to_path_buf();
        let directory = fixture.boundary.directory().try_clone().map_err(|source| {
            ApplicationError::InspectRuntimePath {
                path: fixture_path.to_path_buf(),
                source,
            }
        })?;
        fixture
            .boundary
            .verify()
            .map_err(|_| ApplicationError::FixtureRuntimeHomeAuthorityUnavailable)?;
        Ok(AuthorizedFixtureRuntimeHome {
            inner: AuthorizedRuntimeHome {
                path: target,
                directory,
                relative,
            },
            boundary: std::sync::Arc::clone(&fixture.boundary),
        })
    }

    /// Authorizes an isolated-canary mutation under this runtime home
    /// (B5-DESIGN §7).
    ///
    /// The third arm beside [`Self::authorize_fixture`] and
    /// [`Self::authorize_production`]. It runs refusals D2 through D6 and
    /// **fails closed before any mutation**: nothing is created until every one
    /// of them has passed, and a home that already exists is only ever
    /// inspected. The one write this function performs is creating the home
    /// itself when it is absent, which is the effect the operator asked for.
    ///
    /// D1 is the capability's own constructor and has already happened by the
    /// time this is called. D7 — the bundled helper's manifest digest — belongs
    /// to the caller and must also already have happened, because it is a check
    /// on the *bundle*, not on the home; the caller proves it by having the
    /// helper bytes to pass to [`AuthorizedIsolatedRuntimeHome::admit`].
    ///
    /// # Errors
    /// Returns the named refusal for each of D2-D6, and
    /// [`ApplicationError::InspectRuntimePath`] or
    /// [`ApplicationError::PrepareRuntimeHome`] when the filesystem itself
    /// fails.
    pub fn authorize_isolated(
        self,
        _enrollment: &IsolatedHomeCanaryCapability,
        guards: &IsolatedHomeGuards,
    ) -> Result<AuthorizedIsolatedRuntimeHome, ApplicationError> {
        // D2 — the home must have been named explicitly. Every other
        // precedence rule reaches a home the operator did not choose for this
        // run, `~/.alluka` included.
        if self.selection != HomeSelection::AllukaHome {
            return Err(ApplicationError::IsolatedHomeNotExplicit {
                selection: self.selection,
            });
        }
        let target = resolve_for_policy(&self.path)?;

        // D3 — the live homes, by the same overlap relation the fixture policy
        // uses: equal, contained, or containing.
        for (root, class) in [
            (guards.user_home.join(".alluka"), "live-alluka"),
            (guards.user_home.join(".via"), "live-via"),
        ] {
            let root = resolve_for_policy(&root)?;
            if isolated_overlaps(&target, &root) {
                return Err(ApplicationError::IsolatedHomeOverlapsLiveHome { class });
            }
        }

        // D4 — the repository checkout.
        let checkout = resolve_for_policy(&guards.repository_checkout)?;
        if isolated_overlaps(&target, &checkout) {
            return Err(ApplicationError::IsolatedHomeOverlapsCheckout);
        }

        // D5/D6 — the directory itself, when it already exists. An absent home
        // is created private and is fresh by construction.
        let directory = match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_isolated_home(&target)?
            }
            Err(source) => {
                return Err(ApplicationError::InspectRuntimePath {
                    path: target,
                    source,
                });
            }
            Ok(_) => {
                let directory = open_canonical_directory(&target).map_err(|source| {
                    ApplicationError::InspectRuntimePath {
                        path: target.clone(),
                        source,
                    }
                })?;
                let metadata = directory.dir_metadata().map_err(|source| {
                    ApplicationError::InspectRuntimePath {
                        path: target.clone(),
                        source,
                    }
                })?;
                // D5 — `is_private_fixture_directory` reused verbatim, so this
                // door and fixture admission cannot drift on what "private"
                // means.
                if !crate::fixture_authority::is_private_fixture_directory(&metadata) {
                    return Err(ApplicationError::IsolatedHomeNotPrivate);
                }
                // D6 — empty, or already carrying this crate's own authority
                // marker from a prior isolated run. Anything else is content
                // this binary did not create, and adopting it would mean
                // writing a runtime home over somebody else's directory.
                if !isolated_home_is_adoptable(&directory).map_err(|source| {
                    ApplicationError::InspectRuntimePath {
                        path: target.clone(),
                        source,
                    }
                })? {
                    return Err(ApplicationError::IsolatedHomeNotFresh);
                }
                directory
            }
        };

        Ok(AuthorizedIsolatedRuntimeHome {
            canonical_path: target,
            directory,
        })
    }

    /// Authorizes a production mutation under this runtime home.
    ///
    /// The capability is mandatory for every production path. There is no
    /// production constructor that accepts caller-supplied home identity or an
    /// optional enrollment marker.
    pub fn authorize_production(
        self,
        _enrollment: &LiveHomeCanaryCapability,
    ) -> Result<AuthorizedProductionRuntimeHome, ApplicationError> {
        let target = resolve_for_policy(&self.path)
            .map_err(|_| ApplicationError::ProductionRuntimeHomeAuthorizationFailed)?;
        let (directory, relative) = capability_boundary(&target)
            .map_err(|_| ApplicationError::ProductionRuntimeHomeAuthorizationFailed)?;
        Ok(AuthorizedProductionRuntimeHome {
            inner: AuthorizedRuntimeHome {
                path: target,
                directory,
                relative,
            },
            prepared: OnceLock::new(),
            prepare_lock: Mutex::new(()),
        })
    }
}

/// Shared implementation hidden behind the two authorization states.
struct AuthorizedRuntimeHome {
    path: PathBuf,
    directory: Dir,
    relative: PathBuf,
}

impl AuthorizedRuntimeHome {
    fn prepare_fixture(&self) -> Result<(), ApplicationError> {
        prepare_private_directory_beneath(&self.directory, &self.relative).map_err(|source| {
            ApplicationError::PrepareRuntimeHome {
                path: self.path.clone(),
                source,
            }
        })
    }

    fn open_target(&self) -> std::io::Result<Dir> {
        open_directory_beneath(&self.directory, &self.relative)
    }
}

/// A runtime home authorized only for production execution.
pub struct AuthorizedProductionRuntimeHome {
    inner: AuthorizedRuntimeHome,
    prepared: OnceLock<PreparedProductionRoot>,
    prepare_lock: Mutex<()>,
}

struct PreparedProductionRoot {
    directory: Dir,
    identity: FileIdentity,
}

impl fmt::Debug for AuthorizedProductionRuntimeHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedProductionRuntimeHome")
            .field("kind", &"enrolled-production")
            .finish()
    }
}

impl AuthorizedProductionRuntimeHome {
    /// Returns the authorized root path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub(crate) fn begin_writer_preparation(
        &self,
    ) -> Result<ProductionWriterPreparation<'_>, ApplicationError> {
        let guard = self
            .prepare_lock
            .lock()
            .map_err(|_| ApplicationError::ProductionRuntimeHomeInitializationUnavailable)?;
        Ok(ProductionWriterPreparation {
            authorized: self,
            _guard: guard,
        })
    }

    /// Opens an already-existing production root without creating or changing it.
    ///
    /// This is crate-private so the legacy-writer reconciler can bind its proof
    /// to a retained directory capability before any production mutation is
    /// possible. A missing root remains a hard failure in this enrollment slice.
    pub(crate) fn inspect_existing_root(&self) -> Result<(Dir, FileIdentity), ApplicationError> {
        let directory = self.inner.open_target().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "open existing home for legacy-writer inspection",
                source,
            }
        })?;
        validate_writer_candidate_directory(&directory).map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "validate existing legacy-writer home",
                source,
            }
        })?;
        let root_identity = identity(&directory.dir_metadata().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "inspect existing legacy-writer home",
                source,
            }
        })?);
        self.verify_candidate_root(root_identity)?;
        Ok((directory, root_identity))
    }

    #[cfg(test)]
    pub(crate) fn prepare(&self) -> Result<(), ApplicationError> {
        let preparation = self.begin_writer_preparation()?;
        let (directory, identity) = preparation.candidate()?;
        preparation.commit(directory, identity)
    }

    fn prepared_root(&self) -> Result<&PreparedProductionRoot, ApplicationError> {
        self.prepared
            .get()
            .ok_or(ApplicationError::ProductionRuntimeHomeNotPrepared)
    }

    fn verify_candidate_root(&self, expected: FileIdentity) -> Result<(), ApplicationError> {
        let bounded = self.inner.open_target().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "reopen prepared directory",
                source,
            }
        })?;
        let bounded_identity = identity(&bounded.dir_metadata().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "inspect reopened directory",
                source,
            }
        })?);
        let path_mapping = open_canonical_directory(&self.inner.path).map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "verify canonical directory mapping",
                source,
            }
        })?;
        let path_identity = identity(&path_mapping.dir_metadata().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "inspect canonical directory mapping",
                source,
            }
        })?);
        if bounded_identity != expected || path_identity != expected {
            return Err(ApplicationError::RuntimeHomeIdentityChanged);
        }
        Ok(())
    }

    fn verify_prepared_root(&self) -> Result<(), ApplicationError> {
        self.verify_candidate_root(self.prepared_root()?.identity)
    }
}

pub(crate) struct ProductionWriterPreparation<'a> {
    authorized: &'a AuthorizedProductionRuntimeHome,
    _guard: MutexGuard<'a, ()>,
}

impl ProductionWriterPreparation<'_> {
    pub(crate) fn existing_candidate(
        &self,
        expected: FileIdentity,
    ) -> Result<(Dir, FileIdentity), ApplicationError> {
        if let Some(prepared) = self.authorized.prepared.get() {
            if prepared.identity != expected {
                return Err(ApplicationError::RuntimeHomeIdentityChanged);
            }
            self.authorized.verify_candidate_root(expected)?;
            let directory = prepared.directory.try_clone().map_err(|source| {
                ApplicationError::ProductionRuntimeHomeOperation {
                    operation: "clone existing writer directory",
                    source,
                }
            })?;
            validate_writer_candidate_directory(&directory).map_err(|source| {
                ApplicationError::ProductionRuntimeHomeOperation {
                    operation: "validate existing writer directory",
                    source,
                }
            })?;
            return Ok((directory, expected));
        }

        let (directory, actual) = self.authorized.inspect_existing_root()?;
        if actual != expected {
            return Err(ApplicationError::RuntimeHomeIdentityChanged);
        }
        Ok((directory, actual))
    }

    #[cfg(test)]
    pub(crate) fn candidate(&self) -> Result<(Dir, FileIdentity), ApplicationError> {
        if let Some(prepared) = self.authorized.prepared.get() {
            self.authorized.verify_candidate_root(prepared.identity)?;
            let directory = prepared.directory.try_clone().map_err(|source| {
                ApplicationError::ProductionRuntimeHomeOperation {
                    operation: "clone prepared writer directory",
                    source,
                }
            })?;
            validate_writer_candidate_directory(&directory).map_err(|source| {
                ApplicationError::ProductionRuntimeHomeOperation {
                    operation: "validate prepared writer-lock directory",
                    source,
                }
            })?;
            return Ok((directory, prepared.identity));
        }

        provision_directory_beneath(
            &self.authorized.inner.directory,
            &self.authorized.inner.relative,
        )
        .map_err(|source| ApplicationError::ProductionRuntimeHomeOperation {
            operation: "provision writer-lock directory",
            source,
        })?;
        let directory = self.authorized.inner.open_target().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "open writer-lock directory",
                source,
            }
        })?;
        validate_writer_candidate_directory(&directory).map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "validate writer-lock directory",
                source,
            }
        })?;
        let identity = identity(&directory.dir_metadata().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "inspect writer-lock directory",
                source,
            }
        })?);
        self.authorized.verify_candidate_root(identity)?;
        Ok((directory, identity))
    }

    pub(crate) fn commit(
        &self,
        directory: Dir,
        expected: FileIdentity,
    ) -> Result<(), ApplicationError> {
        make_directory_private(&directory).map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "seal writer-owned directory",
                source,
            }
        })?;
        self.authorized.verify_candidate_root(expected)?;
        if let Some(prepared) = self.authorized.prepared.get() {
            if prepared.identity != expected {
                return Err(ApplicationError::RuntimeHomeIdentityChanged);
            }
            return Ok(());
        }
        self.authorized
            .prepared
            .set(PreparedProductionRoot {
                directory,
                identity: expected,
            })
            .map_err(|_| ApplicationError::ProductionRuntimeHomeInitializationUnavailable)
    }
}

/// A runtime home authorized only beneath an isolated fixture root.
pub struct AuthorizedFixtureRuntimeHome {
    inner: AuthorizedRuntimeHome,
    boundary: SharedCapabilityRoot,
}

impl fmt::Debug for AuthorizedFixtureRuntimeHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedFixtureRuntimeHome")
            .field("kind", &"isolated-fixture")
            .finish()
    }
}

impl AuthorizedFixtureRuntimeHome {
    /// Returns the authorized root path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Creates the authorized root directory.
    pub fn prepare(&self) -> Result<(), ApplicationError> {
        self.verify_boundary()?;
        self.inner.prepare_fixture()?;
        self.verify_boundary()
    }

    pub(crate) fn verify_boundary(&self) -> Result<(), ApplicationError> {
        self.boundary
            .verify()
            .map_err(|_| ApplicationError::FixtureRuntimeHomeAuthorityUnavailable)
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
struct HermeticCanaryLayout {
    fixture_parent: SharedCapabilityRoot,
    extras: Arc<crate::fixture_authority::FixtureExtras>,
    manifest: HermeticCanaryLayoutManifest,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
impl HermeticCanaryLayout {
    fn verify(&self) -> Result<(), CapabilityError> {
        match verify_hermetic_canary_layout_manifest(&self.fixture_parent, &self.manifest) {
            Ok(()) => Ok(()),
            Err(FixtureAuthorityError::Io(source)) => Err(CapabilityError::Io(source)),
            Err(_) => Err(CapabilityError::IdentityChanged),
        }
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
impl fmt::Debug for HermeticCanaryLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticCanaryLayout")
            .field("kind", &"sealed-sibling-layout")
            .finish()
    }
}

/// Capability root for a live production home or a disposable verification canary.
///
/// Live homes are admitted only by a retained [`crate::ProductionWriterAuthority`]
/// after explicit enrollment and writer-lock acquisition. The private,
/// feature-gated canary path instead consumes both fixed fixture targets
/// together and retains their fixture lock plus versioned identity manifest;
/// it cannot mint or overlap a live home.
pub struct ProductionBoundary {
    canonical_path: PathBuf,
    directory: Dir,
    identity: FileIdentity,
    writer_lease: ProductionBoundaryLease,
    projection_leases: Mutex<BTreeSet<FileIdentity>>,
}

enum ProductionBoundaryLease {
    Writer(Arc<WriterLeaseGuard>),
    PilotWriter(Arc<WriterLeaseGuard>),
    Fixture {
        boundary: SharedCapabilityRoot,
        extras: Arc<crate::fixture_authority::FixtureExtras>,
    },
    FixtureTarget(TargetRootAuthority),
    #[cfg(all(unix, feature = "verification-process-canary"))]
    HermeticCanary(Arc<HermeticCanaryLayout>),
    #[cfg(any(test, feature = "test-support"))]
    TestOnly,
}

impl ProductionBoundaryLease {
    fn verify(&self, directory: &Dir) -> Result<(), CapabilityError> {
        match self {
            Self::Writer(lease) | Self::PilotWriter(lease) => {
                lease.verify(directory).map_err(CapabilityError::from)
            }
            Self::Fixture { boundary, .. } => {
                boundary.verify()?;
                if identity(&directory.dir_metadata()?)
                    != identity(&boundary.directory().dir_metadata()?)
                {
                    return Err(CapabilityError::IdentityChanged);
                }
                Ok(())
            }
            Self::FixtureTarget(target) => target.verify().map_err(|error| match error {
                crate::FixtureAuthorityError::Io(source) => CapabilityError::Io(source),
                _ => CapabilityError::IdentityChanged,
            }),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            Self::HermeticCanary(layout) => layout.verify(),
            #[cfg(any(test, feature = "test-support"))]
            Self::TestOnly => Ok(()),
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Writer(_) => "enrolled-production",
            Self::PilotWriter(_) => "rust-pilot",
            Self::Fixture { .. } => "isolated-fixture",
            Self::FixtureTarget(_) => "isolated-fixture-target",
            #[cfg(all(unix, feature = "verification-process-canary"))]
            Self::HermeticCanary(_) => "hermetic-canary",
            #[cfg(any(test, feature = "test-support"))]
            Self::TestOnly => "enrolled-production",
        }
    }
}

impl fmt::Debug for ProductionBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: never reveal the canonical home path, directory handle, or
        // filesystem identity in diagnostics, events, or logs.
        formatter
            .debug_struct("ProductionBoundary")
            .field("kind", &self.writer_lease.kind())
            .finish()
    }
}

impl ProductionBoundary {
    /// Revalidates the retained boundary lease and canonical runtime-home root.
    ///
    /// The raw directory capability deliberately remains crate-private.
    pub fn verify(&self) -> Result<(), CapabilityError> {
        <Self as CapabilityRoot>::verify(self)
    }

    pub(crate) fn is_rust_pilot_writer(&self) -> bool {
        matches!(self.writer_lease, ProductionBoundaryLease::PilotWriter(_))
    }

    /// Derives a projection boundary while retaining the exact admitted
    /// fixture authority that grants it.
    ///
    /// This crate-private constructor deliberately accepts no raw path and
    /// cannot mint production writer authority.
    pub(crate) fn from_fixture_authority(
        authority: &FreshFixtureAuthority,
    ) -> Result<Self, CapabilityError> {
        authority.boundary.verify()?;
        let directory = authority.boundary.directory().try_clone()?;
        let boundary = Self {
            canonical_path: authority.boundary.canonical_path().to_path_buf(),
            identity: identity(&directory.dir_metadata()?),
            directory,
            writer_lease: ProductionBoundaryLease::Fixture {
                boundary: Arc::clone(&authority.boundary),
                extras: Arc::clone(&authority.extras),
            },
            projection_leases: Mutex::new(BTreeSet::new()),
        };
        boundary.verify()?;
        Ok(boundary)
    }

    /// Derives a private store boundary from one exact fixture-local target.
    ///
    /// The target capability is retained for the boundary's full lifetime;
    /// no raw path can enter this constructor.
    pub(crate) fn from_fixture_target(
        target: TargetRootAuthority,
    ) -> Result<Self, crate::FixtureAuthorityError> {
        target.verify()?;
        let canonical_path = target
            .boundary
            .canonical_path()
            .join("targets")
            .join(&target.id);
        let directory = target.directory.try_clone()?;
        let boundary = Self {
            canonical_path,
            identity: identity(&directory.dir_metadata()?),
            directory,
            writer_lease: ProductionBoundaryLease::FixtureTarget(target),
            projection_leases: Mutex::new(BTreeSet::new()),
        };
        boundary.verify()?;
        Ok(boundary)
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn from_hermetic_canary_targets(
        compatibility: TargetRootAuthority,
        private_ledger: TargetRootAuthority,
    ) -> Result<(Self, Self), FixtureAuthorityError> {
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
        validate_private_directory(&compatibility.targets_directory)?;
        validate_private_directory(&private_ledger.targets_directory)?;
        validate_private_directory(&compatibility.directory)?;
        validate_private_directory(&private_ledger.directory)?;
        let manifest = admit_hermetic_canary_layout_manifest(&compatibility, &private_ledger)?;

        let fixture_parent = Arc::clone(&compatibility.boundary);
        let extras = Arc::clone(&compatibility.extras);
        let compatibility_identity = compatibility.identity;
        let private_ledger_identity = private_ledger.identity;
        let compatibility_path = fixture_parent
            .canonical_path()
            .join("targets")
            .join(HERMETIC_CANARY_COMPAT_TARGET);
        let private_ledger_path = fixture_parent
            .canonical_path()
            .join("targets")
            .join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET);
        let layout = Arc::new(HermeticCanaryLayout {
            fixture_parent,
            extras,
            manifest,
        });
        layout.verify()?;

        let compatibility_boundary = Self {
            canonical_path: compatibility_path,
            directory: compatibility.directory,
            identity: compatibility_identity,
            writer_lease: ProductionBoundaryLease::HermeticCanary(Arc::clone(&layout)),
            projection_leases: Mutex::new(BTreeSet::new()),
        };
        let private_ledger_boundary = Self {
            canonical_path: private_ledger_path,
            directory: private_ledger.directory,
            identity: private_ledger_identity,
            writer_lease: ProductionBoundaryLease::HermeticCanary(layout),
            projection_leases: Mutex::new(BTreeSet::new()),
        };
        compatibility_boundary.verify()?;
        private_ledger_boundary.verify()?;
        Ok((compatibility_boundary, private_ledger_boundary))
    }

    pub(crate) fn from_writer_lease(
        authorized: &AuthorizedProductionRuntimeHome,
        writer_lease: Arc<WriterLeaseGuard>,
    ) -> Result<Self, ApplicationError> {
        authorized.verify_prepared_root()?;
        let prepared = authorized.prepared_root()?;
        let directory = prepared.directory.try_clone().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "clone prepared directory capability",
                source,
            }
        })?;
        Ok(Self {
            canonical_path: authorized.path().to_path_buf(),
            directory,
            identity: prepared.identity,
            writer_lease: ProductionBoundaryLease::Writer(writer_lease),
            projection_leases: Mutex::new(BTreeSet::new()),
        })
    }

    pub(crate) fn from_pilot_lease(
        canonical_path: PathBuf,
        directory: Dir,
        lease: Arc<WriterLeaseGuard>,
    ) -> Result<Self, ApplicationError> {
        let metadata =
            directory
                .dir_metadata()
                .map_err(|source| ApplicationError::InspectRuntimePath {
                    path: canonical_path.clone(),
                    source,
                })?;
        let identity = identity(&metadata);
        Ok(Self {
            canonical_path,
            directory,
            identity,
            writer_lease: ProductionBoundaryLease::PilotWriter(lease),
            projection_leases: Mutex::new(BTreeSet::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_authorized_home(
        authorized: &AuthorizedProductionRuntimeHome,
    ) -> Result<Self, ApplicationError> {
        authorized.verify_prepared_root()?;
        let prepared = authorized.prepared_root()?;
        let directory = prepared.directory.try_clone().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "clone prepared test directory capability",
                source,
            }
        })?;
        Ok(Self {
            canonical_path: authorized.path().to_path_buf(),
            directory,
            identity: prepared.identity,
            writer_lease: ProductionBoundaryLease::TestOnly,
            projection_leases: Mutex::new(BTreeSet::new()),
        })
    }

    /// Opens a canonical directory and wraps it as a production boundary for
    /// crate-internal tests.
    ///
    /// This is the single test-only constructor that reads a canonical root and
    /// records its identity. Production code cannot call this constructor.
    ///
    /// Gated the same way as the rest of this crate's fixture-only surface
    /// ([`crate::JournalCommit::for_fixture`]): the `test-support` feature
    /// unifies it into the library that out-of-crate integration tests link
    /// against, so a gate can drive a real store without any production
    /// consumer gaining a path-taking constructor.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn from_canonical_root(path: &Path) -> Result<Self, ApplicationError> {
        let canonical_path = path.to_path_buf();
        let directory = open_canonical_directory(&canonical_path).map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "open canonical production root",
                source,
            }
        })?;
        let identity = identity(&directory.dir_metadata().map_err(|source| {
            ApplicationError::ProductionRuntimeHomeOperation {
                operation: "inspect canonical production root",
                source,
            }
        })?);
        Ok(Self {
            canonical_path,
            directory,
            identity,
            writer_lease: ProductionBoundaryLease::TestOnly,
            projection_leases: Mutex::new(BTreeSet::new()),
        })
    }

    pub(crate) fn acquire_projection_lease(
        &self,
        workspace: FileIdentity,
    ) -> Result<bool, CapabilityError> {
        self.verify()?;
        let registry = match &self.writer_lease {
            ProductionBoundaryLease::Fixture { extras, .. } => &extras.projection_leases,
            ProductionBoundaryLease::FixtureTarget(target) => &target.extras.projection_leases,
            _ => &self.projection_leases,
        };
        let mut leases = match registry.lock() {
            Ok(leases) => leases,
            Err(poisoned) => poisoned.into_inner(),
        };
        Ok(leases.insert(workspace))
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn hermetic_canary_extras(
        &self,
    ) -> Result<Arc<crate::fixture_authority::FixtureExtras>, CapabilityError> {
        self.verify()?;
        match &self.writer_lease {
            ProductionBoundaryLease::HermeticCanary(layout) => Ok(Arc::clone(&layout.extras)),
            _ => Err(CapabilityError::IdentityChanged),
        }
    }

    pub(crate) fn release_projection_lease(&self, workspace: FileIdentity) {
        let registry = match &self.writer_lease {
            ProductionBoundaryLease::Fixture { extras, .. } => &extras.projection_leases,
            ProductionBoundaryLease::FixtureTarget(target) => &target.extras.projection_leases,
            _ => &self.projection_leases,
        };
        let mut leases = match registry.lock() {
            Ok(leases) => leases,
            Err(poisoned) => poisoned.into_inner(),
        };
        leases.remove(&workspace);
    }
}

impl Sealed for ProductionBoundary {}

impl CapabilityRoot for ProductionBoundary {
    fn verify(&self) -> Result<(), CapabilityError> {
        validate_private_directory(&self.directory)?;
        self.writer_lease.verify(&self.directory)?;
        let reopened = open_canonical_directory(&self.canonical_path)?;
        validate_private_directory(&reopened)?;
        if identity(&reopened.dir_metadata()?) != self.identity {
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

#[cfg(all(test, unix))]
fn provision_directory_beneath(directory: &Dir, relative: &Path) -> std::io::Result<()> {
    use cap_std::fs::{DirBuilder, DirBuilderExt, Permissions, PermissionsExt};
    use std::path::Component;

    let mut current = directory.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "writer-home path contains a non-normal component",
            ));
        };
        let name = Path::new(name);
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        let created = match current.create_dir_with(name, &builder) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error),
        };
        if created {
            crate::fs_util::sync_dir(&current)?;
        }
        current = crate::fs_util::open_dir_path_nofollow(&current, name)?;
        if created {
            current.set_permissions(".", Permissions::from_mode(0o700))?;
        }
        validate_private_directory(&current)?;
        if created {
            crate::fs_util::sync_dir(&current)?;
        }
    }
    Ok(())
}

#[cfg(all(test, not(unix)))]
fn provision_directory_beneath(directory: &Dir, relative: &Path) -> std::io::Result<()> {
    use std::path::Component;

    let mut current = directory.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "writer-home path contains a non-normal component",
            ));
        };
        let name = Path::new(name);
        let created = match current.create_dir(name) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error),
        };
        if created {
            crate::fs_util::sync_dir(&current)?;
        }
        current = crate::fs_util::open_dir_path_nofollow(&current, name)?;
        validate_private_directory(&current)?;
        if created {
            crate::fs_util::sync_dir(&current)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_directory_private(directory: &Dir) -> std::io::Result<()> {
    use cap_std::fs::{Permissions, PermissionsExt};

    directory.set_permissions(".", Permissions::from_mode(0o700))?;
    validate_private_directory(directory)?;
    directory.try_clone()?.into_std_file().sync_all()
}

#[cfg(not(unix))]
fn make_directory_private(directory: &Dir) -> std::io::Result<()> {
    directory.try_clone()?.into_std_file().sync_all()
}

#[cfg(unix)]
fn validate_private_directory(directory: &Dir) -> std::io::Result<()> {
    use cap_std::fs::MetadataExt;

    let metadata = directory.dir_metadata()?;
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "writer-owned directory mode, type, or owner is unsafe",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_writer_candidate_directory(directory: &Dir) -> std::io::Result<()> {
    use cap_std::fs::MetadataExt;

    let metadata = directory.dir_metadata()?;
    let raw_mode = metadata.mode() & 0o7777;
    if !metadata.is_dir()
        || raw_mode & 0o7022 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "writer candidate directory is not safely owner-controlled",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_directory(directory: &Dir) -> std::io::Result<()> {
    if directory.dir_metadata()?.is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "writer-owned runtime home is not a directory",
        ))
    }
}

#[cfg(not(unix))]
fn validate_writer_candidate_directory(directory: &Dir) -> std::io::Result<()> {
    validate_private_directory(directory)
}

/// Compatibility-aware runtime-home resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeHomeResolver;

impl RuntimeHomeResolver {
    /// Resolves the documented home precedence without canonicalizing explicit paths.
    pub fn resolve(
        inputs: &HomeInputs,
        probe: &impl DirectoryProbe,
    ) -> Result<ResolvedRuntimeHome, ApplicationError> {
        ensure_not_empty("HOME", &inputs.user_home)?;

        let (path, selection) = if let Some(path) = &inputs.orchestrator_config_dir {
            ensure_not_empty("ORCHESTRATOR_CONFIG_DIR", path)?;
            (path.clone(), HomeSelection::OrchestratorConfigDir)
        } else if let Some(path) = &inputs.alluka_home {
            ensure_not_empty("ALLUKA_HOME", path)?;
            (path.clone(), HomeSelection::AllukaHome)
        } else if let Some(path) = &inputs.via_home {
            ensure_not_empty("VIA_HOME", path)?;
            (path.join("orchestrator"), HomeSelection::ViaHome)
        } else {
            let alluka = inputs.user_home.join(".alluka");
            if probe.exists(&alluka) {
                (alluka, HomeSelection::ExistingAlluka)
            } else {
                (inputs.user_home.join(".via"), HomeSelection::ViaFallback)
            }
        };

        Ok(ResolvedRuntimeHome { path, selection })
    }
}

fn ensure_not_empty(name: &'static str, path: &Path) -> Result<(), ApplicationError> {
    if path.as_os_str().is_empty() {
        Err(ApplicationError::EmptyRuntimeHomeInput { name })
    } else {
        Ok(())
    }
}

fn resolve_for_policy(path: &Path) -> Result<PathBuf, ApplicationError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| ApplicationError::InspectRuntimePath {
                path: path.to_path_buf(),
                source,
            })?
            .join(path)
    };
    let normalized = normalize_lexically(&absolute);
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();

    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        missing.push(name.to_os_string());
        let Some(parent) = existing.parent() else {
            break;
        };
        existing = parent;
    }

    if existing.as_os_str().is_empty() {
        existing = Path::new(".");
    }

    let mut resolved =
        std::fs::canonicalize(existing).map_err(|source| ApplicationError::InspectRuntimePath {
            path: path.to_path_buf(),
            source,
        })?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(normalize_lexically(&resolved))
}

fn capability_boundary(target: &Path) -> Result<(Dir, PathBuf), ApplicationError> {
    let mut existing = target;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| ApplicationError::InspectRuntimePath {
                path: target.to_path_buf(),
                source: std::io::Error::other("runtime path has no existing ancestor"),
            })?;
    }
    let relative = target
        .strip_prefix(existing)
        .map_err(|_| ApplicationError::InspectRuntimePath {
            path: target.to_path_buf(),
            source: std::io::Error::other("runtime path escaped its canonical ancestor"),
        })?
        .to_path_buf();
    let directory = open_canonical_directory(existing).map_err(|source| {
        ApplicationError::InspectRuntimePath {
            path: existing.to_path_buf(),
            source,
        }
    })?;
    Ok((directory, relative))
}

fn open_directory_beneath(directory: &Dir, relative: &Path) -> std::io::Result<Dir> {
    let mut current = directory.try_clone()?.into_std_file();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::other(
                "authorized runtime path contains an unexpected component",
            ));
        };
        current = cap_primitives::fs::open_dir_nofollow(&current, Path::new(name))?;
    }
    Ok(Dir::from_std_file(current))
}

pub(crate) fn open_canonical_directory(path: &Path) -> std::io::Result<Dir> {
    use cap_primitives::fs::{open_ambient_dir, open_dir_nofollow};

    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if names.is_empty() => {
                root.push(component.as_os_str());
            }
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(std::io::Error::other(
                    "canonical directory contains an unexpected path component",
                ));
            }
        }
    }
    if root.as_os_str().is_empty() {
        return Err(std::io::Error::other(
            "canonical directory path is not absolute",
        ));
    }

    let mut directory = open_ambient_dir(&root, ambient_authority())?;
    for name in names {
        directory = open_dir_nofollow(&directory, Path::new(name))?;
    }
    Ok(Dir::from_std_file(directory))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal = matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                );
                if last_is_normal {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[cfg(unix)]
fn prepare_private_directory_beneath(directory: &Dir, relative: &Path) -> std::io::Result<()> {
    use cap_std::fs::{DirBuilder, DirBuilderExt, Permissions, PermissionsExt};

    if relative.as_os_str().is_empty() {
        return directory.set_permissions(".", Permissions::from_mode(0o700));
    }
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    directory.create_dir_with(relative, &builder)?;
    directory.set_permissions(relative, Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn prepare_private_directory_beneath(directory: &Dir, relative: &Path) -> std::io::Result<()> {
    directory.create_dir_all(relative)
}

#[cfg(all(test, unix))]
mod tests {
    #[cfg(target_os = "macos")]
    use super::trusted_root_relative_path;
    use super::{ReadOnlyEventLogAuthority, open_canonical_directory};
    use cap_std::{ambient_authority, fs::Dir};

    fn authority_with_retained_cwd(
        current_directory: &Path,
    ) -> std::io::Result<ReadOnlyEventLogAuthority> {
        Ok(ReadOnlyEventLogAuthority::from_retained_directories(
            Dir::open_ambient_dir(Path::new("/"), ambient_authority())?,
            Ok(Dir::open_ambient_dir(
                current_directory,
                ambient_authority(),
            )?),
        ))
    }

    #[test]
    fn retained_event_log_target_rejects_final_symlink_swap()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-leaf-swap-{}",
            std::process::id()
        ));
        let home = root.join("home");
        let events = home.join("events");
        let outside = root.join("outside.jsonl");
        let leaf = events.join("mission.jsonl");
        std::fs::create_dir_all(&events)?;
        std::fs::write(&leaf, b"inside")?;
        std::fs::write(&outside, b"outside")?;

        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;
        let event_root = authority.bind_runtime_home(&home)?;
        let target = event_root.event_log(Path::new("events/mission.jsonl"))?;
        target.probe()?;
        std::fs::remove_file(&leaf)?;
        std::os::unix::fs::symlink(&outside, &leaf)?;

        let result = target.open();
        assert!(result.is_err());

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn direct_event_log_target_preserves_tmp_alias_prefix() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::io::Read;

        let root = Path::new("/tmp").join(format!(
            "orchestrator-rs-event-read-tmp-alias-{}",
            std::process::id()
        ));
        let leaf = root.join("direct.jsonl");
        std::fs::create_dir_all(&root)?;
        std::fs::write(&leaf, b"direct")?;

        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;
        let target = authority.bind_direct(&leaf)?;
        target.probe()?;
        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "direct");

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trusted_macos_aliases_are_narrow_and_lexical() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            trusted_root_relative_path(Path::new("/tmp/nanika/events"))?,
            PathBuf::from("private/tmp/nanika/events")
        );
        assert_eq!(
            trusted_root_relative_path(Path::new("/var/folders/nanika"))?,
            PathBuf::from("private/var/folders/nanika")
        );
        assert_eq!(
            trusted_root_relative_path(Path::new("/opt/nanika"))?,
            PathBuf::from("opt/nanika")
        );
        assert_eq!(
            trusted_root_relative_path(Path::new("/tmp/missing/../events"))?,
            PathBuf::from("private/tmp/missing/../events")
        );
        Ok(())
    }

    #[test]
    fn event_log_authority_rejects_arbitrary_symlinked_home_and_direct_parent()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-parent-symlink-{}",
            std::process::id()
        ));
        let actual_home = root.join("actual-home");
        let actual_direct = root.join("actual-direct");
        std::fs::create_dir_all(actual_home.join("events"))?;
        std::fs::create_dir_all(&actual_direct)?;
        std::fs::write(actual_direct.join("direct.jsonl"), b"direct")?;
        let linked_home = root.join("linked-home");
        let linked_direct = root.join("linked-direct");
        std::os::unix::fs::symlink(&actual_home, &linked_home)?;
        std::os::unix::fs::symlink(&actual_direct, &linked_direct)?;

        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;
        assert!(authority.bind_runtime_home(&linked_home).is_err());
        assert!(
            authority
                .bind_direct(&linked_direct.join("direct.jsonl"))
                .is_err()
        );

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn direct_event_path_does_not_clean_a_missing_canceled_component()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-direct-missing-canceled-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("mission.jsonl"), b"must-not-open")?;
        let authority = authority_with_retained_cwd(&root)?;

        let error = match authority.bind_direct(Path::new("missing/../mission.jsonl")) {
            Ok(_) => return Err("missing component was lexically canceled".into()),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn direct_event_path_does_not_clean_a_non_directory_canceled_component()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-direct-notdir-canceled-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("not-a-directory"), b"file")?;
        std::fs::write(root.join("mission.jsonl"), b"must-not-open")?;
        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;

        let path = root.join("not-a-directory/../mission.jsonl");
        let error = match authority.bind_direct(&path) {
            Ok(_) => return Err("non-directory component was lexically canceled".into()),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn direct_event_path_does_not_clean_a_symlink_canceled_component()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-direct-symlink-canceled-{}",
            std::process::id()
        ));
        let actual = root.join("actual");
        std::fs::create_dir_all(&actual)?;
        std::fs::write(root.join("mission.jsonl"), b"must-not-open")?;
        std::os::unix::fs::symlink(&actual, root.join("linked"))?;
        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;

        let path = root.join("linked/../mission.jsonl");
        if authority.bind_direct(&path).is_ok() {
            return Err("symbolic-link component was lexically canceled".into());
        }

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn retained_event_log_parent_survives_path_rename_and_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Read;

        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-parent-replace-{}",
            std::process::id()
        ));
        let home = root.join("home");
        let original_home = root.join("original-home");
        let leaf = home.join("events/mission.jsonl");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(&leaf, b"original")?;

        let authority = ReadOnlyEventLogAuthority::acquire_ambient()?;
        let event_root = authority.bind_runtime_home(&home)?;
        let target = event_root.event_log(Path::new("events/mission.jsonl"))?;
        target.probe()?;

        std::fs::rename(&home, &original_home)?;
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(home.join("events/mission.jsonl"), b"replacement")?;

        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "original");
        drop(opened);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn relative_event_path_uses_retained_cwd_after_cwd_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Read;

        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-cwd-replace-{}",
            std::process::id()
        ));
        let current_directory = root.join("cwd");
        let retained_directory = root.join("retained-cwd");
        std::fs::create_dir_all(current_directory.join("events"))?;
        std::fs::write(
            current_directory.join("events/mission.jsonl"),
            b"retained-cwd",
        )?;
        let authority = authority_with_retained_cwd(&current_directory)?;

        std::fs::rename(&current_directory, &retained_directory)?;
        std::fs::create_dir_all(current_directory.join("events"))?;
        std::fs::write(
            current_directory.join("events/mission.jsonl"),
            b"replacement-cwd",
        )?;

        let target = authority.bind_direct(Path::new("events/mission.jsonl"))?;
        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "retained-cwd");
        drop(opened);
        drop(target);
        drop(authority);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn relative_event_path_uses_retained_cwd_after_ancestor_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Read;

        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-cwd-ancestor-replace-{}",
            std::process::id()
        ));
        let ancestor = root.join("ancestor");
        let current_directory = ancestor.join("cwd");
        let retained_ancestor = root.join("retained-ancestor");
        std::fs::create_dir_all(current_directory.join("events"))?;
        std::fs::write(
            current_directory.join("events/mission.jsonl"),
            b"retained-ancestor",
        )?;
        let authority = authority_with_retained_cwd(&current_directory)?;

        std::fs::rename(&ancestor, &retained_ancestor)?;
        std::fs::create_dir_all(current_directory.join("events"))?;
        std::fs::write(
            current_directory.join("events/mission.jsonl"),
            b"replacement-ancestor",
        )?;

        let target = authority.bind_direct(Path::new("events/mission.jsonl"))?;
        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "retained-ancestor");
        drop(opened);
        drop(target);
        drop(authority);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn leading_parent_uses_retained_cwd_after_ancestor_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Read;

        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-cwd-leading-parent-{}",
            std::process::id()
        ));
        let ancestor = root.join("ancestor");
        let current_directory = ancestor.join("cwd");
        let retained_ancestor = root.join("retained-ancestor");
        std::fs::create_dir_all(&current_directory)?;
        std::fs::write(ancestor.join("mission.jsonl"), b"retained-parent")?;
        let authority = authority_with_retained_cwd(&current_directory)?;

        std::fs::rename(&ancestor, &retained_ancestor)?;
        std::fs::create_dir_all(&current_directory)?;
        std::fs::write(ancestor.join("mission.jsonl"), b"replacement-parent")?;

        let target = authority.bind_direct(Path::new("../mission.jsonl"))?;
        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "retained-parent");
        drop(opened);
        drop(target);
        drop(authority);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn absolute_event_path_does_not_resolve_retained_cwd() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::io::Read;

        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-event-read-unresolvable-cwd-{}",
            std::process::id()
        ));
        let absolute_log = root.join("absolute.jsonl");
        std::fs::create_dir_all(&root)?;
        std::fs::write(&absolute_log, b"absolute")?;
        let authority = ReadOnlyEventLogAuthority::from_retained_directories(
            Dir::open_ambient_dir(Path::new("/"), ambient_authority())?,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "injected unresolvable current directory",
            )),
        );

        let target = authority.bind_direct(&absolute_log)?;
        let mut opened = target.open()?;
        let mut content = String::new();
        opened.read_to_string(&mut content)?;
        assert_eq!(content, "absolute");
        let relative_error = match authority.bind_direct(Path::new("relative.jsonl")) {
            Ok(_) => return Err("injected current-directory failure was not retained".into()),
            Err(error) => error,
        };
        assert_eq!(relative_error.kind(), std::io::ErrorKind::NotFound);
        drop(opened);
        drop(target);
        drop(authority);

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn canonical_directory_acquisition_rejects_symlink_components()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-open-nofollow-{}",
            std::process::id()
        ));
        let actual = root.join("actual");
        std::fs::create_dir_all(&actual)?;
        std::os::unix::fs::symlink(&actual, root.join("replacement"))?;

        let canonical_actual = std::fs::canonicalize(&actual)?;
        assert!(open_canonical_directory(&canonical_actual).is_ok());
        assert!(open_canonical_directory(&root.join("replacement")).is_err());

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn shared_policy_resolution_canonicalizes_a_final_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-rs-policy-symlink-{}",
            std::process::id()
        ));
        let actual = root.join("actual");
        let linked = root.join("linked");
        std::fs::create_dir_all(&actual)?;
        std::os::unix::fs::symlink(&actual, &linked)?;

        assert_eq!(
            super::resolve_for_policy(&linked)?,
            std::fs::canonicalize(&actual)?
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    use crate::{
        DirectoryProbe, HomeInputs, HomeSelection, LiveHomeCanaryCapability, ProductionBoundary,
        RuntimeHomeResolver,
    };
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
    };

    static CASE: AtomicU64 = AtomicU64::new(1);

    #[derive(Default)]
    struct Probe {
        dirs: BTreeSet<PathBuf>,
    }
    impl DirectoryProbe for Probe {
        fn exists(&self, path: &Path) -> bool {
            self.dirs.contains(path)
        }
    }

    #[test]
    fn the_bundled_helper_digest_is_sha256() {
        // The published SHA-256 of the empty input, so this implementation is
        // pinned against something outside this crate rather than against
        // itself.
        assert_eq!(
            super::bundled_helper_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_eq!(
            super::bundled_helper_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn the_isolated_selector_requires_exact_opt_in_and_a_matching_helper_digest() {
        use super::{IsolatedHomeCanaryCapability, attest_bundled_helper};
        for refused in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("1 "),
            Some("01"),
        ] {
            assert!(IsolatedHomeCanaryCapability::for_isolated_enrollment_if(refused).is_none());
        }
        assert!(IsolatedHomeCanaryCapability::for_isolated_enrollment_if(Some("1")).is_some());

        let digest = super::bundled_helper_digest(b"helper");
        assert!(attest_bundled_helper(&digest, b"helper", None).is_ok());
        // The env cross-check may confirm the manifest and nothing else: it can
        // neither supply a digest nor override one.
        assert!(attest_bundled_helper(&digest, b"helper", Some(&digest)).is_ok());
        assert!(attest_bundled_helper(&digest, b"other", None).is_err());
        assert!(attest_bundled_helper("", b"helper", None).is_err());
        assert!(attest_bundled_helper("NOTHEX", b"helper", None).is_err());
        assert!(
            attest_bundled_helper(
                &digest,
                b"helper",
                Some(&super::bundled_helper_digest(b"x"))
            )
            .is_err()
        );
    }

    #[test]
    fn enrollment_selector_requires_exact_opt_in() {
        assert!(LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("1")).is_some());
        assert!(LiveHomeCanaryCapability::for_explicit_enrollment_if(None).is_none());
        assert!(LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("0")).is_none());
        assert!(LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("true")).is_none());
        assert!(LiveHomeCanaryCapability::for_explicit_enrollment_if(Some("yes")).is_none());
    }

    #[test]
    fn enrolled_canary_authorizes_and_prepares_a_hermetic_production_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::env::temp_dir();
        let temp = std::fs::canonicalize(&temp).unwrap_or(temp);
        let fake_user_home = temp.join(format!(
            "orchestrator-rs-enroll-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let alluka = fake_user_home.join(".alluka");
        let mut probe = Probe::default();
        probe.dirs.insert(alluka.clone());
        let resolved = RuntimeHomeResolver::resolve(
            &HomeInputs::from_user_home(fake_user_home.clone()),
            &probe,
        )?;
        assert_eq!(resolved.selection(), HomeSelection::ExistingAlluka);

        // The only production authorization API requires this opaque proof;
        // there is no optional or caller-path-derived alternative.
        let cap = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&cap)?;
        authorized.prepare()?;
        assert!(alluka.is_dir());

        let _ = std::fs::remove_dir_all(&fake_user_home);
        Ok(())
    }

    #[test]
    fn production_boundary_from_enrolled_home_implements_capability_root()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::capability::CapabilityRoot;

        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let fake_user_home = temp.join(format!(
            "orchestrator-rs-prodbnd-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let alluka = fake_user_home.join(".alluka");
        let mut probe = Probe::default();
        probe.dirs.insert(alluka.clone());
        let resolved = RuntimeHomeResolver::resolve(
            &HomeInputs::from_user_home(fake_user_home.clone()),
            &probe,
        )?;
        let cap = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&cap)?;
        authorized.prepare()?;

        let boundary = ProductionBoundary::from_authorized_home(&authorized)?;
        assert_eq!(boundary.canonical_path(), authorized.path());
        // A freshly prepared home is empty.
        assert!(boundary.directory().entries()?.next().is_none());
        // Re-verification succeeds against the unchanged root.
        assert!(boundary.verify().is_ok());

        // Removing the canonical root breaks re-verification.
        std::fs::remove_dir_all(&alluka)?;
        assert!(boundary.verify().is_err());

        let _ = std::fs::remove_dir_all(&fake_user_home);
        Ok(())
    }

    #[test]
    fn production_authorization_normalizes_relative_parent_components()
    -> Result<(), Box<dyn std::error::Error>> {
        let current = std::env::current_dir()?;
        let parent = current.parent().ok_or("workspace has no parent")?;
        let mut inputs = HomeInputs::from_user_home(parent.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(Path::new("../relative-runtime").to_path_buf());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        assert_eq!(authorized.path(), parent.join("relative-runtime"));
        Ok(())
    }

    #[test]
    fn absolute_parent_components_cannot_discard_the_filesystem_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
        let relative_temp = canonical_temp.strip_prefix(Path::new("/"))?;
        let candidate = Path::new("/..")
            .join(relative_temp)
            .join("root-normalization-audit");
        let mut inputs = HomeInputs::from_user_home(canonical_temp.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(candidate);
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        assert_eq!(
            authorized.path(),
            canonical_temp.join("root-normalization-audit")
        );
        Ok(())
    }

    #[test]
    fn production_authority_debug_is_redacted() -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let sensitive = temp.join(format!(
            "secret-runtime-home-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut inputs = HomeInputs::from_user_home(temp.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(sensitive.clone());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        let debug = format!("{authorized:?}");
        assert!(!debug.contains(sensitive.to_string_lossy().as_ref()));
        assert_eq!(
            debug,
            "AuthorizedProductionRuntimeHome { kind: \"enrolled-production\" }"
        );
        Ok(())
    }

    #[test]
    fn resolver_input_and_result_debug_redact_paths_and_user_identifiers()
    -> Result<(), Box<dyn std::error::Error>> {
        let sensitive = PathBuf::from("/secret-user-13579/private-runtime");
        let mut inputs = HomeInputs::from_user_home("/secret-user-13579/home");
        inputs.orchestrator_config_dir = Some(sensitive.clone());
        inputs.alluka_home = Some(PathBuf::from("/secret-user-13579/alluka"));
        inputs.via_home = Some(PathBuf::from("/secret-user-13579/via"));
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;

        let input_debug = format!("{inputs:?}");
        let resolved_debug = format!("{resolved:?}");
        for value in [&input_debug, &resolved_debug] {
            assert!(!value.contains("13579"), "{value}");
            assert!(!value.contains("secret-user"), "{value}");
            assert!(
                !value.contains(sensitive.to_string_lossy().as_ref()),
                "{value}"
            );
        }
        assert_eq!(
            input_debug,
            "HomeInputs { orchestrator_config_dir_present: true, alluka_home_present: true, via_home_present: true }"
        );
        assert_eq!(
            resolved_debug,
            "ResolvedRuntimeHome { selection: OrchestratorConfigDir }"
        );
        Ok(())
    }

    #[test]
    fn concurrent_production_prepare_serializes_one_retained_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::capability::CapabilityRoot;

        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "orchestrator-rs-prod-concurrent-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root)?;
        let runtime = root.join("runtime");
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(runtime.clone());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = Arc::new(resolved.authorize_production(&enrollment)?);
        let barrier = Arc::new(Barrier::new(8));

        let mut workers = Vec::new();
        for _ in 0..8 {
            let authorized = Arc::clone(&authorized);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                authorized.prepare()
            }));
        }
        for worker in workers {
            worker
                .join()
                .map_err(|_| std::io::Error::other("prepare worker panicked"))??;
        }

        let first = ProductionBoundary::from_authorized_home(&authorized)?;
        let second = ProductionBoundary::from_authorized_home(&authorized)?;
        assert!(first.verify().is_ok());
        assert!(second.verify().is_ok());
        assert_eq!(
            super::identity(&first.directory().dir_metadata()?),
            super::identity(&second.directory().dir_metadata()?)
        );

        drop(first);
        drop(second);
        drop(authorized);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn concurrent_prepare_after_identity_swap_fails_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "orchestrator-rs-prod-swap-concurrent-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let runtime = root.join("runtime");
        let displaced = root.join("displaced");
        std::fs::create_dir_all(&runtime)?;
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(runtime.clone());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = Arc::new(resolved.authorize_production(&enrollment)?);
        authorized.prepare()?;
        std::fs::rename(&runtime, &displaced)?;
        std::fs::create_dir(&runtime)?;
        std::fs::write(runtime.join("replacement-sentinel"), b"untouched")?;

        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let authorized = Arc::clone(&authorized);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                authorized.prepare()
            }));
        }
        for worker in workers {
            assert!(matches!(
                worker
                    .join()
                    .map_err(|_| std::io::Error::other("swap worker panicked"))?,
                Err(crate::ApplicationError::RuntimeHomeIdentityChanged)
            ));
        }
        assert_eq!(
            std::fs::read(runtime.join("replacement-sentinel"))?,
            b"untouched"
        );

        drop(authorized);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn production_error_chain_redacts_runtime_path_and_user_identifier()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::error::Error;

        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "secret-user-24680-prod-error-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let allowed = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed)?;
        std::fs::create_dir_all(&outside)?;
        let runtime = allowed.join("missing/runtime");
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home-24680"));
        inputs.orchestrator_config_dir = Some(runtime.clone());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;
        std::os::unix::fs::symlink(&outside, allowed.join("missing"))?;
        let error = match authorized.prepare() {
            Err(error) => error,
            Ok(()) => return Err(std::io::Error::other("symlink swap was accepted").into()),
        };

        let mut rendered = vec![format!("{error}"), format!("{error:?}")];
        let mut source = error.source();
        while let Some(current) = source {
            rendered.push(format!("{current}"));
            rendered.push(format!("{current:?}"));
            source = current.source();
        }
        for value in rendered {
            assert!(!value.contains(root.to_string_lossy().as_ref()), "{value}");
            assert!(!value.contains("24680"), "{value}");
            assert!(!value.contains("secret-user"), "{value}");
        }

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn production_boundary_rejects_unprepared_and_identity_swapped_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "orchestrator-rs-prod-identity-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let runtime = root.join("runtime");
        let displaced = root.join("displaced-runtime");
        std::fs::create_dir_all(&runtime)?;
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(runtime.clone());
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        assert!(matches!(
            ProductionBoundary::from_authorized_home(&authorized),
            Err(crate::ApplicationError::ProductionRuntimeHomeNotPrepared)
        ));
        authorized.prepare()?;
        std::fs::rename(&runtime, &displaced)?;
        std::fs::create_dir(&runtime)?;
        std::fs::write(runtime.join("replacement-sentinel"), b"untouched")?;

        assert!(matches!(
            ProductionBoundary::from_authorized_home(&authorized),
            Err(crate::ApplicationError::RuntimeHomeIdentityChanged)
        ));
        assert_eq!(
            std::fs::read(runtime.join("replacement-sentinel"))?,
            b"untouched"
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn production_preparation_rejects_post_authorization_symlink_escape()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "orchestrator-rs-prod-symlink-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let allowed_parent = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed_parent)?;
        std::fs::create_dir_all(&outside)?;
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(allowed_parent.join("missing/runtime"));
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        std::os::unix::fs::symlink(&outside, allowed_parent.join("missing"))?;
        assert!(authorized.prepare().is_err());
        assert!(!outside.join("runtime").exists());

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn production_preparation_rejects_in_boundary_redirect_to_live_home()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
        let root = temp.join(format!(
            "orchestrator-rs-prod-live-redirect-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        let fake_user_home = root.join("user");
        let fake_alluka = fake_user_home.join(".alluka");
        std::fs::create_dir_all(&fake_alluka)?;
        let mut inputs = HomeInputs::from_user_home(root.join("ignored-user-home"));
        inputs.orchestrator_config_dir = Some(fake_user_home.join("candidate/runtime"));
        let resolved = RuntimeHomeResolver::resolve(&inputs, &Probe::default())?;
        let enrollment = LiveHomeCanaryCapability { _private: () };
        let authorized = resolved.authorize_production(&enrollment)?;

        std::os::unix::fs::symlink(&fake_alluka, fake_user_home.join("candidate"))?;
        assert!(authorized.prepare().is_err());
        assert!(!fake_alluka.join("runtime").exists());

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn production_boundary_from_canonical_root_rejects_a_missing_path() {
        let missing = std::env::temp_dir().join(format!(
            "orchestrator-rs-prodbnd-missing-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!missing.exists());
        // The internal constructor still requires a real canonical directory;
        // a missing path is refused before any boundary is minted.
        assert!(ProductionBoundary::from_canonical_root(&missing).is_err());
    }
}
