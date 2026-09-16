#![cfg(all(unix, feature = "verification-process-canary"))]
// The sealed exact-leaf primitive is staged for the following verification-
// canary composition cell (`hermetic_process_canary.rs`). It has no non-test
// consumer yet; the dead-code expectation documents that and fails closed if
// the primitive is wired up without removing this gate.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the sealed exact-leaf primitive is staged for the hermetic process canary"
    )
)]

//! Atomic exact-name publisher for sealed fixture authority.
//!
//! A private, feature-gated primitive that publishes a single mode-`0700`
//! directory at an exact, caller-named leaf beneath canonical temporary
//! storage. Publication is atomic and fail-closed:
//!
//! 1. the admission policy is prevalidated before any filesystem mutation;
//! 2. an unpredictable staging sibling is created via no-follow `mkdir`;
//! 3. the staging directory is atomically published at the final leaf name
//!    with `renameat_with(..., RENAME_EXCL)`, which fails if the destination
//!    already exists and never overwrites it;
//! 4. the canonical final path is reopened no-follow and its identity is
//!    compared to the staging inode retained before publication, detecting any
//!    mid-flight swap of the leaf, its parent, or the staging name itself;
//! 5. the parent directory is synchronized before and after publication.
//!
//! On any rejection the existing leaf is never mutated or deleted by an
//! unverified path (ADR-0001 §1: "Recovery never deletes such a prefix unless
//! later state proves this implementation owns it"). Failed publication may
//! preserve an unpredictable, authority-marked staging sibling: the available
//! pathname removal API cannot atomically bind deletion to the retained inode.
//!
//! Every diagnostic redacts the canonical path, device, inode, and temporary
//! prefix. The capability is non-cloneable and its raw directory handle never
//! leaves this crate.
//!
//! This publication transition does not claim to prevent a cold, same-UID
//! replacement after every retained authority has been dropped. Read-only
//! recovery verifies the stable v1 layout; stronger cold-substitution proof is
//! outside this slice.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use cap_std::fs::Dir;
use rustix::fs::{RenameFlags, renameat_with};
use thiserror::Error;

use crate::{
    fixture_authority::{
        FixtureAdmissionPolicy, FixtureAuthorityError, FixturePreparationPoint,
        FreshFixtureAuthority, UnpublishedFixtureAuthority, validate_policy_boundary,
    },
    fs_util::{
        FileIdentity, create_dir_private, identity, mode, open_dir_path_nofollow, sync_dir,
        validate_component,
    },
    runtime_home::open_canonical_directory,
};

/// Maximum byte length of a caller-supplied exact-leaf label.
const MAX_LABEL_BYTES: usize = 64;

/// Failures reported while publishing or recovering an exact leaf.
///
/// Every variant is deliberately path- and identity-free: the canonical leaf
/// path, device, inode, owner, and temporary prefix never appear in the
/// [`core::fmt::Display`] or [`core::fmt::Debug`] rendering of an error.
#[derive(Debug, Error)]
pub(crate) enum ExactLeafError {
    /// The caller-supplied leaf label failed charset or length validation.
    #[error("exact-leaf label is invalid")]
    InvalidLabel,
    /// The prospective leaf is not contained by both the policy and the
    /// process temporary directories.
    #[error("exact-leaf boundary is outside canonical temporary directory")]
    OutsideTemp,
    /// The prospective leaf overlaps a live home, repository checkout, runtime
    /// candidate, or explicitly forbidden root. `class` is a stable, non-
    /// sensitive category label.
    #[error("exact-leaf boundary overlaps forbidden root class {class}")]
    ForbiddenOverlap { class: &'static str },
    /// The exact leaf already exists; publication was refused without mutation.
    #[error("exact-leaf already exists")]
    LeafExists,
    /// The exact leaf was not present when recovery attempted to reopen it.
    #[error("exact-leaf is missing")]
    LeafMissing,
    /// The exact leaf reachable at the canonical path is not the inode this
    /// authority published or admitted. Covers mid-flight swap of the leaf,
    /// its parent, or the staging name, plus any post-publication tamper
    /// detected by the retained fixture boundary.
    #[error("exact-leaf identity changed during publication or use")]
    LeafSwap,
    /// The exact leaf exists but its mode, type, or owner is unsafe.
    #[error("exact-leaf mode, type, or owner is unsafe")]
    UnsafeLeaf,
    /// A filesystem operation against the canonical temporary directory failed.
    /// The wrapped [`std::io::Error`] carries no caller-supplied path.
    #[error("exact-leaf capability filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// The stable fixture authority rejected the prepared or recovered root.
    #[error("exact-leaf fixture authority admission failed: {0}")]
    FixtureAuthority(#[source] FixtureAuthorityError),
}

/// Authority that publishes and recovers exact leaves beneath canonical temp.
///
/// Stateless; kept as a unit struct so the publication surface reads as a
/// named capability rather than a bag of free functions.
pub(crate) struct ExactLeafAuthority;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationPoint {
    MarkerCreatedBeforeIdentityRetention,
    LockCreatedBeforeIdentityRetention,
    Prepared,
    Published,
}

impl fmt::Debug for ExactLeafAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactLeafAuthority")
            .field("kind", &"sealed-exact-leaf")
            .finish()
    }
}

impl ExactLeafAuthority {
    /// Publishes a fresh exact leaf named `leaf_label` beneath the canonical
    /// temporary directory recorded in `policy`.
    pub(crate) fn publish(
        policy: &FixtureAdmissionPolicy,
        leaf_label: &str,
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        let final_name = validate_label(leaf_label)?;
        let policy_temp = std::fs::canonicalize(policy.temporary_directory())?;
        if !policy_temp.is_absolute() {
            return Err(ExactLeafError::OutsideTemp);
        }
        let parent = open_canonical_directory(&policy_temp)?;
        let canonical_final = policy_temp.join(final_name);
        Self::publish_core(policy, &parent, final_name, canonical_final, |_, _| {})
    }

    /// Publishes at an exact root path (the `NANIKA_HERMETIC_RUN_ROOT`
    /// contract: the path denotes the exact leaf, not a parent for a generated
    /// child). The path's parent must exist; the path itself must not.
    #[allow(dead_code, reason = "staged for Commit E CLI canary exposure")]
    pub(crate) fn publish_at(
        policy: &FixtureAdmissionPolicy,
        exact_root: &Path,
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        let canonical_parent =
            std::fs::canonicalize(exact_root.parent().ok_or(ExactLeafError::OutsideTemp)?)?;
        if !canonical_parent.is_absolute() {
            return Err(ExactLeafError::OutsideTemp);
        }
        let final_name = exact_root.file_name().ok_or(ExactLeafError::InvalidLabel)?;
        let parent = open_canonical_directory(&canonical_parent)?;
        let canonical_final = canonical_parent.join(final_name);
        Self::publish_core(
            policy,
            &parent,
            Path::new(final_name),
            canonical_final,
            |_, _| {},
        )
    }

    #[cfg(test)]
    fn publish_at_with_hook(
        policy: &FixtureAdmissionPolicy,
        exact_root: &Path,
        hook: impl FnMut(PublicationPoint, &Path),
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        let canonical_parent =
            std::fs::canonicalize(exact_root.parent().ok_or(ExactLeafError::OutsideTemp)?)?;
        let final_name = exact_root.file_name().ok_or(ExactLeafError::InvalidLabel)?;
        let parent = open_canonical_directory(&canonical_parent)?;
        Self::publish_core(
            policy,
            &parent,
            Path::new(final_name),
            canonical_parent.join(final_name),
            hook,
        )
    }

    fn publish_core(
        policy: &FixtureAdmissionPolicy,
        parent: &Dir,
        final_name: &Path,
        canonical_final: PathBuf,
        mut hook: impl FnMut(PublicationPoint, &Path),
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        validate_policy_boundary(&canonical_final, policy).map_err(map_boundary_error)?;

        // Probe (no-follow) so the common existing-leaf case avoids staging
        // churn. The authoritative existence check is the NOREPLACE rename
        // below, which wins under any concurrent race.
        if parent.symlink_metadata(final_name).is_ok() {
            return Err(ExactLeafError::LeafExists);
        }

        let staging_name = staging_name_os_random()?;
        let staging_path = Path::new(&staging_name);

        let (staging_directory, staging_identity, parent_fd) =
            create_and_verify_staging(parent, staging_path)?;
        let staging_canonical = canonical_final
            .parent()
            .ok_or(ExactLeafError::OutsideTemp)?
            .join(staging_path);
        let prepared = match UnpublishedFixtureAuthority::prepare_with_hook(
            &staging_canonical,
            &canonical_final,
            staging_directory,
            policy,
            |point| {
                hook(
                    match point {
                        FixturePreparationPoint::MarkerCreatedBeforeIdentityRetention => {
                            PublicationPoint::MarkerCreatedBeforeIdentityRetention
                        }
                        FixturePreparationPoint::LockCreatedBeforeIdentityRetention => {
                            PublicationPoint::LockCreatedBeforeIdentityRetention
                        }
                    },
                    staging_path,
                );
            },
        ) {
            Ok(prepared) => prepared,
            Err(error) => return Err(map_boundary_error(error)),
        };

        // Re-verify the staging name still resolves (no-follow) to the inode
        // we created before asking the kernel to rename it. A name swap to a
        // symlink fails the no-follow open; a swap to a different directory
        // fails the identity check. The authoritative protection remains the
        // canonical reopen after rename.
        if prepared
            .verify_named_in_parent(parent, staging_path)
            .is_err()
        {
            return Err(ExactLeafError::LeafSwap);
        }
        hook(PublicationPoint::Prepared, staging_path);

        match renameat_with(
            &parent_fd,
            staging_path,
            &parent_fd,
            final_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(errno) => {
                if errno == rustix::io::Errno::EXIST {
                    return Err(ExactLeafError::LeafExists);
                }
                return Err(ExactLeafError::Io(std::io::Error::from(errno)));
            }
        }
        sync_dir(parent)?;
        hook(PublicationPoint::Published, final_name);

        // The published leaf is reopened through both the retained parent and a
        // fresh canonical walk. Either path catching a different inode than the
        // staging identity proves a mid-flight swap; we fail closed and do not
        // delete the published leaf by an unverified path.
        let leaf_dir = open_dir_path_nofollow(parent, final_name)?;
        let leaf_metadata = leaf_dir.dir_metadata()?;
        if !is_safe_exact_leaf_directory(&leaf_metadata)
            || identity(&leaf_metadata) != staging_identity
        {
            return Err(ExactLeafError::LeafSwap);
        }
        let canonical_dir = open_canonical_directory(&canonical_final)?;
        let canonical_metadata = canonical_dir.dir_metadata()?;
        if !is_safe_exact_leaf_directory(&canonical_metadata)
            || identity(&canonical_metadata) != staging_identity
        {
            return Err(ExactLeafError::LeafSwap);
        }

        prepared.into_published().map_err(map_boundary_error)
    }

    /// Reopens an existing exact leaf without creating or modifying it.
    pub(crate) fn recover(
        policy: &FixtureAdmissionPolicy,
        leaf_label: &str,
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        let final_name = validate_label(leaf_label)?;
        let policy_temp = std::fs::canonicalize(policy.temporary_directory())?;
        if !policy_temp.is_absolute() {
            return Err(ExactLeafError::OutsideTemp);
        }
        let parent = open_canonical_directory(&policy_temp)?;
        let canonical_final = policy_temp.join(final_name);
        Self::recover_core(policy, &parent, final_name, canonical_final)
    }

    /// Recovers at an exact root path (the `NANIKA_HERMETIC_RUN_ROOT` contract).
    #[allow(dead_code, reason = "staged for Commit E CLI canary exposure")]
    pub(crate) fn recover_at(
        policy: &FixtureAdmissionPolicy,
        exact_root: &Path,
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        let canonical_parent =
            std::fs::canonicalize(exact_root.parent().ok_or(ExactLeafError::OutsideTemp)?)?;
        if !canonical_parent.is_absolute() {
            return Err(ExactLeafError::OutsideTemp);
        }
        let final_name = exact_root.file_name().ok_or(ExactLeafError::InvalidLabel)?;
        let parent = open_canonical_directory(&canonical_parent)?;
        let canonical_final = canonical_parent.join(final_name);
        Self::recover_core(policy, &parent, Path::new(final_name), canonical_final)
    }

    fn recover_core(
        policy: &FixtureAdmissionPolicy,
        parent: &Dir,
        final_name: &Path,
        canonical_final: PathBuf,
    ) -> Result<FreshFixtureAuthority, ExactLeafError> {
        validate_policy_boundary(&canonical_final, policy).map_err(map_boundary_error)?;

        let retained = open_dir_path_nofollow(parent, final_name).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ExactLeafError::LeafMissing
            } else {
                ExactLeafError::Io(source)
            }
        })?;
        let retained_metadata = retained.dir_metadata()?;
        if !is_safe_exact_leaf_directory(&retained_metadata) {
            return Err(ExactLeafError::UnsafeLeaf);
        }
        let retained_identity = identity(&retained_metadata);

        // Reopen via a fresh canonical walk so a swapped parent or leaf is
        // detected independently of the retained parent handle.
        let canonical_dir = open_canonical_directory(&canonical_final)?;
        let canonical_metadata = canonical_dir.dir_metadata()?;
        if !is_safe_exact_leaf_directory(&canonical_metadata)
            || identity(&canonical_metadata) != retained_identity
        {
            return Err(ExactLeafError::LeafSwap);
        }

        FreshFixtureAuthority::recover_existing(canonical_final, retained, policy)
            .map_err(map_recovery_error)
    }
}

/// Validates the caller-supplied label and returns it as a single [`Path`]
/// component. The label must be non-empty, at most [`MAX_LABEL_BYTES`] bytes,
/// and match the project's safe component charset.
fn validate_label(leaf_label: &str) -> Result<&Path, ExactLeafError> {
    if leaf_label.len() > MAX_LABEL_BYTES || !validate_component(leaf_label) {
        return Err(ExactLeafError::InvalidLabel);
    }
    Ok(Path::new(leaf_label))
}

/// Generates 16 bytes of OS randomness via the workspace's `getrandom`
/// dependency and renders them as a 32-character hex string. Used for
/// staging sibling names so they are unpredictable to same-UID races.
fn staging_name_os_random() -> Result<String, ExactLeafError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|source| {
        ExactLeafError::Io(std::io::Error::other(format!(
            "getrandom failed during exact-leaf publication: {source}"
        )))
    })?;
    Ok(format!(".staging-{}", hex_encode(&bytes)))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

/// Creates the staging sibling, validates it, synchronizes it and its parent,
/// and returns the retained staging identity plus a duplicated parent file
/// descriptor suitable for `renameat_with`.
///
/// Every failure after `mkdir` preserves the unpredictable staging name. The
/// available parent-path removal operation cannot atomically prove the name
/// still denotes this retained inode.
fn create_and_verify_staging(
    parent: &Dir,
    staging_path: &Path,
) -> Result<(Dir, FileIdentity, std::fs::File), ExactLeafError> {
    create_dir_private(parent, staging_path)?;
    let (staging_dir, staging_identity) = match (|| -> Result<_, ExactLeafError> {
        let staging_dir = open_dir_path_nofollow(parent, staging_path)?;
        let metadata = staging_dir.dir_metadata()?;
        if !is_safe_exact_leaf_directory(&metadata) {
            return Err(ExactLeafError::UnsafeLeaf);
        }
        let staging_identity = identity(&metadata);
        sync_dir(&staging_dir)?;
        sync_dir(parent)?;
        Ok((staging_dir, staging_identity))
    })() {
        Ok(prepared) => prepared,
        Err(error) => {
            // Do NOT clean the staging before its creator identity is retained.
            // The staging has an OS-random name under a private parent; leaving
            // it is safer than deleting by an unverified pathname (ADR-0001 §1).
            return Err(error);
        }
    };
    let parent_fd = parent.try_clone()?.into_std_file();
    Ok((staging_dir, staging_identity, parent_fd))
}

/// Maps the shared fixture boundary validator onto the exact-leaf error space.
fn map_boundary_error(error: FixtureAuthorityError) -> ExactLeafError {
    match error {
        FixtureAuthorityError::ForbiddenOverlap {
            class: "outside-test-tmpdir" | "outside-process-tmpdir",
        } => ExactLeafError::OutsideTemp,
        FixtureAuthorityError::ForbiddenOverlap { class } => {
            ExactLeafError::ForbiddenOverlap { class }
        }
        FixtureAuthorityError::IdentityChanged => ExactLeafError::LeafSwap,
        FixtureAuthorityError::Io(source) => ExactLeafError::Io(source),
        _ => ExactLeafError::UnsafeLeaf,
    }
}

fn map_recovery_error(error: FixtureAuthorityError) -> ExactLeafError {
    match error {
        FixtureAuthorityError::RootNotHarnessCreated => ExactLeafError::LeafMissing,
        FixtureAuthorityError::InvalidRootMode => ExactLeafError::UnsafeLeaf,
        FixtureAuthorityError::IdentityChanged => ExactLeafError::LeafSwap,
        FixtureAuthorityError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            ExactLeafError::LeafMissing
        }
        other => map_boundary_error(other),
    }
}

fn is_safe_exact_leaf_directory(metadata: &cap_std::fs::Metadata) -> bool {
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

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        error::Error,
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        thread,
    };

    use super::{ExactLeafAuthority, ExactLeafError, PublicationPoint};
    use crate::{
        FixtureAdmissionPolicy,
        fixture_authority::{AUTHORITY_LOCK, AUTHORITY_MARKER, AUTHORITY_MARKER_BYTES},
    };

    static CASE: AtomicU64 = AtomicU64::new(1);
    const LOCK_PROBE_ROOT_ENV: &str = "NANIKA_TEST_FIXTURE_LOCK_PROBE_ROOT";
    const LOCK_PROBE_EXPECT_ENV: &str = "NANIKA_TEST_FIXTURE_LOCK_PROBE_EXPECT";

    struct Envelope {
        outer: PathBuf,
        live_user: PathBuf,
        checkout: PathBuf,
        policy_temp: PathBuf,
        policy: FixtureAdmissionPolicy,
    }

    impl Envelope {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let outer = fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
            let outer = outer.join(format!(
                "orchestrator-rs-exact-leaf-{label}-{}-{}",
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
            Ok(Self {
                outer,
                live_user,
                checkout,
                policy_temp,
                policy,
            })
        }

        fn next_label(&self) -> String {
            format!(
                "leaf-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            )
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

    fn create_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        fs::write(path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }

    fn replace_directory(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
        let displaced = path.with_extension("displaced");
        fs::rename(path, &displaced)?;
        create_private_directory(path)?;
        Ok(displaced)
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

    fn run_lock_probe(root: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("exact_leaf_authority::tests::fixture_lock_probe_child")
            .arg("--nocapture")
            .env(LOCK_PROBE_ROOT_ENV, root)
            .env(LOCK_PROBE_EXPECT_ENV, expected)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "lock probe failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }

    #[test]
    fn fixture_lock_probe_child() -> Result<(), Box<dyn Error>> {
        let Some(root) = std::env::var_os(LOCK_PROBE_ROOT_ENV) else {
            return Ok(());
        };
        let expected = std::env::var(LOCK_PROBE_EXPECT_ENV)?;
        let root = PathBuf::from(root);
        let root_probe = fs::File::open(&root)?;
        let named_probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(AUTHORITY_LOCK))?;
        let root_contended = rustix::fs::flock(
            &root_probe,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .is_err();
        let named_contended = rustix::fs::flock(
            &named_probe,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .is_err();
        match expected.as_str() {
            "contended" => assert!(root_contended && named_contended),
            "available" => assert!(!root_contended && !named_contended),
            _ => return Err("unknown lock probe expectation".into()),
        }
        Ok(())
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    struct TreeSnapshot {
        relative: PathBuf,
        kind: &'static str,
        mode: u32,
        inode: u64,
        links: u64,
    }

    fn snapshot_tree(root: &Path) -> Result<Vec<TreeSnapshot>, Box<dyn Error>> {
        fn visit(
            root: &Path,
            relative: &Path,
            out: &mut Vec<TreeSnapshot>,
        ) -> Result<(), Box<dyn Error>> {
            let path = root.join(relative);
            let metadata = fs::symlink_metadata(&path)?;
            let kind = if metadata.is_dir() {
                "dir"
            } else if metadata.is_file() {
                "file"
            } else if metadata.file_type().is_symlink() {
                "symlink"
            } else {
                "other"
            };
            out.push(TreeSnapshot {
                relative: relative.to_path_buf(),
                kind,
                mode: metadata.mode() & 0o7777,
                inode: metadata.ino(),
                links: metadata.nlink(),
            });
            if metadata.is_dir() {
                let mut children: Vec<_> = fs::read_dir(&path)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<Result<_, _>>()?;
                children.sort();
                for child in children {
                    visit(root, &relative.join(child), out)?;
                }
            }
            Ok(())
        }

        let mut out = Vec::new();
        visit(root, Path::new(""), &mut out)?;
        Ok(out)
    }

    #[test]
    fn fresh_publish_lands_exact_private_leaf() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("fresh")?;
        let label = envelope.next_label();

        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;

        let metadata = fs::metadata(leaf.canonical_path())?;
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(leaf.canonical_path(), &envelope.policy_temp.join(&label));
        // The retained capability handle resolves to the same private leaf.
        let handle_metadata = leaf.directory().dir_metadata()?;
        assert!(handle_metadata.is_dir());
        assert_eq!(super::mode(&handle_metadata) & 0o7777, 0o700);
        assert_eq!(leaf.identity()?, super::identity(&handle_metadata));
        assert!(leaf.verify().is_ok());
        Ok(())
    }

    #[test]
    fn prepared_root_holds_both_fixture_locks_before_rename() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("pre-rename-locks")?;
        let root = envelope.policy_temp.join(envelope.next_label());
        let parent = envelope.policy_temp.clone();

        let authority =
            ExactLeafAuthority::publish_at_with_hook(&envelope.policy, &root, |point, name| {
                if point == PublicationPoint::Prepared {
                    assert!(run_lock_probe(&parent.join(name), "contended").is_ok());
                }
            })?;

        assert!(authority.verify().is_ok());
        Ok(())
    }

    #[test]
    fn published_root_keeps_both_prepared_locks_without_reacquire() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("post-rename-locks")?;
        let root = envelope.policy_temp.join(envelope.next_label());
        let parent = envelope.policy_temp.clone();

        let authority =
            ExactLeafAuthority::publish_at_with_hook(&envelope.policy, &root, |point, name| {
                if point == PublicationPoint::Published {
                    assert!(run_lock_probe(&parent.join(name), "contended").is_ok());
                }
            })?;

        run_lock_probe(&root, "contended")?;
        drop(authority);
        run_lock_probe(&root, "available")?;
        Ok(())
    }

    #[test]
    fn final_capability_release_allows_one_read_only_recovery() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("final-release")?;
        let label = envelope.next_label();
        let authority = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        let root = envelope.policy_temp.join(&label);

        run_lock_probe(&root, "contended")?;
        drop(authority);
        run_lock_probe(&root, "available")?;

        let recovered = ExactLeafAuthority::recover(&envelope.policy, &label)?;
        assert!(recovered.verify().is_ok());
        Ok(())
    }

    #[test]
    fn losing_publisher_preserves_owned_residue_and_never_touches_winner()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("losing-publisher")?;
        let root = envelope.policy_temp.join(envelope.next_label());
        let mut residue = None;

        let error = match ExactLeafAuthority::publish_at_with_hook(
            &envelope.policy,
            &root,
            |point, name| {
                if point == PublicationPoint::Prepared {
                    let staging = envelope.policy_temp.join(name);
                    if let Ok(metadata) = fs::metadata(&staging) {
                        residue = Some((staging, (metadata.dev(), metadata.ino())));
                    }
                    assert!(create_private_directory(&root).is_ok());
                    assert!(fs::write(root.join("sentinel"), b"winner").is_ok());
                }
            },
        ) {
            Ok(_) => return Err("publisher unexpectedly won the no-replace race".into()),
            Err(error) => error,
        };

        assert!(matches!(error, ExactLeafError::LeafExists));
        assert_eq!(fs::read(root.join("sentinel"))?, b"winner");
        let (staging, expected_identity) =
            residue.ok_or("prepared publication did not expose its staging identity")?;
        let metadata = fs::metadata(&staging)?;
        assert_eq!((metadata.dev(), metadata.ino()), expected_identity);
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        assert_eq!(
            fs::read(staging.join(AUTHORITY_MARKER))?,
            AUTHORITY_MARKER_BYTES
        );
        assert_eq!(fs::read(staging.join(AUTHORITY_LOCK))?, b"");
        assert_eq!(
            entry_names(&staging)?,
            [AUTHORITY_LOCK, AUTHORITY_MARKER]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect()
        );
        Ok(())
    }

    #[test]
    fn marker_substitution_between_create_and_identity_retention_mints_no_capability()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("created-marker-swap")?;
        let root = envelope.policy_temp.join(envelope.next_label());
        let mut evidence = None;

        let error = match ExactLeafAuthority::publish_at_with_hook(
            &envelope.policy,
            &root,
            |point, name| {
                if point == PublicationPoint::MarkerCreatedBeforeIdentityRetention {
                    let staging = envelope.policy_temp.join(name);
                    let marker = staging.join(AUTHORITY_MARKER);
                    let displaced = staging.join("marker-displaced");
                    if let Ok(retained) = fs::metadata(&marker) {
                        assert!(fs::rename(&marker, &displaced).is_ok());
                        assert!(create_private_file(&marker, AUTHORITY_MARKER_BYTES).is_ok());
                        evidence =
                            Some((staging, (retained.dev(), retained.ino()), displaced, marker));
                    }
                }
            },
        ) {
            Ok(_) => return Err("marker substitution minted a fixture capability".into()),
            Err(error) => error,
        };

        assert!(matches!(error, ExactLeafError::LeafSwap));
        assert!(!root.exists());
        let (staging, retained_identity, displaced, replacement) =
            evidence.ok_or("marker substitution hook did not run")?;
        let displaced_metadata = fs::metadata(displaced)?;
        let replacement_metadata = fs::metadata(replacement)?;
        assert_eq!(
            (displaced_metadata.dev(), displaced_metadata.ino()),
            retained_identity
        );
        assert_ne!(
            (replacement_metadata.dev(), replacement_metadata.ino()),
            retained_identity
        );
        assert!(staging.exists());
        Ok(())
    }

    #[test]
    fn lock_substitution_between_create_and_identity_retention_mints_no_capability()
    -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("created-lock-swap")?;
        let root = envelope.policy_temp.join(envelope.next_label());
        let mut evidence = None;

        let error = match ExactLeafAuthority::publish_at_with_hook(
            &envelope.policy,
            &root,
            |point, name| {
                if point == PublicationPoint::LockCreatedBeforeIdentityRetention {
                    let staging = envelope.policy_temp.join(name);
                    let lock = staging.join(AUTHORITY_LOCK);
                    let displaced = staging.join("lock-displaced");
                    if let Ok(retained) = fs::metadata(&lock) {
                        assert!(fs::rename(&lock, &displaced).is_ok());
                        assert!(create_private_file(&lock, b"").is_ok());
                        evidence =
                            Some((staging, (retained.dev(), retained.ino()), displaced, lock));
                    }
                }
            },
        ) {
            Ok(_) => return Err("lock substitution minted a fixture capability".into()),
            Err(error) => error,
        };

        assert!(matches!(error, ExactLeafError::LeafSwap));
        assert!(!root.exists());
        let (staging, retained_identity, displaced, replacement) =
            evidence.ok_or("lock substitution hook did not run")?;
        let displaced_metadata = fs::metadata(displaced)?;
        let replacement_metadata = fs::metadata(replacement)?;
        assert_eq!(
            (displaced_metadata.dev(), displaced_metadata.ino()),
            retained_identity
        );
        assert_ne!(
            (replacement_metadata.dev(), replacement_metadata.ino()),
            retained_identity
        );
        assert!(staging.exists());
        Ok(())
    }

    #[test]
    fn marker_swap_after_rename_prevents_capability_minting() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("post-rename-marker-swap")?;
        let root = envelope.policy_temp.join(envelope.next_label());

        let error =
            match ExactLeafAuthority::publish_at_with_hook(&envelope.policy, &root, |point, _| {
                if point == PublicationPoint::Published {
                    assert!(
                        fs::rename(root.join(AUTHORITY_MARKER), root.join("marker-displaced"))
                            .is_ok()
                    );
                    assert!(
                        create_private_file(&root.join(AUTHORITY_MARKER), AUTHORITY_MARKER_BYTES)
                            .is_ok()
                    );
                }
            }) {
                Ok(_) => return Err("substituted marker minted a fixture capability".into()),
                Err(error) => error,
            };

        assert!(matches!(error, ExactLeafError::LeafSwap));
        Ok(())
    }

    #[test]
    fn lock_swap_after_rename_prevents_capability_minting() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("post-rename-lock-swap")?;
        let root = envelope.policy_temp.join(envelope.next_label());

        let error =
            match ExactLeafAuthority::publish_at_with_hook(&envelope.policy, &root, |point, _| {
                if point == PublicationPoint::Published {
                    assert!(
                        fs::rename(root.join(AUTHORITY_LOCK), root.join("lock-displaced")).is_ok()
                    );
                    assert!(create_private_file(&root.join(AUTHORITY_LOCK), b"").is_ok());
                }
            }) {
                Ok(_) => return Err("substituted lock minted a fixture capability".into()),
                Err(error) => error,
            };

        assert!(matches!(error, ExactLeafError::LeafSwap));
        Ok(())
    }

    #[test]
    fn no_exact_leaf_capability_or_production_bridge_remains_in_source() {
        let exact_source = include_str!("exact_leaf_authority.rs");
        let runtime_source = include_str!("runtime_home.rs");
        let workspace_source = include_str!("workspace.rs");

        assert!(!exact_source.contains(&["struct Exact", "Leaf {"].concat()));
        assert!(!runtime_source.contains("from_exact_leaf"));
        assert!(!runtime_source.contains("ProductionBoundaryLease::ExactLeaf"));
        assert!(!workspace_source.contains("exact_leaf_authority::ExactLeaf"));
    }

    #[test]
    fn noreplace_rejects_preexisting_leaf_without_mutation() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("exists")?;
        let label = envelope.next_label();
        let final_path = envelope.policy_temp.join(&label);
        create_private_directory(&final_path)?;
        fs::write(final_path.join("sentinel"), b"preserve")?;
        let before = snapshot_tree(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::publish(&envelope.policy, &label) {
            Ok(_) => return Err("pre-existing leaf was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::LeafExists));
        assert_eq!(snapshot_tree(&envelope.policy_temp)?, before);
        assert_eq!(fs::read(final_path.join("sentinel"))?, b"preserve");
        Ok(())
    }

    #[test]
    fn noreplace_rejects_preexisting_leaf_after_probe_race() -> Result<(), Box<dyn Error>> {
        // Probe observed no leaf; create one before the publish call proceeds
        // past staging. The authoritative check is the NOREPLACE rename itself,
        // which must still refuse the second publisher without overwriting the
        // existing leaf or leaving staging behind.
        let envelope = Envelope::new("probe-race")?;
        let label = envelope.next_label();
        let final_path = envelope.policy_temp.join(&label);

        // Pre-create the leaf through a sibling authority so the probe inside
        // publish() would observe NotFound only if it ran before this line.
        let _first = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        assert!(final_path.exists());

        let error = match ExactLeafAuthority::publish(&envelope.policy, &label) {
            Ok(_) => return Err("second publish should have been refused".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::LeafExists));
        // No staging residue.
        for entry in fs::read_dir(&envelope.policy_temp)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(!name.contains(".staging-"), "staging residue: {name}");
        }
        Ok(())
    }

    #[test]
    fn symlink_final_rejected_without_mutation() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("symlink")?;
        let label = envelope.next_label();
        let final_path = envelope.policy_temp.join(&label);
        let target = envelope.outer.join("symlink-target");
        create_private_directory(&target)?;
        symlink(&target, &final_path)?;
        let before = snapshot_tree(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::publish(&envelope.policy, &label) {
            Ok(_) => return Err("symlink final was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::LeafExists));
        assert_eq!(snapshot_tree(&envelope.policy_temp)?, before);
        assert!(fs::symlink_metadata(&final_path)?.file_type().is_symlink());
        Ok(())
    }

    #[test]
    fn verify_detects_leaf_replacement() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("verify-replace")?;
        let label = envelope.next_label();
        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        assert!(leaf.verify().is_ok());

        replace_directory(leaf.canonical_path())?;

        assert!(leaf.verify().is_err());
        Ok(())
    }

    #[test]
    fn verify_detects_mode_tamper() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("verify-mode")?;
        let label = envelope.next_label();
        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        assert!(leaf.verify().is_ok());

        fs::set_permissions(leaf.canonical_path(), fs::Permissions::from_mode(0o755))?;

        assert!(leaf.verify().is_err());
        Ok(())
    }

    #[test]
    fn invalid_label_charset_and_length_rejected() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("invalid-label")?;
        let too_long = "a".repeat(65);
        for bad in ["has/slash", "has space", "has.dot", "", too_long.as_str()] {
            assert!(
                matches!(
                    ExactLeafAuthority::publish(&envelope.policy, bad),
                    Err(ExactLeafError::InvalidLabel)
                ),
                "label {bad:?} should be rejected"
            );
        }
        assert!(ExactLeafAuthority::publish(&envelope.policy, "ok_Leaf-1").is_ok());
        Ok(())
    }

    #[test]
    fn overlap_with_forbidden_root_rejected_before_mutation() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("overlap-forbidden")?;
        let forbidden = envelope.policy_temp.join("forbidden-zone");
        create_private_directory(&forbidden)?;
        let policy = FixtureAdmissionPolicy::new(
            &envelope.live_user,
            &envelope.checkout,
            &envelope.policy_temp,
        )
        .with_forbidden_root(&forbidden);
        let before = entry_names(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::publish(&policy, "forbidden-zone") {
            Ok(_) => return Err("overlap with forbidden root was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ExactLeafError::ForbiddenOverlap {
                class: "explicit-forbidden-root"
            }
        ));
        assert_eq!(entry_names(&envelope.policy_temp)?, before);
        Ok(())
    }

    #[test]
    fn overlap_with_repository_checkout_rejected_before_mutation() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("overlap-checkout")?;
        fs::write(envelope.checkout.join("tracked"), b"tracked")?;
        let policy = FixtureAdmissionPolicy::new(
            &envelope.live_user,
            &envelope.checkout,
            &envelope.checkout,
        );
        let before = entry_names(&envelope.checkout)?;

        let error = match ExactLeafAuthority::publish(&policy, "leaf") {
            Ok(_) => return Err("overlap with repository checkout was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ExactLeafError::ForbiddenOverlap {
                class: "repository-checkout"
            }
        ));
        assert_eq!(entry_names(&envelope.checkout)?, before);
        Ok(())
    }

    #[test]
    fn admission_writes_nothing_outside_policy_temp() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("envelope")?;
        let sentinel = envelope.outer.join("outside-sentinel");
        create_private_file(&sentinel, b"unchanged")?;
        let before = entry_names(&envelope.outer)?;

        let label = envelope.next_label();
        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        assert!(leaf.verify().is_ok());

        assert_eq!(entry_names(&envelope.outer)?, before);
        assert_eq!(fs::read(&sentinel)?, b"unchanged");
        Ok(())
    }

    #[test]
    fn debug_and_error_chain_redact_canonical_path() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("secret-user-99999")?;
        // Charset-safe label that still carries a sensitive token through the
        // canonical path so the redaction check is meaningful.
        let label = "leaf-secret-user-99999".to_string();
        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        let debug = format!("{leaf:?}");
        assert_eq!(
            debug,
            "FreshFixtureAuthority { kind: \"fresh-isolated-fixture\" }"
        );

        let error = match ExactLeafAuthority::publish(&envelope.policy, &label) {
            Ok(_) => return Err("second publish should have been refused".into()),
            Err(error) => error,
        };
        let mut rendered = vec![format!("{error}"), format!("{error:?}")];
        let mut source: Option<&dyn Error> = error.source();
        while let Some(current) = source {
            rendered.push(format!("{current}"));
            rendered.push(format!("{current:?}"));
            source = current.source();
        }
        for value in rendered {
            assert!(!value.contains("secret-user-99999"), "{value}");
            assert!(
                !value.contains(envelope.policy_temp.to_string_lossy().as_ref()),
                "{value}"
            );
            assert!(!value.contains(label.as_str()), "{value}");
        }
        Ok(())
    }

    #[test]
    fn concurrent_publish_of_same_label_exactly_one_winner() -> Result<(), Box<dyn Error>> {
        let envelope = Arc::new(Envelope::new("concurrent")?);
        let label = envelope.next_label();
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let envelope = Arc::clone(&envelope);
            let barrier = Arc::clone(&barrier);
            let label = label.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                ExactLeafAuthority::publish(&envelope.policy, &label)
            }));
        }

        let mut successes = 0usize;
        let mut leaf_exists = 0usize;
        let mut other_errors = Vec::new();
        for worker in workers {
            match worker
                .join()
                .map_err(|_| std::io::Error::other("concurrent worker panicked"))?
            {
                Ok(_leaf) => successes += 1,
                Err(ExactLeafError::LeafExists) => leaf_exists += 1,
                Err(other) => other_errors.push(other),
            }
        }
        assert!(
            other_errors.is_empty(),
            "unexpected errors: {other_errors:?}"
        );
        assert_eq!(successes, 1, "exactly one publisher should win");
        assert_eq!(leaf_exists, 7, "the other seven should observe LeafExists");

        let final_path = envelope.policy_temp.join(&label);
        let metadata = fs::metadata(&final_path)?;
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
        let names = entry_names(&envelope.policy_temp)?;
        for name in names.iter().filter(|name| name.starts_with(".staging-")) {
            let residue = envelope.policy_temp.join(name);
            let metadata = fs::metadata(&residue)?;
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
            assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
            assert_eq!(
                entry_names(&residue)?,
                [AUTHORITY_LOCK, AUTHORITY_MARKER]
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect()
            );
            assert_eq!(
                fs::read(residue.join(AUTHORITY_MARKER))?,
                AUTHORITY_MARKER_BYTES
            );
            assert_eq!(fs::read(residue.join(AUTHORITY_LOCK))?, b"");
        }
        assert!(names.contains(&label));
        Ok(())
    }

    #[test]
    fn recovery_reopens_published_identity() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("recover-ok")?;
        let label = envelope.next_label();
        let published = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        let published_identity = published.identity()?;
        drop(published);

        let recovered = ExactLeafAuthority::recover(&envelope.policy, &label)?;
        assert_eq!(recovered.identity()?, published_identity);
        assert_eq!(
            recovered.canonical_path(),
            &envelope.policy_temp.join(&label)
        );
        assert!(recovered.verify().is_ok());
        Ok(())
    }

    #[test]
    fn recovery_missing_leaf_fails_closed() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("recover-missing")?;
        let label = envelope.next_label();

        let error = match ExactLeafAuthority::recover(&envelope.policy, &label) {
            Ok(_) => return Err("recovery of a missing leaf was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::LeafMissing));
        Ok(())
    }

    #[test]
    fn recovery_detects_post_publish_mode_tamper() -> Result<(), Box<dyn Error>> {
        // Recover itself carries no retained identity across processes, so it
        // cannot detect a same-mode replacement. It still rejects an unsafe
        // mode, which is the property the exact-leaf primitive owns. A
        // consumer that needs replacement-since-publish detection layers a
        // versioned manifest above this leaf.
        let envelope = Envelope::new("recover-mode")?;
        let label = envelope.next_label();
        let published = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        drop(published);
        let final_path = envelope.policy_temp.join(&label);
        fs::set_permissions(&final_path, fs::Permissions::from_mode(0o755))?;
        let before = snapshot_tree(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::recover(&envelope.policy, &label) {
            Ok(_) => return Err("recovery of an unsafe-mode leaf was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::UnsafeLeaf));
        assert_eq!(snapshot_tree(&envelope.policy_temp)?, before);
        Ok(())
    }

    #[test]
    fn recovery_rejects_unmarked_leaf_read_only() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("unmarked-v2")?;
        let label = envelope.next_label();
        // Create a 0700 owner-owned leaf WITHOUT the v2 marker.
        let unmarked = envelope.policy_temp.join(&label);
        create_private_directory(&unmarked)?;
        let before = snapshot_tree(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::recover(&envelope.policy, &label) {
            Ok(_) => return Err("recovery of an unmarked leaf was accepted".into()),
            Err(error) => error,
        };
        assert!(
            matches!(error, ExactLeafError::LeafMissing),
            "an unmarked leaf must be rejected, not silently admitted"
        );
        // The tree must be unchanged (read-only rejection).
        assert_eq!(snapshot_tree(&envelope.policy_temp)?, before);
        Ok(())
    }

    #[test]
    fn recovery_rejects_wrong_marker_content_read_only() -> Result<(), Box<dyn Error>> {
        let envelope = Envelope::new("wrong-marker-v2")?;
        let label = envelope.next_label();
        let leaf = ExactLeafAuthority::publish(&envelope.policy, &label)?;
        drop(leaf);
        // Overwrite the marker with wrong content (preserving mode 0600).
        let marker_path = envelope.policy_temp.join(&label).join(AUTHORITY_MARKER);
        fs::write(&marker_path, b"wrong-marker\n")?;
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600))?;
        let before = snapshot_tree(&envelope.policy_temp)?;

        let error = match ExactLeafAuthority::recover(&envelope.policy, &label) {
            Ok(_) => return Err("recovery with wrong marker content was accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(error, ExactLeafError::LeafMissing));
        // Read-only: the tree is unchanged.
        assert_eq!(snapshot_tree(&envelope.policy_temp)?, before);
        Ok(())
    }
}
