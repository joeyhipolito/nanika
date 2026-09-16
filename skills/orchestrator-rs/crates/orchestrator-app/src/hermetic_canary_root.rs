#![cfg(all(unix, feature = "verification-process-canary"))]

use std::{fmt, path::Path, sync::Arc};

use crate::{
    FixtureAdmissionPolicy, FixtureAuthorityError, FreshFixtureAuthority, IsolatedFixtureRoot,
    ProductionBoundary,
    exact_leaf_authority::{ExactLeafAuthority, ExactLeafError},
    fixture_authority::{
        HERMETIC_CANARY_COMPAT_TARGET, HERMETIC_CANARY_PRIVATE_LEDGER_TARGET,
        publish_hermetic_canary_layout_manifest,
    },
    runtime_store::PrivateProcessLedgerBoundary,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HermeticCanaryDisposition {
    Fresh,
    Recovered,
}

pub(crate) struct HermeticCanaryAuthority {
    compatibility_home: Arc<ProductionBoundary>,
    private_process_ledger: PrivateProcessLedgerBoundary,
    disposition: HermeticCanaryDisposition,
}

impl fmt::Debug for HermeticCanaryAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticCanaryAuthority")
            .field("kind", &"sealed-hermetic-canary")
            .field("disposition", &self.disposition)
            .finish()
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the sealed root is staged for the following verification-canary composition cell"
    )
)]
impl HermeticCanaryAuthority {
    pub(crate) fn admit(
        fixture: IsolatedFixtureRoot,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let fixture = FreshFixtureAuthority::admit(fixture, policy)?;
        Self::publish_fixture(fixture)
    }

    /// Atomically publishes a fixture authority at one exact path, then
    /// derives the fixed compatibility and private-ledger targets plus the
    /// stable v1 identity manifest from that sole authority.
    pub(crate) fn publish_exact_at(
        policy: &FixtureAdmissionPolicy,
        exact_root: &Path,
    ) -> Result<Self, ExactLeafError> {
        let fixture = ExactLeafAuthority::publish_at(policy, exact_root)?;
        Self::publish_fixture(fixture).map_err(ExactLeafError::FixtureAuthority)
    }

    pub(crate) fn recover_exact_at(
        policy: &FixtureAdmissionPolicy,
        exact_root: &Path,
    ) -> Result<Self, ExactLeafError> {
        let fixture = ExactLeafAuthority::recover_at(policy, exact_root)?;
        Self::recover_fixture(fixture).map_err(ExactLeafError::FixtureAuthority)
    }

    fn publish_fixture(fixture: FreshFixtureAuthority) -> Result<Self, FixtureAuthorityError> {
        let compatibility = fixture.create_target(HERMETIC_CANARY_COMPAT_TARGET)?;
        let private_ledger = fixture.create_target(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET)?;
        publish_hermetic_canary_layout_manifest(&fixture, &compatibility, &private_ledger)?;
        Self::from_targets(
            compatibility,
            private_ledger,
            HermeticCanaryDisposition::Fresh,
        )
    }

    pub(crate) fn recover(
        fixture: IsolatedFixtureRoot,
        policy: &FixtureAdmissionPolicy,
    ) -> Result<Self, FixtureAuthorityError> {
        let fixture = FreshFixtureAuthority::recover(fixture, policy)?;
        Self::recover_fixture(fixture)
    }

    fn recover_fixture(fixture: FreshFixtureAuthority) -> Result<Self, FixtureAuthorityError> {
        let compatibility = fixture.open_target(HERMETIC_CANARY_COMPAT_TARGET)?;
        let private_ledger = fixture.open_target(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET)?;
        Self::from_targets(
            compatibility,
            private_ledger,
            HermeticCanaryDisposition::Recovered,
        )
    }

    fn from_targets(
        compatibility: crate::TargetRootAuthority,
        private_ledger: crate::TargetRootAuthority,
        disposition: HermeticCanaryDisposition,
    ) -> Result<Self, FixtureAuthorityError> {
        let (compatibility, private_ledger) =
            ProductionBoundary::from_hermetic_canary_targets(compatibility, private_ledger)?;
        let compatibility_home = Arc::new(compatibility);
        let private_process_ledger = PrivateProcessLedgerBoundary::new(Arc::new(private_ledger));
        Ok(Self {
            compatibility_home,
            private_process_ledger,
            disposition,
        })
    }

    pub(crate) fn compatibility_home(&self) -> &Arc<ProductionBoundary> {
        &self.compatibility_home
    }

    pub(crate) fn private_process_ledger(&self) -> &PrivateProcessLedgerBoundary {
        &self.private_process_ledger
    }

    pub(crate) const fn disposition(&self) -> HermeticCanaryDisposition {
        self.disposition
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
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use super::{
        HERMETIC_CANARY_COMPAT_TARGET, HERMETIC_CANARY_PRIVATE_LEDGER_TARGET,
        HermeticCanaryAuthority, HermeticCanaryDisposition,
    };
    use crate::{
        FixtureAdmissionPolicy, FreshFixtureAuthority, IsolatedFixtureRoot,
        capability::CapabilityRoot, exact_leaf_authority::ExactLeafAuthority,
    };

    const MARKER: &str = ".orchestrator-fixture-authority";
    const MARKER_BYTES: &[u8] = b"orchestrator-rs-fixture-v2\n";
    const V1_MARKER_BYTES: &[u8] = b"orchestrator-rs-fixture-v1\n";
    const LOCK: &str = ".orchestrator-fixture-lock";
    const MANIFEST: &str = ".orchestrator-hermetic-canary-layout-v1";
    static CASE: AtomicU64 = AtomicU64::new(1);

    struct TestEnvelope {
        outer: PathBuf,
        harness: PathBuf,
        policy: FixtureAdmissionPolicy,
    }

    impl TestEnvelope {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let temporary = fs::canonicalize(std::env::temp_dir()).unwrap_or(std::env::temp_dir());
            let outer = temporary.join(format!(
                "orchestrator-rs-hermetic-{label}-{}-{}",
                std::process::id(),
                CASE.fetch_add(1, Ordering::Relaxed)
            ));
            let harness = outer.join("envelope");
            create_private_directory(&outer)?;
            create_private_directory(&harness)?;
            let policy = FixtureAdmissionPolicy::new(
                outer.join("live-user"),
                outer.join("checkout"),
                &temporary,
            );
            Ok(Self {
                outer,
                harness,
                policy,
            })
        }

        fn create_authority(&self) -> Result<(PathBuf, HermeticCanaryAuthority), Box<dyn Error>> {
            let fixture = IsolatedFixtureRoot::create_fresh(&self.harness)?;
            let root = fixture.path().to_path_buf();
            let authority = HermeticCanaryAuthority::admit(fixture, &self.policy)?;
            Ok((root, authority))
        }

        fn recover(&self, root: &Path) -> Result<HermeticCanaryAuthority, Box<dyn Error>> {
            let fixture = IsolatedFixtureRoot::identify(root)?;
            Ok(HermeticCanaryAuthority::recover(fixture, &self.policy)?)
        }

        fn create_exact_authority(
            &self,
            label: &str,
        ) -> Result<(PathBuf, HermeticCanaryAuthority), Box<dyn Error>> {
            let root = self.outer.join(label);
            let authority = HermeticCanaryAuthority::publish_exact_at(&self.policy, &root)?;
            Ok((root, authority))
        }
    }

    impl Drop for TestEnvelope {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.outer);
        }
    }

    fn create_private_directory(path: &Path) -> std::io::Result<()> {
        fs::create_dir(path)?;
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

    fn child_paths(root: &Path) -> (PathBuf, PathBuf) {
        let targets = root.join("targets");
        (
            targets.join(HERMETIC_CANARY_COMPAT_TARGET),
            targets.join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
        )
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

    #[derive(Debug, Eq, PartialEq)]
    struct TreeEntrySnapshot {
        relative: PathBuf,
        device: u64,
        inode: u64,
        mode: u32,
        links: u64,
        bytes: Option<Vec<u8>>,
    }

    fn tree_snapshot(root: &Path) -> Result<Vec<TreeEntrySnapshot>, Box<dyn Error>> {
        fn visit(
            root: &Path,
            relative: &Path,
            snapshots: &mut Vec<TreeEntrySnapshot>,
        ) -> Result<(), Box<dyn Error>> {
            let path = root.join(relative);
            let metadata = fs::symlink_metadata(&path)?;
            snapshots.push(TreeEntrySnapshot {
                relative: relative.to_path_buf(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
                bytes: metadata.is_file().then(|| fs::read(&path)).transpose()?,
            });
            if metadata.is_dir() {
                let mut children = fs::read_dir(path)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<Result<Vec<_>, _>>()?;
                children.sort();
                for child in children {
                    visit(root, &relative.join(child), snapshots)?;
                }
            }
            Ok(())
        }

        let mut snapshots = Vec::new();
        visit(root, Path::new(""), &mut snapshots)?;
        Ok(snapshots)
    }

    #[test]
    fn fresh_layout_creates_distinct_private_siblings() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("fresh")?;
        let (root, authority) = case.create_authority()?;
        let (compatibility, private_ledger) = child_paths(&root);
        let root_metadata = fs::metadata(&root)?;
        let marker_metadata = fs::metadata(root.join(MARKER))?;
        let lock_metadata = fs::metadata(root.join(LOCK))?;
        let manifest_metadata = fs::metadata(root.join(MANIFEST))?;

        assert_eq!(authority.disposition(), HermeticCanaryDisposition::Fresh);
        assert!(root_metadata.is_dir());
        assert_eq!(root_metadata.permissions().mode() & 0o7777, 0o700);
        assert!(marker_metadata.is_file());
        assert_eq!(marker_metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(marker_metadata.nlink(), 1);
        assert_eq!(fs::read(root.join(MARKER))?, MARKER_BYTES);
        assert!(lock_metadata.is_file());
        assert_eq!(lock_metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(lock_metadata.nlink(), 1);
        assert!(fs::read(root.join(LOCK))?.is_empty());
        assert!(manifest_metadata.is_file());
        assert_eq!(manifest_metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(manifest_metadata.nlink(), 1);
        assert!(!fs::read(root.join(MANIFEST))?.is_empty());
        assert_eq!(
            fs::metadata(&compatibility)?.permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(&private_ledger)?.permissions().mode() & 0o7777,
            0o700
        );
        let compatibility_metadata = fs::metadata(compatibility)?;
        let private_ledger_metadata = fs::metadata(private_ledger)?;
        assert_ne!(
            (compatibility_metadata.dev(), compatibility_metadata.ino()),
            (private_ledger_metadata.dev(), private_ledger_metadata.ino())
        );
        Ok(())
    }

    #[test]
    fn exact_publication_derives_fixed_distinct_target_identities() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("exact-fixed-targets")?;
        let (root, authority) = case.create_exact_authority("exact-root")?;
        let (compatibility, private_ledger) = child_paths(&root);
        let compatibility_metadata = fs::metadata(&compatibility)?;
        let private_ledger_metadata = fs::metadata(&private_ledger)?;

        assert_eq!(
            authority.compatibility_home().canonical_path(),
            compatibility
        );
        assert_eq!(
            authority
                .private_process_ledger()
                .production_boundary()
                .canonical_path(),
            private_ledger
        );
        assert_ne!(
            (compatibility_metadata.dev(), compatibility_metadata.ino()),
            (private_ledger_metadata.dev(), private_ledger_metadata.ino())
        );
        assert_eq!(
            entry_names(&root)?,
            [LOCK, MANIFEST, MARKER, "targets"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect()
        );
        Ok(())
    }

    #[test]
    fn partial_exact_layout_recovery_is_read_only() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("exact-partial")?;
        let root = case.outer.join("partial-root");
        let fixture = ExactLeafAuthority::publish_at(&case.policy, &root)?;
        let compatibility = fixture.create_target(HERMETIC_CANARY_COMPAT_TARGET)?;
        drop(compatibility);
        drop(fixture);
        let before = tree_snapshot(&root)?;

        assert!(HermeticCanaryAuthority::recover_exact_at(&case.policy, &root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        assert!(!root.join(MANIFEST).exists());
        assert!(
            !root
                .join("targets")
                .join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET)
                .exists()
        );
        Ok(())
    }

    #[test]
    fn recovery_reopens_exact_existing_layout() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("recover")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);

        let recovered = case.recover(&root)?;

        assert_eq!(
            recovered.disposition(),
            HermeticCanaryDisposition::Recovered
        );
        assert!(recovered.compatibility_home().verify().is_ok());
        assert!(
            recovered
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn recovery_rejects_missing_sibling_without_repair() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("missing-sibling")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let (compatibility, private_ledger) = child_paths(&root);
        let compatibility_metadata = fs::metadata(&compatibility)?;
        fs::remove_dir(&private_ledger)?;

        assert!(case.recover(&root).is_err());
        assert!(!private_ledger.exists());
        let retained_metadata = fs::metadata(compatibility)?;
        assert_eq!(
            (retained_metadata.dev(), retained_metadata.ino()),
            (compatibility_metadata.dev(), compatibility_metadata.ino())
        );
        Ok(())
    }

    #[test]
    fn concurrent_recovery_is_refused_by_root_inode_lock() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("busy")?;
        let (root, authority) = case.create_authority()?;

        let second = case.recover(&root);

        assert!(second.is_err());
        assert!(authority.compatibility_home().verify().is_ok());
        Ok(())
    }

    #[test]
    fn retained_child_keeps_parent_lock_after_wrapper_drop() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("retained-child")?;
        let (root, authority) = case.create_authority()?;
        let retained = Arc::clone(authority.compatibility_home());
        drop(authority);

        assert!(case.recover(&root).is_err());
        drop(retained);
        assert!(case.recover(&root).is_ok());
        Ok(())
    }

    #[test]
    fn retained_private_ledger_keeps_parent_lock_after_wrapper_drop() -> Result<(), Box<dyn Error>>
    {
        let case = TestEnvelope::new("retained-private-ledger")?;
        let (root, authority) = case.create_authority()?;
        let retained = authority.private_process_ledger().clone();
        drop(authority);

        assert!(case.recover(&root).is_err());
        drop(retained);
        assert!(case.recover(&root).is_ok());
        Ok(())
    }

    #[test]
    fn unmarked_root_recovery_does_not_mutate_it() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("unmarked")?;
        let created = IsolatedFixtureRoot::create_fresh(&case.harness)?;
        let root = created.path().to_path_buf();
        drop(created);
        let before = tree_snapshot(&root)?;

        let recovered = case.recover(&root);

        assert!(recovered.is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn wrong_marker_recovery_does_not_mutate_it() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("wrong-marker")?;
        let created = IsolatedFixtureRoot::create_fresh(&case.harness)?;
        let root = created.path().to_path_buf();
        drop(created);
        create_private_file(&root.join(MARKER), b"wrong\n")?;
        create_private_file(&root.join(LOCK), b"")?;
        let before = tree_snapshot(&root)?;

        let recovered = case.recover(&root);

        assert!(recovered.is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn v1_fixture_protocol_is_rejected_without_mutation() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("v1-protocol")?;
        let created = IsolatedFixtureRoot::create_fresh(&case.harness)?;
        let root = created.path().to_path_buf();
        drop(created);
        create_private_file(&root.join(MARKER), V1_MARKER_BYTES)?;
        create_private_file(&root.join(LOCK), b"")?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn named_lock_remains_exclusive_for_v1_interoperation() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("named-lock")?;
        let (root, authority) = case.create_authority()?;
        let competing = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(LOCK))?;

        let contention = rustix::fs::flock(
            &competing,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        );

        assert!(contention.is_err());
        assert!(authority.compatibility_home().verify().is_ok());
        Ok(())
    }

    #[test]
    fn v2_recovery_releases_root_lock_when_v1_named_lock_contends() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("reverse-lock-interoperation")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);

        let legacy_named_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(LOCK))?;
        rustix::fs::flock(
            &legacy_named_lock,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )?;

        assert!(case.recover(&root).is_err());
        let root_probe = fs::File::open(&root)?;
        rustix::fs::flock(
            &root_probe,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )?;
        drop(root_probe);

        drop(legacy_named_lock);
        assert!(case.recover(&root).is_ok());
        Ok(())
    }

    #[test]
    fn generic_same_named_targets_without_manifest_are_not_promoted() -> Result<(), Box<dyn Error>>
    {
        let case = TestEnvelope::new("generic-same-names")?;
        let fixture = IsolatedFixtureRoot::create_fresh(&case.harness)?;
        let root = fixture.path().to_path_buf();
        let generic = FreshFixtureAuthority::admit(fixture, &case.policy)?;
        let compatibility = generic.create_target(HERMETIC_CANARY_COMPAT_TARGET)?;
        let private_ledger = generic.create_target(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET)?;
        drop(compatibility);
        drop(private_ledger);
        drop(generic);
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        assert!(!root.join(MANIFEST).exists());
        Ok(())
    }

    #[test]
    fn compatibility_replacement_invalidates_both_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("replace-compat")?;
        let (root, authority) = case.create_authority()?;
        let (compatibility, _) = child_paths(&root);
        let _displaced = replace_directory(&compatibility)?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn private_ledger_replacement_invalidates_both_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("replace-ledger")?;
        let (root, authority) = case.create_authority()?;
        let (_, private_ledger) = child_paths(&root);
        let _displaced = replace_directory(&private_ledger)?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn post_drop_compatibility_substitution_is_not_recaptured() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("post-drop-compat")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let (compatibility, _) = child_paths(&root);
        let displaced = replace_directory(&compatibility)?;
        fs::remove_dir(displaced)?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn post_drop_private_ledger_substitution_is_not_recaptured() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("post-drop-ledger")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let (_, private_ledger) = child_paths(&root);
        let displaced = replace_directory(&private_ledger)?;
        fs::remove_dir(displaced)?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn post_drop_targets_substitution_is_not_recaptured() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("post-drop-targets")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let targets = root.join("targets");
        let displaced = case.outer.join("displaced-targets");
        fs::rename(&targets, &displaced)?;
        create_private_directory(&targets)?;
        fs::rename(
            displaced.join(HERMETIC_CANARY_COMPAT_TARGET),
            targets.join(HERMETIC_CANARY_COMPAT_TARGET),
        )?;
        fs::rename(
            displaced.join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
            targets.join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
        )?;
        fs::remove_dir(displaced)?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn post_drop_parent_substitution_is_not_recaptured() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("post-drop-parent")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let displaced = case.outer.join("displaced-root");
        fs::rename(&root, &displaced)?;
        create_private_directory(&root)?;
        for name in [MARKER, LOCK, MANIFEST, "targets"] {
            fs::rename(displaced.join(name), root.join(name))?;
        }
        fs::remove_dir(displaced)?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn targets_container_replacement_invalidates_both_preserved_children()
    -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("replace-targets")?;
        let (root, authority) = case.create_authority()?;
        let targets = root.join("targets");
        let displaced = targets.with_extension("displaced");
        fs::rename(&targets, &displaced)?;
        create_private_directory(&targets)?;
        fs::rename(
            displaced.join(HERMETIC_CANARY_COMPAT_TARGET),
            targets.join(HERMETIC_CANARY_COMPAT_TARGET),
        )?;
        fs::rename(
            displaced.join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
            targets.join(HERMETIC_CANARY_PRIVATE_LEDGER_TARGET),
        )?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn parent_replacement_invalidates_both_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("replace-parent")?;
        let (root, authority) = case.create_authority()?;
        let displaced = root.with_extension("displaced");
        fs::rename(&root, displaced)?;
        create_private_directory(&root)?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn symlink_target_is_rejected_without_repair() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("symlink")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let (compatibility, _) = child_paths(&root);
        let displaced = compatibility.with_extension("displaced");
        fs::rename(&compatibility, &displaced)?;
        symlink(&displaced, &compatibility)?;

        assert!(case.recover(&root).is_err());
        assert!(
            fs::symlink_metadata(&compatibility)?
                .file_type()
                .is_symlink()
        );
        Ok(())
    }

    #[test]
    fn wrong_mode_target_is_rejected_without_repair() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("wrong-mode")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        let (_, private_ledger) = child_paths(&root);
        fs::set_permissions(&private_ledger, fs::Permissions::from_mode(0o755))?;

        assert!(case.recover(&root).is_err());
        assert_eq!(
            fs::metadata(private_ledger)?.permissions().mode() & 0o7777,
            0o755
        );
        Ok(())
    }

    #[test]
    fn debug_and_recovery_error_redact_fixture_path() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("secret-user-86420")?;
        let (root, authority) = case.create_authority()?;
        let debug = format!("{authority:?}");
        drop(authority);
        fs::write(root.join(MARKER), b"wrong\n")?;
        let error = match case.recover(&root) {
            Err(error) => error,
            Ok(_) => return Err(std::io::Error::other("wrong marker was accepted").into()),
        };
        let mut rendered = vec![format!("{error:?}"), error.to_string()];
        let mut source = error.source();
        while let Some(current) = source {
            rendered.push(format!("{current:?}"));
            rendered.push(current.to_string());
            source = current.source();
        }

        assert_eq!(
            debug,
            "HermeticCanaryAuthority { kind: \"sealed-hermetic-canary\", disposition: Fresh }"
        );
        assert!(!debug.contains("86420"));
        for diagnostic in rendered {
            assert!(!diagnostic.contains("86420"));
            assert!(!diagnostic.contains("secret-user"));
            assert!(!diagnostic.contains(root.to_string_lossy().as_ref()));
        }
        Ok(())
    }

    #[test]
    fn admission_writes_nothing_outside_envelope() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("envelope")?;
        let sentinel = case.outer.join("outside-sentinel");
        fs::write(&sentinel, b"unchanged")?;
        let before = entry_names(&case.outer)?;

        let (_root, authority) = case.create_authority()?;

        assert_eq!(entry_names(&case.outer)?, before);
        assert_eq!(fs::read(sentinel)?, b"unchanged");
        assert!(authority.compatibility_home().verify().is_ok());
        Ok(())
    }

    #[test]
    fn marker_replacement_invalidates_parent_authority() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("marker-replacement")?;
        let (root, authority) = case.create_authority()?;
        fs::rename(root.join(MARKER), root.join("marker-displaced"))?;
        create_private_file(&root.join(MARKER), MARKER_BYTES)?;

        assert!(authority.compatibility_home().verify().is_err());
        Ok(())
    }

    #[test]
    fn marker_hardlink_invalidates_parent_authority() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("marker-hardlink")?;
        let (root, authority) = case.create_authority()?;
        let displaced = root.join("marker-displaced");
        fs::rename(root.join(MARKER), &displaced)?;
        fs::hard_link(&displaced, root.join(MARKER))?;

        assert!(authority.compatibility_home().verify().is_err());
        Ok(())
    }

    #[test]
    fn missing_manifest_recovery_is_read_only() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("missing-manifest")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        fs::remove_file(root.join(MANIFEST))?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn wrong_manifest_recovery_is_read_only() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("wrong-manifest")?;
        let (root, authority) = case.create_authority()?;
        drop(authority);
        fs::write(root.join(MANIFEST), b"wrong-layout\n")?;
        let before = tree_snapshot(&root)?;

        assert!(case.recover(&root).is_err());
        assert_eq!(tree_snapshot(&root)?, before);
        Ok(())
    }

    #[test]
    fn manifest_replacement_invalidates_both_live_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("manifest-replacement")?;
        let (root, authority) = case.create_authority()?;
        let displaced = case.outer.join("manifest-displaced");
        fs::rename(root.join(MANIFEST), &displaced)?;
        create_private_file(&root.join(MANIFEST), &fs::read(displaced)?)?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn manifest_hardlink_invalidates_both_live_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("manifest-hardlink")?;
        let (root, authority) = case.create_authority()?;
        let displaced = case.outer.join("manifest-displaced");
        fs::rename(root.join(MANIFEST), &displaced)?;
        fs::hard_link(&displaced, root.join(MANIFEST))?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn extra_parent_entry_invalidates_both_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("extra-parent")?;
        let (root, authority) = case.create_authority()?;
        create_private_file(&root.join("extra"), b"extra")?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn extra_target_entry_invalidates_both_boundaries() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("extra-target")?;
        let (root, authority) = case.create_authority()?;
        create_private_directory(&root.join("targets").join("extra"))?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(
            authority
                .private_process_ledger()
                .production_boundary()
                .verify()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn lock_replacement_cannot_create_split_brain_authority() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("lock-replacement")?;
        let (root, authority) = case.create_authority()?;
        fs::rename(root.join(LOCK), root.join("lock-displaced"))?;
        create_private_file(&root.join(LOCK), b"")?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(case.recover(&root).is_err());
        Ok(())
    }

    #[test]
    fn lock_hardlink_invalidates_parent_authority() -> Result<(), Box<dyn Error>> {
        let case = TestEnvelope::new("lock-hardlink")?;
        let (root, authority) = case.create_authority()?;
        let displaced = root.join("lock-displaced");
        fs::rename(root.join(LOCK), &displaced)?;
        fs::hard_link(&displaced, root.join(LOCK))?;

        assert!(authority.compatibility_home().verify().is_err());
        assert!(case.recover(&root).is_err());
        Ok(())
    }
}
