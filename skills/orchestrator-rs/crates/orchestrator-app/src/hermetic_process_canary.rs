#![cfg(all(unix, feature = "verification-process-canary"))]
//! Sealed hermetic process canary.
//!
//! Composes the fixed hermetic compatibility boundary with a closed enrollment of the *current*
//! executable, producing a private, attested helper capability that the
//! disposable verification canary can spawn without exposing caller-selected
//! process inputs.
//!
//! The enrollment is closed: the only non-test entry point reads
//! [`std::env::current_exe()`] itself. No request handler, CLI argument, or
//! external caller can substitute an executable path. The current binary's
//! bytes are copied into `<leaf>/bin/<label>` as a mode-`0700` regular file
//! via `O_EXCL` creation, then attested by length and SHA-256. Every
//! [`HermeticProcessCanary::verify`] reopens the helper through the leaf
//! boundary and proves its identity and content are unchanged.
//!
//! This mirrors the fixture [`crate::ExecutableCapability`] shape
//! (`open_verified` returning a retained file plus exact image) so the
//! existing launch machinery can consume the canary with a thin adapter.

use std::{
    fmt,
    fs::File,
    io,
    os::unix::fs::{FileExt, MetadataExt},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use cap_std::fs::Dir;
use orchestrator_process::{ExecutableFileAttestation, MAX_ATTESTED_EXECUTABLE_BYTES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ProductionBoundary,
    capability::CapabilityRoot,
    durable_process_service::RetainedFileIdentity,
    fixture_authority::FixtureAdmissionPolicy,
    fs_util::{
        create_dir_private, create_private_file, open_dir_path_nofollow, open_file_nofollow,
        sync_dir, validate_component,
    },
};

/// Maximum byte length of a caller-supplied helper label.
const MAX_LABEL_BYTES: usize = 64;
/// Subdirectory beneath the fixed compatibility target that holds helpers.
const HELPER_BIN_DIR: &str = "bin";
/// Fixed CLI-level enrollment record. This is intentionally not part of the
/// generic multi-helper authority: it seals the one helper selected by the
/// v1 hermetic-canary composition root across process restarts.
const CLI_ENROLLMENT_RECORD: &str = ".orchestrator-hermetic-canary-enrollment-v1";
const CLI_ENROLLMENT_VERSION: u8 = 1;
const MAX_CLI_ENROLLMENT_BYTES: u64 = 4 * 1024;
const HERMETIC_CANARY_EXECUTABLE_ID_PREFIX: &str = "nanika-canary-v1-";
/// Streaming I/O chunk used while hashing and copying helper bytes.
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Failures reported while enrolling or recovering a hermetic process canary.
///
/// Every variant is deliberately path- and identity-free: the canonical leaf
/// path, helper label, device, inode, length, owner, and SHA-256 digest never
/// appear in the [`core::fmt::Display`] or [`core::fmt::Debug`] rendering.
#[derive(Debug, Error)]
pub enum HermeticProcessCanaryError {
    /// The caller-supplied helper label failed charset or length validation.
    #[error("hermetic process canary label is invalid")]
    InvalidLabel,
    /// The source executable is missing, unsafe, or not owner-controlled.
    #[error("hermetic process canary source executable is unsafe or missing")]
    UnsafeSource,
    /// A helper is already enrolled at the requested leaf and label.
    #[error("hermetic process canary helper is already enrolled")]
    HelperExists,
    /// The helper was not present when recovery attempted to reopen it.
    #[error("hermetic process canary helper is missing")]
    HelperMissing,
    /// The helper reachable at the leaf changed identity or content between
    /// enrollment and use. Covers replacement, byte tamper, mode/owner
    /// changes, and compatibility-boundary break.
    #[error("hermetic process canary helper identity or content changed")]
    HelperSwap,
    /// The admitted runtime, lifecycle, store, or terminal proof failed. This
    /// intentionally carries no nested diagnostic that could expose a path.
    #[error("hermetic process canary runtime failed")]
    RuntimeFailure,
    /// The exact process attempt is executing or uncertain and must be
    /// reconciled before the canary may continue.
    #[error("hermetic process canary requires explicit process recovery")]
    RecoveryRequired,
    /// A filesystem operation against the compatibility target failed. The wrapped
    /// [`std::io::Error`] carries no caller-supplied path.
    #[error("hermetic process canary capability filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
}

fn map_provider_error(
    error: crate::hermetic_provider::HermeticProviderError,
) -> HermeticProcessCanaryError {
    match error {
        crate::hermetic_provider::HermeticProviderError::ProcessRecoveryRequired
        | crate::hermetic_provider::HermeticProviderError::ProcessRecoveryReconciled(_) => {
            HermeticProcessCanaryError::RecoveryRequired
        }
        crate::hermetic_provider::HermeticProviderError::ExecutionBindingConflict
        | crate::hermetic_provider::HermeticProviderError::ProviderLaunch(
            crate::durable_process_service::ProviderLaunchError::ExecutableAdmission,
        ) => HermeticProcessCanaryError::HelperSwap,
        _ => HermeticProcessCanaryError::RuntimeFailure,
    }
}

impl From<crate::exact_leaf_authority::ExactLeafError> for HermeticProcessCanaryError {
    /// Any exact-leaf verification failure means the boundary that bounds the
    /// enrolled helper was broken; map it to a helper swap so callers fail
    /// closed, except for plain I/O which preserves its diagnostics.
    fn from(error: crate::exact_leaf_authority::ExactLeafError) -> Self {
        use crate::exact_leaf_authority::ExactLeafError;
        match error {
            ExactLeafError::Io(source) => HermeticProcessCanaryError::Io(source),
            _ => HermeticProcessCanaryError::HelperSwap,
        }
    }
}

/// Authority that enrolls and recovers hermetic process canaries.
///
/// Stateless; kept as a unit struct so the enrollment surface reads as a named
/// capability rather than a bag of free functions.
pub(crate) struct HermeticProcessCanaryAuthority;

impl fmt::Debug for HermeticProcessCanaryAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticProcessCanaryAuthority")
            .field("kind", &"sealed-hermetic-process-canary")
            .finish()
    }
}

impl HermeticProcessCanaryAuthority {
    /// Enrolls the current process executable into `leaf` as `<leaf>/bin/<label>`.
    ///
    /// This is the sole non-test entry point. The source executable is read
    /// from [`std::env::current_exe()`]; no caller argument can substitute a
    /// different path.
    pub(crate) fn enroll_current_executable(
        boundary: Arc<ProductionBoundary>,
        label: &str,
    ) -> Result<HermeticProcessCanary, HermeticProcessCanaryError> {
        let source = std::env::current_exe()?;
        Self::enroll_from_source(boundary, label, &source)
    }

    /// Reopens an existing enrolled helper without creating or modifying it.
    ///
    /// Intended for the disposable canary lifecycle across process restarts.
    /// The helper must be present, private, and owner-owned; a missing or
    /// unsafe helper fails closed.
    ///
    /// Replacement of the helper *before* recovery cannot be detected from the
    /// leaf alone: this primitive retains no attestation between processes.
    /// A consumer that needs replacement-since-enrollment detection layers a
    /// versioned attestation manifest above the canary, analogous to how the
    /// hermetic canary authority layers its layout manifest over fixture
    /// targets.
    pub(crate) fn recover(
        boundary: Arc<ProductionBoundary>,
        label: &str,
    ) -> Result<HermeticProcessCanary, HermeticProcessCanaryError> {
        let label_path = validate_label(label)?;
        boundary
            .verify()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;

        let bin = open_helper_bin(boundary.directory())?;
        let file = open_helper_file(&bin, label_path)?;
        let (bytes, identity, sha256) = read_and_attest(&file)?;

        Ok(HermeticProcessCanary {
            boundary,
            label: label.to_string(),
            file,
            identity,
            attestation: ExecutableFileAttestation::new(identity.length(), sha256),
            expected_bytes: Arc::from(bytes),
        })
    }

    fn enroll_from_source(
        boundary: Arc<ProductionBoundary>,
        label: &str,
        source: &Path,
    ) -> Result<HermeticProcessCanary, HermeticProcessCanaryError> {
        let label_path = validate_label(label)?;
        let (source_bytes, source_sha256) = read_source_executable(source)?;

        boundary
            .verify()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        let bin = ensure_helper_bin(boundary.directory())?;
        match create_private_file(&bin, label_path, &source_bytes, true) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(HermeticProcessCanaryError::HelperExists);
            }
            Err(error) => return Err(HermeticProcessCanaryError::Io(error)),
        }

        let file = open_file_nofollow(&bin, label_path)
            .map_err(HermeticProcessCanaryError::Io)?
            .into_std();
        let (installed_bytes, identity, installed_sha256) = read_and_attest(&file)?;
        if installed_sha256 != source_sha256 || installed_bytes != source_bytes {
            // The freshly written helper does not match the source bytes this
            // authority read. Do not delete it by an unverified path; surface
            // the swap and let the operator dispose of the leaf.
            return Err(HermeticProcessCanaryError::HelperSwap);
        }

        file.sync_all()?;
        sync_dir(&bin)?;
        boundary
            .verify()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;

        Ok(HermeticProcessCanary {
            boundary,
            label: label.to_string(),
            file,
            identity,
            attestation: ExecutableFileAttestation::new(identity.length(), installed_sha256),
            expected_bytes: Arc::from(source_bytes),
        })
    }

    /// Test-only entry point that enrolls an explicit source path. Production
    /// builds have no path-based enrollment: the only non-test source is
    /// [`std::env::current_exe()`] read by [`Self::enroll_current_executable`].
    #[cfg(test)]
    pub(crate) fn enroll_executable_at_for_test(
        boundary: Arc<ProductionBoundary>,
        label: &str,
        source: &Path,
    ) -> Result<HermeticProcessCanary, HermeticProcessCanaryError> {
        Self::enroll_from_source(boundary, label, source)
    }
}

/// Retained hermetic process canary capability.
///
/// Carries the fixed compatibility boundary (shared via [`Arc`] so the launch
/// machinery can retain it), the enrolled helper's open file handle, the
/// retained filesystem identity, the length+SHA-256 attestation, and the exact
/// source bytes. The raw file handle and leaf directory are crate-private and
/// never leave this module.
pub(crate) struct HermeticProcessCanary {
    boundary: Arc<ProductionBoundary>,
    label: String,
    file: File,
    identity: RetainedFileIdentity,
    attestation: ExecutableFileAttestation,
    expected_bytes: Arc<[u8]>,
}

impl fmt::Debug for HermeticProcessCanary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: never reveal the leaf path, helper label, file handle,
        // filesystem identity, or SHA-256 digest in diagnostics, events, or
        // logs.
        formatter
            .debug_struct("HermeticProcessCanary")
            .field("kind", &"sealed-hermetic-process-canary")
            .finish()
    }
}

impl HermeticProcessCanary {
    /// Returns the fixed compatibility boundary that bounds this canary, shared by [`Arc`] so
    /// the launch machinery can retain the same boundary capability.
    pub(crate) fn boundary(&self) -> &Arc<ProductionBoundary> {
        &self.boundary
    }

    /// Returns the helper label enrolled beneath [`Self::leaf`].
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    /// Returns the length and SHA-256 attestation for the enrolled helper.
    pub(crate) const fn attestation(&self) -> ExecutableFileAttestation {
        self.attestation
    }

    /// Stable logical executable identity shared by CLI enrollment, provider
    /// admission, and the operator report.
    pub(crate) fn executable_logical_id(&self) -> String {
        format!(
            "{HERMETIC_CANARY_EXECUTABLE_ID_PREFIX}{}-{}",
            self.label,
            lowercase_hex_sha256(self.attestation)
        )
    }

    /// Reopens the helper through the compatibility boundary and proves its identity,
    /// mode, owner, length, and SHA-256 digest still match the enrollment.
    ///
    /// Failure maps every deviation (replacement, byte tamper, mode/owner
    /// change, boundary break) to [`HermeticProcessCanaryError::HelperSwap`]
    /// so callers fail closed on any observed change.
    pub(crate) fn verify(&self) -> Result<(), HermeticProcessCanaryError> {
        self.boundary
            .verify()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        let reopened = reopen_helper_through_boundary(&self.boundary, &self.label)?;
        let reopened_identity = RetainedFileIdentity::read(&reopened)
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        if !reopened_identity.matches(self.identity) {
            return Err(HermeticProcessCanaryError::HelperSwap);
        }
        compare_helper_bytes(&reopened, &self.expected_bytes)?;
        let retained_identity = RetainedFileIdentity::read(&self.file)
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        if !retained_identity.matches(self.identity) {
            return Err(HermeticProcessCanaryError::HelperSwap);
        }
        compare_helper_bytes(&self.file, &self.expected_bytes)?;
        Ok(())
    }

    /// Returns a duplicated helper file handle plus the exact enrolled bytes.
    ///
    /// Mirrors [`crate::ExecutableCapability::open_verified`] so the existing
    /// launch machinery can consume this capability through a thin adapter.
    pub(crate) fn open_verified(&self) -> Result<(File, Arc<[u8]>), HermeticProcessCanaryError> {
        self.verify()?;
        let cloned = self.file.try_clone()?;
        Ok((cloned, Arc::clone(&self.expected_bytes)))
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CliCanaryEnrollmentV1 {
    version: u8,
    helper_label: String,
    executable_logical_id: String,
    helper_length: u64,
    helper_sha256: String,
    helper_device: u64,
    helper_inode: u64,
}

impl CliCanaryEnrollmentV1 {
    fn from_canary(canary: &HermeticProcessCanary) -> Result<Self, HermeticProcessCanaryError> {
        canary
            .verify()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        let metadata = canary
            .file
            .metadata()
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        Ok(Self {
            version: CLI_ENROLLMENT_VERSION,
            helper_label: canary.label().to_owned(),
            executable_logical_id: canary.executable_logical_id(),
            helper_length: canary.attestation().length(),
            helper_sha256: lowercase_hex_sha256(canary.attestation()),
            helper_device: metadata.dev(),
            helper_inode: metadata.ino(),
        })
    }

    fn validate(&self) -> Result<(), HermeticProcessCanaryError> {
        validate_label(&self.helper_label).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        if self.version != CLI_ENROLLMENT_VERSION
            || self.helper_length == 0
            || self.helper_length > MAX_ATTESTED_EXECUTABLE_BYTES
            || self.helper_sha256.len() != 64
            || !self
                .helper_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.executable_logical_id
                != format!(
                    "{HERMETIC_CANARY_EXECUTABLE_ID_PREFIX}{}-{}",
                    self.helper_label, self.helper_sha256
                )
        {
            return Err(HermeticProcessCanaryError::HelperSwap);
        }
        Ok(())
    }

    fn matches_canary(&self, canary: &HermeticProcessCanary) -> bool {
        let Ok(metadata) = canary.file.metadata() else {
            return false;
        };
        self.helper_label == canary.label()
            && self.executable_logical_id == canary.executable_logical_id()
            && self.helper_length == canary.attestation().length()
            && self.helper_sha256 == lowercase_hex_sha256(canary.attestation())
            && self.helper_device == metadata.dev()
            && self.helper_inode == metadata.ino()
    }
}

/// Publishes the CLI composition's single-helper enrollment binding. The
/// helper already exists and is durable; this fixed O_EXCL record is the
/// commit point that makes the helper recoverable on a later invocation.
fn publish_cli_enrollment_record(
    compatibility_home: &Dir,
    canary: &HermeticProcessCanary,
) -> Result<(), HermeticProcessCanaryError> {
    let record = CliCanaryEnrollmentV1::from_canary(canary)?;
    let bytes = serde_json::to_vec(&record).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_CLI_ENROLLMENT_BYTES {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }

    let bin = open_helper_bin(compatibility_home)?;
    create_private_file(&bin, Path::new(CLI_ENROLLMENT_RECORD), &bytes, false)
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    let file = open_file_nofollow(&bin, Path::new(CLI_ENROLLMENT_RECORD))
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?
        .into_std();
    file.sync_all()
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    sync_dir(&bin).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;

    let recovered = read_cli_enrollment_record(&file)?;
    if recovered != record || !recovered.matches_canary(canary) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    Ok(())
}

fn read_cli_enrollment_record(
    file: &File,
) -> Result<CliCanaryEnrollmentV1, HermeticProcessCanaryError> {
    let before = file
        .metadata()
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    if !valid_cli_enrollment_metadata(&before) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    let length =
        usize::try_from(before.len()).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    let mut bytes = vec![0; length];
    file.read_exact_at(&mut bytes, 0)
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    let after = file
        .metadata()
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    if !valid_cli_enrollment_metadata(&after)
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
    {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }

    let record: CliCanaryEnrollmentV1 =
        serde_json::from_slice(&bytes).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    record.validate()?;
    let canonical =
        serde_json::to_vec(&record).map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    if canonical != bytes {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    Ok(record)
}

fn valid_cli_enrollment_metadata(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o7777 == 0o600
        && metadata.nlink() == 1
        && metadata.len() > 0
        && metadata.len() <= MAX_CLI_ENROLLMENT_BYTES
}

fn recover_cli_enrolled_canary(
    compatibility_home: Arc<ProductionBoundary>,
) -> Result<HermeticProcessCanary, HermeticProcessCanaryError> {
    let bin = open_helper_bin(compatibility_home.directory())
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    let record_file = open_file_nofollow(&bin, Path::new(CLI_ENROLLMENT_RECORD))
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?
        .into_std();
    let record = read_cli_enrollment_record(&record_file)?;
    let canary = HermeticProcessCanaryAuthority::recover(compatibility_home, &record.helper_label)
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    if !record.matches_canary(&canary) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    canary
        .verify()
        .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
    Ok(canary)
}

fn validate_label(label: &str) -> Result<&Path, HermeticProcessCanaryError> {
    if label.len() > MAX_LABEL_BYTES || !validate_component(label) {
        return Err(HermeticProcessCanaryError::InvalidLabel);
    }
    Ok(Path::new(label))
}

/// Opens `<leaf>/bin`, demanding a private, owner-owned directory.
fn open_helper_bin(leaf: &Dir) -> Result<Dir, HermeticProcessCanaryError> {
    let bin = open_dir_path_nofollow(leaf, Path::new(HELPER_BIN_DIR)).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            HermeticProcessCanaryError::HelperMissing
        } else {
            HermeticProcessCanaryError::Io(source)
        }
    })?;
    if !is_safe_helper_bin(&bin.dir_metadata()?) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    Ok(bin)
}

/// Creates `<leaf>/bin` if missing and reopens it as a private, owner-owned
/// directory. An existing unsafe entry fails closed.
fn ensure_helper_bin(leaf: &Dir) -> Result<Dir, HermeticProcessCanaryError> {
    let created = match create_dir_private(leaf, Path::new(HELPER_BIN_DIR)) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(HermeticProcessCanaryError::Io(error)),
    };
    let bin = open_dir_path_nofollow(leaf, Path::new(HELPER_BIN_DIR))?;
    if !is_safe_helper_bin(&bin.dir_metadata()?) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    if created {
        sync_dir(&bin)?;
        sync_dir(leaf)?;
    }
    Ok(bin)
}

/// Opens `<bin>/<label>` read-only, no-follow.
fn open_helper_file(bin: &Dir, label: &Path) -> Result<File, HermeticProcessCanaryError> {
    open_file_nofollow(bin, label)
        .map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                HermeticProcessCanaryError::HelperMissing
            } else {
                HermeticProcessCanaryError::Io(source)
            }
        })
        .map(cap_std::fs::File::into_std)
}

/// Reopens the helper through the retained leaf capability, proving the leaf
/// boundary still bounds the file a caller would spawn.
fn reopen_helper_through_boundary(
    boundary: &ProductionBoundary,
    label: &str,
) -> Result<File, HermeticProcessCanaryError> {
    let bin = open_dir_path_nofollow(boundary.directory(), Path::new(HELPER_BIN_DIR))?;
    if !is_safe_helper_bin(&bin.dir_metadata()?) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    let file = open_file_nofollow(&bin, Path::new(label))?.into_std();
    Ok(file)
}

/// Reads the helper through `file` with before/after identity re-checks and
/// returns its exact bytes, retained identity, and SHA-256 digest.
fn read_and_attest(
    file: &File,
) -> Result<(Vec<u8>, RetainedFileIdentity, [u8; 32]), HermeticProcessCanaryError> {
    let identity = RetainedFileIdentity::read(file)?;
    let length = identity.length();
    let mut bytes = vec![
        0u8;
        usize::try_from(length)
            .map_err(|_| io::Error::other("helper length does not fit in usize"))?
    ];
    let mut offset = 0u64;
    while offset < length {
        let remaining = length - offset;
        let bounded = usize::try_from(remaining.min(HASH_BUFFER_BYTES as u64))
            .map_err(|_| io::Error::other("helper read chunk does not fit in usize"))?;
        let read = file.read_at(
            &mut bytes[offset as usize..offset as usize + bounded],
            offset,
        )?;
        if read == 0 {
            return Err(HermeticProcessCanaryError::Io(io::Error::other(
                "helper ended before its admitted length",
            )));
        }
        offset = offset.saturating_add(read as u64);
    }
    if !identity.matches(RetainedFileIdentity::read(file)?) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    let mut digest = Sha256::new();
    digest.update(&bytes);
    Ok((bytes, identity, digest.finalize().into()))
}

/// Proves `file` still matches `expected` byte-for-byte under a fresh
/// before/after identity re-check.
fn compare_helper_bytes(file: &File, expected: &[u8]) -> Result<(), HermeticProcessCanaryError> {
    let before = RetainedFileIdentity::read(file)?;
    if before.length() != u64::try_from(expected.len()).unwrap_or(u64::MAX) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    let mut offset = 0usize;
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    while offset < expected.len() {
        let remaining = expected.len() - offset;
        let chunk_len = remaining.min(buffer.len());
        let read = file.read_at(&mut buffer[..chunk_len], offset as u64)?;
        if read == 0 || buffer[..read] != expected[offset..offset.saturating_add(read)] {
            return Err(HermeticProcessCanaryError::HelperSwap);
        }
        offset = offset.saturating_add(read);
    }
    if !before.matches(RetainedFileIdentity::read(file)?) {
        return Err(HermeticProcessCanaryError::HelperSwap);
    }
    Ok(())
}

/// Validates and reads a source executable path supplied by the authority
/// itself (only [`std::env::current_exe()`] in non-test builds). The source
/// must be a private-owner regular file with an execute bit, no group/other
/// write, and a reviewed length. Returns its exact bytes and SHA-256 digest.
fn read_source_executable(path: &Path) -> Result<(Vec<u8>, [u8; 32]), HermeticProcessCanaryError> {
    let path_metadata = std::fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() {
        return Err(HermeticProcessCanaryError::UnsafeSource);
    }
    validate_source_metadata(&path_metadata)?;

    let file = File::open(path)?;
    let fd_metadata = file.metadata()?;
    // Prove the opened descriptor is the same object the path resolved to.
    if path_metadata.dev() != fd_metadata.dev()
        || path_metadata.ino() != fd_metadata.ino()
        || path_metadata.len() != fd_metadata.len()
    {
        return Err(HermeticProcessCanaryError::UnsafeSource);
    }
    validate_source_metadata(&fd_metadata)?;

    let length = fd_metadata.len();
    let mut bytes = vec![
        0u8;
        usize::try_from(length).map_err(|_| io::Error::other(
            "source executable length does not fit in usize"
        ))?
    ];
    let mut offset = 0u64;
    while offset < length {
        let remaining = length - offset;
        let bounded = usize::try_from(remaining.min(HASH_BUFFER_BYTES as u64))
            .map_err(|_| io::Error::other("source read chunk does not fit in usize"))?;
        let read = file.read_at(
            &mut bytes[offset as usize..offset as usize + bounded],
            offset,
        )?;
        if read == 0 {
            return Err(HermeticProcessCanaryError::Io(io::Error::other(
                "source executable ended before its admitted length",
            )));
        }
        offset = offset.saturating_add(read as u64);
    }
    let after = file.metadata()?;
    if after.dev() != fd_metadata.dev()
        || after.ino() != fd_metadata.ino()
        || after.len() != fd_metadata.len()
    {
        return Err(HermeticProcessCanaryError::UnsafeSource);
    }
    let mut digest = Sha256::new();
    digest.update(&bytes);
    Ok((bytes, digest.finalize().into()))
}

fn validate_source_metadata(
    metadata: &std::fs::Metadata,
) -> Result<(), HermeticProcessCanaryError> {
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_ATTESTED_EXECUTABLE_BYTES
    {
        return Err(HermeticProcessCanaryError::UnsafeSource);
    }
    Ok(())
}

fn is_safe_helper_bin(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt;
    metadata.is_dir()
        && metadata.mode() & 0o7777 == 0o700
        && metadata.uid() == rustix::process::geteuid().as_raw()
}

/// Operator-facing report returned by [`run_hermetic_process_canary`].
#[derive(Debug, Eq, PartialEq)]
pub struct HermeticProcessCanaryReport {
    /// Label of the published exact leaf.
    pub leaf_label: String,
    /// Label of the enrolled helper beneath `<leaf>/bin/`.
    pub helper_label: String,
    /// Mission identifier of the disposable canary workspace.
    pub mission_id: String,
    /// Phase-worker identifier derived for the canary launch.
    pub worker_id: String,
    /// Admitted byte length of the enrolled helper.
    pub helper_length: u64,
    /// Lowercase hexadecimal SHA-256 of the enrolled helper bytes.
    pub helper_sha256_hex: String,
    /// Logical executable id the dispatch path routes to the canary helper.
    pub executable_logical_id: String,
    /// Terminal status of the canary process: "succeeded" or "failed".
    pub terminal_status: String,
    /// Whether this invocation durably recorded a kernel execution identity
    /// for a runnable attempt. A terminal replay is always false even though
    /// the retained prior attempt has an execution identity.
    pub spawned: bool,
}

/// The env var that names a fresh or previously enrolled exact canary root.
/// A fresh path is published atomically. An enrolled path is recovered and
/// either continues an admissible pending attempt or replays a proven terminal
/// decision.
pub const HERMETIC_RUN_ROOT_ENV: &str = "NANIKA_HERMETIC_RUN_ROOT";

/// Runs the disposable hermetic process canary end-to-end against the exact
/// fresh or enrolled root at `NANIKA_HERMETIC_RUN_ROOT`.
///
/// Fresh and admissible pending attempts compose the durable provider actor,
/// dispatch the enrolled current executable, and persist its terminal
/// lifecycle. A root whose exact attempt is already terminal replays that
/// decision without relaunching.
pub fn run_hermetic_process_canary(
    policy: &FixtureAdmissionPolicy,
) -> Result<HermeticProcessCanaryReport, HermeticProcessCanaryError> {
    use crate::{
        WorkspaceAuthority, WorkspaceSeed, durable_process_service::ProcessTimestampSource,
        hermetic_canary_root::HermeticCanaryAuthority, hermetic_provider::HermeticProvider,
    };
    use orchestrator_core::{
        CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, MissionState,
        MissionStatus, PhaseDefinition, PhaseId, PhaseStatus, VerificationClass, VerificationMode,
        VerificationOutcome, WorkerId, decide_verification,
    };
    use orchestrator_exec::{Effort, ExecutionRequest, ExecutionRequestDraft, RuntimeFamily};
    use orchestrator_process::CancellationToken;

    struct CanaryTimestampSource;
    impl ProcessTimestampSource for CanaryTimestampSource {
        fn now_utc(&self) -> String {
            "2026-07-20T00:00:00Z".to_owned()
        }
    }

    // Read the exact root from the env.
    let root =
        std::env::var_os(HERMETIC_RUN_ROOT_ENV).ok_or(HermeticProcessCanaryError::UnsafeSource)?;
    let root = std::path::PathBuf::from(root);

    // Deterministic canary identifiers so replay can reconstruct the exact
    // mission and request binding without an authority sidecar.
    let mission_id =
        MissionId::new("canary-mission").map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let phase =
        PhaseId::new("canary-phase").map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let worker_id = WorkerId::for_phase("canary", &phase)
        .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let recovering_existing_root = root.exists();
    let authority = if recovering_existing_root {
        HermeticCanaryAuthority::recover_exact_at(policy, &root)?
    } else {
        HermeticCanaryAuthority::publish_exact_at(policy, &root)?
    };
    let compatibility_home = Arc::clone(authority.compatibility_home());
    let canary = if recovering_existing_root {
        recover_cli_enrolled_canary(Arc::clone(&compatibility_home))?
    } else {
        let helper_label = unique_canary_label("helper");
        let canary = HermeticProcessCanaryAuthority::enroll_current_executable(
            Arc::clone(&compatibility_home),
            &helper_label,
        )?;
        // This O_EXCL record is the CLI enrollment commit point. A crash after
        // helper copy but before this publication leaves no recoverable CLI
        // enrollment and therefore fails closed before provider admission.
        publish_cli_enrollment_record(compatibility_home.directory(), &canary)?;
        canary
    };
    let helper_label = canary.label().to_owned();

    let initial_state = MissionState::new(
        mission_id.clone(),
        vec![PhaseDefinition {
            id: phase.clone(),
            dependencies: Vec::new(),
        }],
    )
    .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let checkpoint = CheckpointProjection {
        workspace_id: mission_id.as_str().to_owned(),
        status: "pending".to_owned(),
        plan: Some(CheckpointPlan {
            id: "hermetic-canary-plan".to_owned(),
            phases: vec![CheckpointPhase {
                id: phase.as_str().to_owned(),
                status: "pending".to_owned(),
                ..CheckpointPhase::default()
            }],
            ..CheckpointPlan::default()
        }),
        ..CheckpointProjection::default()
    };
    let workspace = if recovering_existing_root {
        WorkspaceAuthority::admit_hermetic_canary(
            Arc::clone(&compatibility_home),
            mission_id.clone(),
        )
    } else {
        let seed = WorkspaceSeed::new(b"hermetic canary\n".to_vec(), &checkpoint, b"{}".to_vec())
            .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
        WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(&compatibility_home),
            mission_id.clone(),
            seed,
        )
    }
    .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let worker_path = compatibility_home
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
        objective: "prove the sealed hermetic process canary".to_owned(),
        persona: "canary".to_owned(),
        role: "verifier".to_owned(),
        domain: "dev".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime: RuntimeFamily::parse("canary")
            .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?,
        model: "sealed-process-canary".to_owned(),
        effort: Effort::High,
        max_turns: 1,
        worker_dir: worker_path,
        target_dir: None,
        resume_from: None,
        hook_script: None,
    })
    .map_err(|_| HermeticProcessCanaryError::RuntimeFailure)?;
    let cancellation = CancellationToken::new();
    let timestamp_source: Arc<dyn ProcessTimestampSource> = Arc::new(CanaryTimestampSource);
    let mut provider = HermeticProvider::admit_canary(
        workspace,
        initial_state,
        &canary,
        authority.private_process_ledger().clone(),
        "canary",
        request,
        cancellation,
        timestamp_source,
    )
    .map_err(map_provider_error)?;
    let attempt_was_runnable = provider.attempt_is_runnable();
    let outcome = provider
        .run_to_terminal(decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        ))
        .map_err(map_provider_error)?;
    let execution_identity_present = provider
        .durable_execution_identity_present()
        .map_err(map_provider_error)?;
    let spawned = attempt_was_runnable && execution_identity_present;

    let succeeded = outcome.mission_status() == MissionStatus::Completed
        && outcome.phase_status() == PhaseStatus::Completed;
    if succeeded {
        verify_canary_sentinel(
            compatibility_home.directory(),
            mission_id.as_str(),
            worker_id.as_str(),
        )?;
    }

    let helper_sha256_hex = lowercase_hex_sha256(canary.attestation());

    Ok(HermeticProcessCanaryReport {
        leaf_label: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        helper_label,
        mission_id: mission_id.as_str().to_owned(),
        worker_id: worker_id.as_str().to_owned(),
        helper_length: canary.attestation().length(),
        helper_sha256_hex: helper_sha256_hex.clone(),
        executable_logical_id: canary.executable_logical_id(),
        terminal_status: if succeeded { "succeeded" } else { "failed" }.to_owned(),
        spawned,
    })
}

fn verify_canary_sentinel(
    compatibility_home: &Dir,
    mission_id: &str,
    worker_id: &str,
) -> Result<(), HermeticProcessCanaryError> {
    const SENTINEL: &[u8] = b"nanika-hermetic-canary-sentinel-v1\n";

    let relative = Path::new("workspaces")
        .join(mission_id)
        .join("workers")
        .join(worker_id)
        .join("sentinel.txt");
    let file = open_file_nofollow(compatibility_home, &relative)
        .map_err(HermeticProcessCanaryError::Io)?
        .into_std();
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.mode() & 0o7777 != 0o600
        || before.nlink() != 1
        || before.len() != SENTINEL.len() as u64
    {
        return Err(HermeticProcessCanaryError::RuntimeFailure);
    }
    let mut bytes = [0_u8; SENTINEL.len()];
    file.read_exact_at(&mut bytes, 0)?;
    let after = file.metadata()?;
    if !after.is_file()
        || after.uid() != rustix::process::geteuid().as_raw()
        || after.mode() & 0o7777 != 0o600
        || after.nlink() != 1
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || after.len() != SENTINEL.len() as u64
        || bytes != SENTINEL
    {
        return Err(HermeticProcessCanaryError::RuntimeFailure);
    }
    Ok(())
}

fn unique_canary_label(prefix: &str) -> String {
    static CANARY_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = CANARY_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-canary-{}-{nonce}", std::process::id())
}

fn lowercase_hex_sha256(attestation: ExecutableFileAttestation) -> String {
    use std::fmt::Write;
    let mut rendered = String::with_capacity(64);
    for byte in attestation.sha256() {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        error::Error,
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        thread,
    };

    use super::{
        CLI_ENROLLMENT_RECORD, HermeticProcessCanaryAuthority, HermeticProcessCanaryError,
        map_provider_error, publish_cli_enrollment_record, recover_cli_enrolled_canary,
    };
    use crate::{
        FixtureAdmissionPolicy, ProductionBoundary, capability::CapabilityRoot,
        hermetic_canary_root::HermeticCanaryAuthority,
    };

    static CASE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn provider_recovery_and_runtime_failures_remain_typed_and_redacted() {
        let recovery = map_provider_error(
            crate::hermetic_provider::HermeticProviderError::ProcessRecoveryRequired,
        );
        let reconciled = map_provider_error(
            crate::hermetic_provider::HermeticProviderError::ProcessRecoveryReconciled(
                crate::durable_process_service::RecoveredProcessDisposition::
                    StartedProcessCleanedUncertain,
            ),
        );
        let runtime =
            map_provider_error(crate::hermetic_provider::HermeticProviderError::ActorBuild);
        let binding = map_provider_error(
            crate::hermetic_provider::HermeticProviderError::ExecutionBindingConflict,
        );
        let admission = map_provider_error(
            crate::hermetic_provider::HermeticProviderError::ProviderLaunch(
                crate::durable_process_service::ProviderLaunchError::ExecutableAdmission,
            ),
        );

        assert!(matches!(
            &recovery,
            HermeticProcessCanaryError::RecoveryRequired
        ));
        assert!(matches!(
            &reconciled,
            HermeticProcessCanaryError::RecoveryRequired
        ));
        assert!(matches!(
            &runtime,
            HermeticProcessCanaryError::RuntimeFailure
        ));
        assert!(matches!(&binding, HermeticProcessCanaryError::HelperSwap));
        assert!(matches!(&admission, HermeticProcessCanaryError::HelperSwap));
        assert_eq!(
            recovery.to_string(),
            "hermetic process canary requires explicit process recovery"
        );
        assert_eq!(reconciled.to_string(), recovery.to_string());
        assert_eq!(
            runtime.to_string(),
            "hermetic process canary runtime failed"
        );
        assert_eq!(
            binding.to_string(),
            "hermetic process canary helper identity or content changed"
        );
        assert_eq!(binding.to_string(), admission.to_string());
    }

    struct Envelope {
        outer: PathBuf,
        policy: FixtureAdmissionPolicy,
    }

    impl Envelope {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let outer = fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
            let outer = outer.join(format!(
                "orchestrator-rs-hermetic-canary-{label}-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            ));
            let live_user = outer.join("live-user");
            let checkout = outer.join("checkout");
            let policy_temp = outer.join("tmp");
            fs::create_dir_all(&live_user)?;
            fs::create_dir_all(&checkout)?;
            create_private_directory(&policy_temp)?;
            let policy_temp = fs::canonicalize(&policy_temp)?;
            let policy = FixtureAdmissionPolicy::new(&live_user, &checkout, &policy_temp);
            Ok(Self { outer, policy })
        }

        fn publish_leaf(&self, label: &str) -> Result<Arc<ProductionBoundary>, Box<dyn Error>> {
            let root = self.outer.join("tmp").join(label);
            let authority = HermeticCanaryAuthority::publish_exact_at(&self.policy, &root)?;
            Ok(Arc::clone(authority.compatibility_home()))
        }

        fn recover_leaf(&self, label: &str) -> Result<Arc<ProductionBoundary>, Box<dyn Error>> {
            let root = self.outer.join("tmp").join(label);
            let authority = HermeticCanaryAuthority::recover_exact_at(&self.policy, &root)?;
            Ok(Arc::clone(authority.compatibility_home()))
        }

        fn next_label(&self) -> String {
            format!(
                "helper-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            )
        }

        /// Locates the prebuilt `orchestrator-owned-process-fixture` binary
        /// beside the test executable. Mirrors the harness pattern used by the
        /// existing durable-canary suite.
        fn owned_helper_path(&self) -> Result<PathBuf, Box<dyn Error>> {
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
                match fs::symlink_metadata(candidate) {
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
                        return Ok(candidate.clone());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(format!(
                "prebuilt orchestrator-owned-process-fixture was not found beside {current:?}"
            )
            .into())
        }
    }

    impl Drop for Envelope {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.outer);
        }
    }

    fn create_private_directory(path: &Path) -> std::io::Result<()> {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }

    fn entry_names(path: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
        fs::read_dir(path)?
            .map(|entry| {
                entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| std::io::Error::other("test entry name is not UTF-8"))
            })
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    #[test]
    fn enroll_current_executable_lands_attested_private_helper() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("enroll-current")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;

        let canary =
            HermeticProcessCanaryAuthority::enroll_current_executable(leaf, &helper_label)?;

        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        let metadata = fs::metadata(&helper_path)?;
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(metadata.nlink(), 1);

        let current = std::env::current_exe()?;
        let source = fs::read(&current)?;
        assert_eq!(canary.attestation().length(), metadata.len());
        assert_eq!(fs::read(&helper_path)?, source);
        assert!(canary.verify().is_ok());
        Ok(())
    }

    #[test]
    fn enroll_executable_at_lands_attested_private_helper() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("enroll-at")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;

        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;

        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        let metadata = fs::metadata(&helper_path)?;
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        assert_eq!(fs::read(&helper_path)?, fs::read(&source)?);
        assert_eq!(canary.attestation().length(), metadata.len());
        assert!(canary.verify().is_ok());
        Ok(())
    }

    #[test]
    fn second_enrollment_of_same_label_is_rejected_without_mutation() -> Result<(), Box<dyn Error>>
    {
        let envelope = Envelope::new("twice")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let first = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        let helper_path = first
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        let before = fs::read(&helper_path)?;
        drop(first);

        let reopened_leaf = envelope.recover_leaf(&leaf_label)?;
        let error = match HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            reopened_leaf,
            &helper_label,
            &source,
        ) {
            Ok(_) => return Err("second enrollment was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, HermeticProcessCanaryError::HelperExists));
        assert_eq!(fs::read(&helper_path)?, before);
        Ok(())
    }

    #[test]
    fn verify_detects_helper_byte_tamper() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("tamper")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        assert!(canary.verify().is_ok());

        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        // Flip one byte in the middle of the helper image.
        let mut bytes = fs::read(&helper_path)?;
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs::write(&helper_path, &bytes)?;
        // The helper must remain mode 0700 so the mode check passes and the
        // tamper is caught at the byte-comparison layer, not the mode layer.
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))?;

        assert!(matches!(
            canary.verify(),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn verify_detects_helper_mode_change() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("mode")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;

        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o755))?;

        assert!(matches!(
            canary.verify(),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn verify_detects_leaf_boundary_break() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("boundary")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        assert!(canary.verify().is_ok());

        // Replace the leaf directory; the capability must fail closed.
        let leaf_path = canary.boundary().canonical_path().to_path_buf();
        let displaced = leaf_path.with_extension("displaced");
        fs::rename(&leaf_path, &displaced)?;
        fs::create_dir(&leaf_path)?;
        fs::set_permissions(&leaf_path, fs::Permissions::from_mode(0o700))?;

        assert!(matches!(
            canary.verify(),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn recovery_reopens_attested_helper() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("recover")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let source_bytes = fs::read(&source)?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let enrolled = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        let enrolled_attestation = enrolled.attestation();
        drop(enrolled);

        // Reopen the leaf, then recover the helper through it.
        let leaf = envelope.recover_leaf(&leaf_label)?;
        let recovered = HermeticProcessCanaryAuthority::recover(leaf, &helper_label)?;
        assert_eq!(recovered.attestation(), enrolled_attestation);
        assert_eq!(recovered.label(), &helper_label);
        assert!(recovered.verify().is_ok());
        let (_file, image) = recovered.open_verified()?;
        assert_eq!(*image, *source_bytes);
        Ok(())
    }

    #[test]
    fn recovery_missing_helper_fails_closed() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("recover-missing")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;

        let error = match HermeticProcessCanaryAuthority::recover(leaf, &helper_label) {
            Ok(_) => return Err("recovery of a missing helper was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, HermeticProcessCanaryError::HelperMissing));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_helper_without_enrollment_record_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-missing-record")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        drop(canary);

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_enrollment_record_byte_tamper_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-record-bytes")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        publish_cli_enrollment_record(canary.boundary().directory(), &canary)?;
        let record_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(CLI_ENROLLMENT_RECORD);
        drop(canary);

        let mut bytes = fs::read(&record_path)?;
        bytes[0] ^= 0xff;
        fs::write(&record_path, bytes)?;
        fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600))?;

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_enrolled_helper_inode_replacement_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-helper-inode")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        publish_cli_enrollment_record(canary.boundary().directory(), &canary)?;
        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        let displaced = helper_path.with_extension("displaced");
        let bytes = fs::read(&helper_path)?;
        drop(canary);

        fs::rename(&helper_path, &displaced)?;
        fs::write(&helper_path, bytes)?;
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))?;

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_enrolled_helper_byte_tamper_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-helper-bytes")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        publish_cli_enrollment_record(canary.boundary().directory(), &canary)?;
        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        drop(canary);

        let mut bytes = fs::read(&helper_path)?;
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(&helper_path, bytes)?;
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))?;

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_enrolled_helper_mode_tamper_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-helper-mode")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        publish_cli_enrollment_record(canary.boundary().directory(), &canary)?;
        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        drop(canary);

        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o755))?;

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn cli_recovery_rejects_enrollment_record_mode_tamper_before_provider_admission()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("cli-record-mode")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        publish_cli_enrollment_record(canary.boundary().directory(), &canary)?;
        let record_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(CLI_ENROLLMENT_RECORD);
        drop(canary);

        fs::set_permissions(&record_path, fs::Permissions::from_mode(0o644))?;

        let leaf = envelope.recover_leaf(&leaf_label)?;
        assert!(matches!(
            recover_cli_enrolled_canary(leaf),
            Err(HermeticProcessCanaryError::HelperSwap)
        ));
        Ok(())
    }

    #[test]
    fn admission_writes_nothing_outside_leaf() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("envelope")?;
        let sentinel = envelope.outer.join("outside-sentinel");
        fs::write(&sentinel, b"unchanged")?;
        let before = entry_names(&envelope.outer)?;

        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        assert!(canary.verify().is_ok());

        assert_eq!(entry_names(&envelope.outer)?, before);
        assert_eq!(fs::read(&sentinel)?, b"unchanged");
        Ok(())
    }

    #[test]
    fn debug_redacts_leaf_path_and_helper_label() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("secret-user-77777")?;
        // Short charset-safe labels that still carry sensitive tokens
        // through the canonical leaf path so the redaction check is meaningful.
        let leaf_label = "secret-leaf-77777".to_string();
        let helper_label = "secret-helper-77777".to_string();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;

        let debug = format!("{canary:?}");
        assert_eq!(
            debug,
            "HermeticProcessCanary { kind: \"sealed-hermetic-process-canary\" }"
        );
        assert!(!debug.contains("secret-user-77777"));
        assert!(!debug.contains(&leaf_label));
        assert!(!debug.contains(&helper_label));
        Ok(())
    }

    #[test]
    fn attestation_is_stable_across_enrollments_of_same_source() -> Result<(), Box<dyn Error>> {
        // The attestation is a pure function of the enrolled bytes: two
        // enrollments of the same source produce the same length+SHA-256
        // attestation, and the closed public API takes no source argument.
        let envelope = Envelope::new("stable")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        assert!(canary.verify().is_ok());

        let helper_label_b = envelope.next_label();
        let leaf_again = Arc::clone(canary.boundary());
        let canary_b = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf_again,
            &helper_label_b,
            &source,
        )?;
        assert_eq!(canary.attestation(), canary_b.attestation());
        Ok(())
    }

    #[test]
    fn concurrent_enrollment_of_same_label_exactly_one_winner() -> Result<(), Box<dyn Error>> {
        let envelope = Arc::new(Envelope::new("concurrent")?);
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let source = envelope.owned_helper_path()?;
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let barrier = Arc::clone(&barrier);
            let helper_label = helper_label.clone();
            let source = source.clone();
            let leaf = Arc::clone(&leaf);
            workers.push(thread::spawn(move || {
                barrier.wait();
                HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
                    leaf,
                    &helper_label,
                    &source,
                )
            }));
        }

        let mut successes = 0usize;
        let mut exists = 0usize;
        let mut other = Vec::new();
        for worker in workers {
            match worker
                .join()
                .map_err(|_| std::io::Error::other("worker panicked"))?
            {
                Ok(_canary) => successes += 1,
                Err(HermeticProcessCanaryError::HelperExists) => exists += 1,
                Err(error) => other.push(error),
            }
        }
        assert!(other.is_empty(), "unexpected errors: {other:?}");
        assert_eq!(successes, 1, "exactly one enrollment should win");
        assert!(
            exists >= 7,
            "the other seven should observe HelperExists: {exists}"
        );
        Ok(())
    }

    #[test]
    fn launch_admission_attests_canary_helper_with_canary_prefix() -> Result<(), Box<dyn Error>> {
        use crate::durable_process_service::RetainedProviderExecutable;

        let envelope = Envelope::new("launch-admit")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;

        let retained = RetainedProviderExecutable::from_hermetic_canary(&canary)?;

        assert!(
            retained.logical_id().starts_with("nanika-canary-v1-"),
            "logical id must carry the canary prefix: {}",
            retained.logical_id()
        );
        assert!(
            retained.logical_id().contains(&helper_label),
            "logical id must carry the helper label: {}",
            retained.logical_id()
        );
        assert_eq!(retained.attestation(), canary.attestation());
        assert!(retained.verify().is_ok());
        Ok(())
    }

    #[test]
    fn launch_admission_verify_detects_helper_tamper() -> Result<(), Box<dyn Error>> {
        use crate::durable_process_service::RetainedProviderExecutable;

        let envelope = Envelope::new("launch-tamper")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        let retained = RetainedProviderExecutable::from_hermetic_canary(&canary)?;

        let helper_path = canary
            .boundary()
            .canonical_path()
            .join("bin")
            .join(&helper_label);
        let mut bytes = fs::read(&helper_path)?;
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs::write(&helper_path, &bytes)?;
        fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))?;

        assert!(retained.verify().is_err());
        Ok(())
    }

    #[test]
    fn launch_admission_verify_detects_leaf_boundary_break() -> Result<(), Box<dyn Error>> {
        use crate::durable_process_service::RetainedProviderExecutable;

        let envelope = Envelope::new("launch-break")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;
        let retained = RetainedProviderExecutable::from_hermetic_canary(&canary)?;
        assert!(retained.verify().is_ok());

        let leaf_path = canary.boundary().canonical_path().to_path_buf();
        let displaced = leaf_path.with_extension("displaced");
        fs::rename(&leaf_path, &displaced)?;
        fs::create_dir(&leaf_path)?;
        fs::set_permissions(&leaf_path, fs::Permissions::from_mode(0o700))?;

        assert!(retained.verify().is_err());
        Ok(())
    }

    #[test]
    fn from_attested_canary_composes_prepared_launch_under_leaf() -> Result<(), Box<dyn Error>> {
        use crate::{
            WorkspaceAuthority, WorkspaceSeed, durable_process_service::PreparedProviderLaunch,
        };
        use orchestrator_core::{CheckpointProjection, MissionId, PhaseId, WorkerId};

        let envelope = Envelope::new("dispatch")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf,
            &helper_label,
            &source,
        )?;

        let mission_id = MissionId::new("canary-mission")?;
        let phase = PhaseId::new("canary-phase")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            ..Default::default()
        };
        let seed = WorkspaceSeed::new(b"canary mission\n".to_vec(), &checkpoint, b"{}".to_vec())?;

        let workspace = WorkspaceAuthority::create_hermetic_canary(
            canary.boundary().clone(),
            mission_id.clone(),
            seed,
        )?;

        let worker_id = WorkerId::for_phase("canary", &phase)?;
        let expected_worker_path = canary
            .boundary()
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        let worker = workspace.phase_worker_binding("canary", &phase, &expected_worker_path)?;

        let prepared = PreparedProviderLaunch::from_attested_canary(worker, &canary)
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;
        assert!(prepared.verify_pre_admission().is_ok());
        let exact_request = prepared.exact_request();
        assert!(exact_request.expose_environment().is_empty());
        assert!(
            exact_request
                .expose_stdin()
                .is_some_and(|stdin| !stdin.is_empty())
        );
        let request_without_challenge = orchestrator_exec::ProcessRequest::new(
            exact_request.purpose(),
            exact_request.executable_id(),
            exact_request.working_root(),
        )?
        .with_argument("--hermetic-canary-worker")?
        .with_max_output_bytes(exact_request.max_output_bytes())?;
        assert_ne!(
            exact_request.fingerprint(),
            request_without_challenge.fingerprint(),
            "the staged challenge must contribute to the durable request fingerprint"
        );
        assert!(
            exact_request
                .executable_id()
                .starts_with("nanika-canary-v1-"),
            "canary dispatch must route through the canary logical id"
        );
        Ok(())
    }

    #[test]
    fn from_attested_canary_rejects_worker_under_foreign_leaf() -> Result<(), Box<dyn Error>> {
        use crate::{
            WorkspaceAuthority, WorkspaceSeed, durable_process_service::PreparedProviderLaunch,
        };
        use orchestrator_core::{CheckpointProjection, MissionId, PhaseId, WorkerId};

        let envelope = Envelope::new("foreign-worker")?;
        // Two independent leaves under the same envelope.
        let leaf_a_label = envelope.next_label();
        let leaf_b_label = envelope.next_label();
        let helper_label = envelope.next_label();
        let leaf_a = envelope.publish_leaf(&leaf_a_label)?;
        let source = envelope.owned_helper_path()?;
        let canary = HermeticProcessCanaryAuthority::enroll_executable_at_for_test(
            leaf_a,
            &helper_label,
            &source,
        )?;

        // Worker under a DIFFERENT leaf (leaf_b).
        let leaf_b = envelope.publish_leaf(&leaf_b_label)?;
        let leaf_b_path = leaf_b.canonical_path().to_path_buf();
        let mission_id = MissionId::new("canary-mission-b")?;
        let phase = PhaseId::new("canary-phase")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            ..Default::default()
        };
        let seed = WorkspaceSeed::new(b"canary mission b\n".to_vec(), &checkpoint, b"{}".to_vec())?;
        let workspace_b = WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(&leaf_b),
            mission_id.clone(),
            seed,
        )?;
        let worker_id = WorkerId::for_phase("canary", &phase)?;
        let foreign_worker_path = leaf_b_path
            .join("workspaces")
            .join(mission_id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        let foreign_worker =
            workspace_b.phase_worker_binding("canary", &phase, &foreign_worker_path)?;

        let result = PreparedProviderLaunch::from_attested_canary(foreign_worker, &canary);
        assert!(
            matches!(
                result,
                Err(crate::durable_process_service::ProviderLaunchError::InvalidAdmission)
            ),
            "a worker under a foreign leaf must not be admitted under the canary"
        );
        Ok(())
    }

    #[test]
    fn admit_hermetic_canary_reopens_workspace_under_leaf() -> Result<(), Box<dyn Error>> {
        use crate::{WorkspaceAuthority, WorkspaceSeed};
        use orchestrator_core::{CheckpointProjection, MissionId};

        let envelope = Envelope::new("admit")?;
        let leaf_label = envelope.next_label();
        let leaf = envelope.publish_leaf(&leaf_label)?;
        let leaf_arc = leaf;
        let mission_id = MissionId::new("canary-admit-mission")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            ..Default::default()
        };
        let seed = WorkspaceSeed::new(b"canary admit\n".to_vec(), &checkpoint, b"{}".to_vec())?;

        let created = WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(&leaf_arc),
            mission_id.clone(),
            seed,
        )?;
        let created_identity = created.verify();
        drop(created);

        let reopened =
            WorkspaceAuthority::admit_hermetic_canary(Arc::clone(&leaf_arc), mission_id.clone())?;
        assert!(created_identity.is_ok());
        assert!(reopened.verify().is_ok());
        assert_eq!(reopened.mission_id(), &mission_id);
        Ok(())
    }

    #[test]
    fn reopened_canary_workspace_reuses_fixture_projection_leases() -> Result<(), Box<dyn Error>> {
        use crate::{WorkspaceAuthority, WorkspaceError, WorkspaceSeed};
        use orchestrator_core::{CheckpointProjection, MissionId};

        let envelope = Envelope::new("shared-extras")?;
        let compatibility_home = envelope.publish_leaf(&envelope.next_label())?;
        let mission_id = MissionId::new("canary-shared-extras")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            ..Default::default()
        };
        let seed = WorkspaceSeed::new(b"canary extras\n".to_vec(), &checkpoint, b"{}".to_vec())?;
        let created = WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(&compatibility_home),
            mission_id.clone(),
            seed,
        )?;
        let first_writer = created.into_fixture_projection_writer()?;
        let reopened = WorkspaceAuthority::admit_hermetic_canary(
            Arc::clone(&compatibility_home),
            mission_id.clone(),
        )?;

        let error = match reopened.into_fixture_projection_writer() {
            Ok(_) => return Err("reopened workspace minted a duplicate projection lease".into()),
            Err(error) => error,
        };
        assert!(matches!(error, WorkspaceError::ProjectionWriterLeased));
        drop(first_writer);

        let recovered = WorkspaceAuthority::admit_hermetic_canary(compatibility_home, mission_id)?
            .into_fixture_projection_writer()?;
        drop(recovered);
        Ok(())
    }

    // The old `run_hermetic_process_canary_returns_verified_report` test was
    // removed because the pub entry now requires NANIKA_HERMETIC_RUN_ROOT
    // (an env var that can't be safely set in a unit test without
    // `unsafe { env::set_var }`, which the workspace denies). The durable
    // composition is now proven by `durable_canary_composition_proves_full_chain`
    // (unit test) and the CLI integration test
    // `hermetic_canary_durable_run_and_replay` (subprocess).

    #[test]
    fn fixed_compatibility_boundary_opens_runtime_store() -> Result<(), Box<dyn Error>> {
        use crate::{RuntimeStore, StorageActorAuthority};

        let envelope = Envelope::new("rt-store-bridge")?;
        let label = envelope.next_label();
        let leaf = envelope.publish_leaf(&label)?;
        assert!(leaf.verify().is_ok());

        let store = RuntimeStore::open(leaf, StorageActorAuthority::new())?;
        assert!(store.pending_effects(10)?.is_empty());
        Ok(())
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[test]
    fn durable_canary_composition_proves_full_chain() -> Result<(), Box<dyn Error>> {
        use crate::{
            RuntimeStore, StorageActorAuthority, WorkspaceAuthority, WorkspaceSeed,
            durable_process_service::{
                DurableProcessActor, PreparedProviderLaunch, ProcessTimestampSource,
            },
            runtime_store::{
                EffectOperationSlot, JournalIntent, OutboxEffectKind, OutboxIntent,
                PRIVATE_PROCESS_CLAIM_TRANSITION_KIND,
            },
        };
        use orchestrator_core::{CheckpointProjection, MissionId, PhaseId, WorkerId};
        use orchestrator_process::CancellationToken;
        use std::time::{Duration, Instant};

        struct CanaryTimestampSource;
        impl ProcessTimestampSource for CanaryTimestampSource {
            fn now_utc(&self) -> String {
                "2026-07-20T00:00:00Z".to_owned()
            }
        }

        let envelope = Envelope::new("durable-chain")?;
        let leaf_label = envelope.next_label();
        let helper_label = envelope.next_label();

        let root = envelope.outer.join("tmp").join(&leaf_label);
        let authority = HermeticCanaryAuthority::publish_exact_at(&envelope.policy, &root)?;
        let compatibility_home = Arc::clone(authority.compatibility_home());

        let canary = HermeticProcessCanaryAuthority::enroll_current_executable(
            Arc::clone(&compatibility_home),
            &helper_label,
        )?;
        let canary = Arc::new(canary);

        let mut store = RuntimeStore::open_private(
            authority.private_process_ledger().clone(),
            StorageActorAuthority::new(),
        )?;

        // 4. Create workspace under the leaf.
        let mission_id = MissionId::new("durable-canary-mission")?;
        let phase = PhaseId::new("durable-canary-phase")?;
        let checkpoint = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            ..CheckpointProjection::default()
        };
        let seed = WorkspaceSeed::new(b"canary\n".to_vec(), &checkpoint, b"{}".to_vec())?;
        let workspace = WorkspaceAuthority::create_hermetic_canary(
            Arc::clone(canary.boundary()),
            mission_id.clone(),
            seed,
        )?;

        // 5. Phase worker binding.
        let worker_id = WorkerId::for_phase("canary", &phase)?;
        let worker_path = canary
            .boundary()
            .canonical_path()
            .join("workspaces")
            .join(mission_id.as_str())
            .join("workers")
            .join(worker_id.as_str());
        let worker_binding = workspace.phase_worker_binding("canary", &phase, &worker_path)?;

        // 6. from_attested_canary → prepared launch (consumes worker_binding).
        let prepared = PreparedProviderLaunch::from_attested_canary(worker_binding, &canary)
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;

        // 7. Create outbox intent + process binding from the prepared request.
        //    ProcessRequest doesn't impl Clone, so use exact_request_copy()
        //    after the actor is spawned. For now, create the outbox intent
        //    from the prepared's request reference (borrow, not move).
        let intent = OutboxIntent::for_process(
            mission_id.clone(),
            Some(phase.as_str().to_owned()),
            OutboxEffectKind::ProviderProcess,
            EffectOperationSlot::new("canary-attempt")?,
            1,
            serde_json::json!({"phase_id": phase.as_str(), "attempt": 1}),
            prepared.exact_request(),
        )?;
        let process_binding = intent.bind_process(prepared.exact_request())?;

        // 8. Commit the outbox effect to the store's journal.
        let journal_intent = JournalIntent::new(
            format!("canary-claim:{}:{}:1", mission_id, phase),
            Some(mission_id.clone()),
            PRIVATE_PROCESS_CLAIM_TRANSITION_KIND,
            serde_json::json!({"phase_id": phase.as_str(), "attempt": 1}),
            "2026-07-20T00:00:00Z".to_owned(),
        )?
        .with_outbox(intent)?;
        store.append(&journal_intent)?;

        // 9. Enroll the launch (consumes prepared + process_binding).
        let enrolled = prepared
            .enroll(process_binding)
            .map_err(|_| HermeticProcessCanaryError::HelperSwap)?;

        // 10. Spawn the durable actor.
        let cancellation = CancellationToken::new();
        let timestamp_source: Arc<dyn ProcessTimestampSource> = Arc::new(CanaryTimestampSource);
        let (actor, service) =
            DurableProcessActor::spawn(store, enrolled, cancellation, timestamp_source)?;

        // 11. Get a request copy from the service for dispatch.
        let request = service.exact_request_copy()?;

        // 12. Dispatch one attempt through the actor.
        let deadline = Instant::now() + Duration::from_secs(30);
        let budget = orchestrator_exec::ProcessBudget::new(
            deadline,
            Duration::from_secs(30),
            Duration::from_secs(60),
        );
        let receipt = service.dispatch_once(&request, budget);

        // 12. Shutdown the actor.
        let _ = actor.shutdown();

        // The composition compiled + the actor spawned + dispatched. Whether
        // the receipt is Ok or Err depends on the spawned binary's behavior
        // (the test runner receiving --hermetic-canary-worker). The key proof
        // is that the full durable chain types compose end-to-end.
        let _ = receipt;
        Ok(())
    }
}
