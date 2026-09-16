use crate::{
    fixture_authority::{FixtureAuthorityError, FreshFixtureAuthority, TargetRootAuthority},
    fs_util::{
        FileIdentity, atomic_replace_private, create_dir_private, create_private_file, identity,
        mode, open_dir_path_nofollow, read_bounded_nofollow, sync_dir,
    },
    workspace::{WorkspaceAuthority, WorkspaceError},
};
use cap_std::fs::Dir;
use orchestrator_core::WorkerId;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use thiserror::Error;

const MAX_SETTINGS_BYTES: usize = 256 * 1024;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const RECORD_NAME: &str = ".orchestrator-settings-overlay.json";
const SETTINGS_NAME: &str = "settings.local.json";
const BACKUP_NAME: &str = "settings.local.json.orchestrator-backup";
static TRANSACTION_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerRole {
    Planner,
    Implementer,
    Reviewer,
}

/// Closed, deterministic deny policy. Callers cannot provide arbitrary settings JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoleDenyPolicy {
    role: WorkerRole,
}

impl RoleDenyPolicy {
    #[must_use]
    pub const fn for_role(role: WorkerRole) -> Self {
        Self { role }
    }

    pub fn encode(self) -> Result<Vec<u8>, OverlayError> {
        const SHARED: &[&str] = &[
            "Bash(git push)",
            "Bash(git checkout -b)",
            "Bash(git branch -D)",
            "Bash(git branch -d)",
            "Bash(git reset --hard)",
            "Bash(git merge)",
            "Bash(git rebase)",
            "Bash(git stash drop)",
            "Bash(gh pr create)",
            "Bash(gh pr merge)",
            "Bash(gh pr close)",
            "Bash(gh issue)",
            "Bash(rm -rf /)",
            "Bash(rm -rf ~)",
        ];
        const ALLOW: &[&str] = &[
            "Glob",
            "Grep",
            "Read",
            "TaskOutput",
            "WebFetch",
            "WebSearch",
        ];
        let mut deny = SHARED.to_vec();
        if matches!(self.role, WorkerRole::Planner | WorkerRole::Reviewer) {
            deny.push("Edit");
        }
        #[derive(Serialize)]
        struct Permissions<'a> {
            allow: &'a [&'a str],
            deny: Vec<&'a str>,
        }
        #[derive(Serialize)]
        struct Settings<'a> {
            permissions: Permissions<'a>,
        }
        serde_json::to_vec_pretty(&Settings {
            permissions: Permissions { allow: ALLOW, deny },
        })
        .map_err(OverlayError::Serialize)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayExit {
    Success,
    Failure,
    Cancelled,
    TimedOut,
    Unwind,
}

#[derive(Debug, Error)]
pub enum OverlayError {
    #[error(transparent)]
    Authority(#[from] FixtureAuthorityError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("settings overlay policy serialization failed: {0}")]
    Serialize(serde_json::Error),
    #[error("settings overlay recovery record is invalid: {0}")]
    InvalidRecord(String),
    #[error("settings overlay conflicts with a pre-existing backup or recovery record")]
    ExistingTransaction,
    #[error("settings overlay encountered a symlink, non-regular file, or wrong private mode")]
    UnsafeEntry,
    #[error("settings overlay input exceeds 256 KiB")]
    SettingsTooLarge,
    #[error("settings overlay target was concurrently edited; recovery evidence is preserved")]
    ConcurrentEdit,
    #[error("settings overlay transaction mutex is poisoned")]
    Poisoned,
    #[error("settings overlay filesystem operation failed at {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordState {
    Prepared,
    BackedUp,
    Installed,
    Restoring,
    Restored,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryRecord {
    version: u32,
    transaction_id: String,
    target_id: String,
    target_device: u64,
    target_inode: u64,
    original_existed: bool,
    original_hex: String,
    overlay_hex: String,
    claude_created: bool,
    state: RecordState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore_origin: Option<RecordState>,
}

impl RecoveryRecord {
    fn original(&self) -> Result<Vec<u8>, OverlayError> {
        decode_hex(&self.original_hex)
    }

    fn overlay(&self) -> Result<Vec<u8>, OverlayError> {
        decode_hex(&self.overlay_hex)
    }
}

enum TransactionStatus {
    Installed,
    Restored,
    Conflict,
}

struct OverlayTransaction {
    target: Option<TargetRootAuthority>,
    worker_directory: Dir,
    claude_directory: Dir,
    claude_identity: FileIdentity,
    record: RecoveryRecord,
    status: TransactionStatus,
}

struct OverlayShared {
    transaction: Mutex<OverlayTransaction>,
    restore_on_drop: AtomicBool,
}

impl Drop for OverlayShared {
    fn drop(&mut self) {
        if self.restore_on_drop.load(Ordering::Acquire) {
            let transaction = self
                .transaction
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = restore_transaction(transaction);
        }
    }
}

/// Shareable teardown token. Every clone converges on the same serialized restore transaction.
#[derive(Clone)]
pub struct SettingsOverlay {
    shared: Arc<OverlayShared>,
}

impl fmt::Debug for SettingsOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SettingsOverlay")
            .field("kind", &"fixture-settings-overlay")
            .finish()
    }
}

impl SettingsOverlay {
    pub fn install(
        target: TargetRootAuthority,
        workspace: &WorkspaceAuthority,
        worker: &WorkerId,
        policy: RoleDenyPolicy,
    ) -> Result<Self, OverlayError> {
        target.verify()?;
        let workspace_boundary = workspace.verified_fixture_boundary()?;
        if !Arc::ptr_eq(&target.boundary, workspace_boundary) {
            return Err(OverlayError::InvalidRecord(
                "target and workspace belong to different fixture authorities".to_owned(),
            ));
        }
        workspace.prepare_worker(worker)?;
        let overlay = policy.encode()?;
        if overlay.len() > MAX_SETTINGS_BYTES {
            return Err(OverlayError::SettingsTooLarge);
        }

        let target_identity = target.identity;
        {
            let mut leases = target
                .extras
                .target_leases
                .lock()
                .map_err(|_| OverlayError::Poisoned)?;
            if !leases.insert(target_identity) {
                return Err(OverlayError::ExistingTransaction);
            }
        }

        match install_leased(target, workspace, worker, overlay) {
            Ok(transaction) => Ok(Self {
                shared: Arc::new(OverlayShared {
                    transaction: Mutex::new(transaction),
                    restore_on_drop: AtomicBool::new(true),
                }),
            }),
            Err(error) => {
                if !error_requires_retained_lease(&error)
                    && !has_recovery_evidence(workspace, worker)
                {
                    workspace
                        .extras()?
                        .target_leases
                        .lock()
                        .map_err(|_| OverlayError::Poisoned)?
                        .remove(&target_identity);
                }
                Err(error)
            }
        }
    }

    pub fn restore(&self) -> Result<(), OverlayError> {
        let mut transaction = self
            .shared
            .transaction
            .lock()
            .map_err(|_| OverlayError::Poisoned)?;
        restore_transaction(&mut transaction)
    }

    pub fn finish(self, _exit: OverlayExit) -> Result<(), OverlayError> {
        self.restore()
    }

    /// Fixture crash-test hook: leave durable evidence for explicit recovery.
    /// This cannot target a production home and refuses while teardown clones exist.
    pub fn abandon_for_recovery(self) -> Result<(), OverlayError> {
        if Arc::strong_count(&self.shared) != 1 {
            return Err(OverlayError::ExistingTransaction);
        }
        let mut transaction = self
            .shared
            .transaction
            .lock()
            .map_err(|_| OverlayError::Poisoned)?;
        release_lease(&mut transaction)?;
        self.shared.restore_on_drop.store(false, Ordering::Release);
        drop(transaction);
        Ok(())
    }
}

impl FreshFixtureAuthority {
    /// Resolves one durable fixture overlay record. Missing records are an idempotent no-op.
    pub fn recover_settings_overlay(
        &self,
        workspace: &WorkspaceAuthority,
        worker: &WorkerId,
    ) -> Result<bool, OverlayError> {
        let workspace_boundary = workspace.verified_fixture_boundary()?;
        if !Arc::ptr_eq(&self.boundary, workspace_boundary) {
            return Err(OverlayError::InvalidRecord(
                "workspace belongs to a different fixture authority".to_owned(),
            ));
        }
        let worker_directory = workspace.worker_directory(worker)?;
        let Some(bytes) =
            read_bounded_nofollow(&worker_directory, Path::new(RECORD_NAME), MAX_RECORD_BYTES)
                .map_err(|source| OverlayError::Io {
                    operation: "read settings recovery record",
                    source,
                })?
        else {
            return Ok(false);
        };
        let record: RecoveryRecord = serde_json::from_slice(&bytes)
            .map_err(|error| OverlayError::InvalidRecord(error.to_string()))?;
        if record.version != 1 || !crate::fs_util::validate_component(&record.target_id) {
            return Err(OverlayError::InvalidRecord(
                "unsupported version or target identifier".to_owned(),
            ));
        }
        let _original = record.original()?;
        let _overlay = record.overlay()?;
        let target = self.open_target(&record.target_id)?;
        let target_identity = target.identity;
        if target_identity.device != record.target_device
            || target_identity.inode != record.target_inode
        {
            return Err(OverlayError::ConcurrentEdit);
        }
        {
            let mut leases = target
                .extras
                .target_leases
                .lock()
                .map_err(|_| OverlayError::Poisoned)?;
            if !leases.insert(target_identity) {
                return Err(OverlayError::ExistingTransaction);
            }
        }
        let claude_directory = match open_dir_path_nofollow(&target.directory, Path::new(".claude"))
        {
            Ok(directory) => directory,
            Err(source)
                if source.kind() == std::io::ErrorKind::NotFound
                    && record.claude_created
                    && !record.original_existed
                    && matches!(record.state, RecordState::Restoring | RecordState::Restored) =>
            {
                worker_directory
                    .remove_file(RECORD_NAME)
                    .map_err(|source| OverlayError::Io {
                        operation: "remove completed settings recovery record",
                        source,
                    })?;
                sync_dir(&worker_directory).map_err(|source| OverlayError::Io {
                    operation: "sync completed settings recovery",
                    source,
                })?;
                target
                    .extras
                    .target_leases
                    .lock()
                    .map_err(|_| OverlayError::Poisoned)?
                    .remove(&target_identity);
                return Ok(true);
            }
            Err(source) => {
                target
                    .extras
                    .target_leases
                    .lock()
                    .map_err(|_| OverlayError::Poisoned)?
                    .remove(&target_identity);
                return Err(OverlayError::Io {
                    operation: "open settings directory for recovery",
                    source,
                });
            }
        };
        let claude_identity =
            identity(
                &claude_directory
                    .dir_metadata()
                    .map_err(|source| OverlayError::Io {
                        operation: "inspect settings directory for recovery",
                        source,
                    })?,
            );
        let mut transaction = OverlayTransaction {
            target: Some(target),
            worker_directory,
            claude_directory,
            claude_identity,
            record,
            status: TransactionStatus::Installed,
        };
        match restore_transaction(&mut transaction) {
            Ok(()) => Ok(true),
            Err(error) => Err(error),
        }
    }
}

fn error_requires_retained_lease(error: &OverlayError) -> bool {
    matches!(
        error,
        OverlayError::Io {
            operation: "sync worker after recovery-record removal",
            ..
        } | OverlayError::Poisoned
    )
}

fn has_recovery_evidence(workspace: &WorkspaceAuthority, worker: &WorkerId) -> bool {
    let directory = match workspace.worker_directory(worker) {
        Ok(directory) => directory,
        Err(_) => return true,
    };
    match directory.symlink_metadata(RECORD_NAME) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

fn install_leased(
    target: TargetRootAuthority,
    workspace: &WorkspaceAuthority,
    worker: &WorkerId,
    overlay: Vec<u8>,
) -> Result<OverlayTransaction, OverlayError> {
    let worker_directory = workspace.worker_directory(worker)?;
    ensure_absent(
        &worker_directory,
        RECORD_NAME,
        OverlayError::ExistingTransaction,
    )?;
    let (claude_directory, claude_created) =
        match create_dir_private(&target.directory, Path::new(".claude")) {
            Ok(()) => (
                open_dir_path_nofollow(&target.directory, Path::new(".claude")).map_err(
                    |source| OverlayError::Io {
                        operation: "open created target settings directory",
                        source,
                    },
                )?,
                true,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                open_dir_path_nofollow(&target.directory, Path::new(".claude")).map_err(
                    |source| OverlayError::Io {
                        operation: "open target settings directory",
                        source,
                    },
                )?,
                false,
            ),
            Err(source) => {
                return Err(OverlayError::Io {
                    operation: "create target settings directory",
                    source,
                });
            }
        };
    let claude_metadata = claude_directory
        .dir_metadata()
        .map_err(|source| OverlayError::Io {
            operation: "inspect target settings directory",
            source,
        })?;
    if mode(&claude_metadata) != 0o700 {
        return Err(OverlayError::UnsafeEntry);
    }
    ensure_absent(
        &claude_directory,
        BACKUP_NAME,
        OverlayError::ExistingTransaction,
    )?;

    let original = match read_bounded_nofollow(
        &claude_directory,
        Path::new(SETTINGS_NAME),
        MAX_SETTINGS_BYTES,
    ) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(OverlayError::SettingsTooLarge);
        }
        Err(source) => {
            return Err(OverlayError::Io {
                operation: "read original target settings",
                source,
            });
        }
    };
    if claude_directory.symlink_metadata(SETTINGS_NAME).is_ok() && original.is_none() {
        return Err(OverlayError::UnsafeEntry);
    }

    let transaction_id = format!(
        "fixture-{}-{}",
        std::process::id(),
        TRANSACTION_NONCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut record = RecoveryRecord {
        version: 1,
        transaction_id,
        target_id: target.id.clone(),
        target_device: target.identity.device,
        target_inode: target.identity.inode,
        original_existed: original.is_some(),
        original_hex: encode_hex(original.as_deref().unwrap_or_default()),
        overlay_hex: encode_hex(&overlay),
        claude_created,
        state: RecordState::Prepared,
        restore_origin: None,
    };
    if let Err(error) = write_record(&worker_directory, &record) {
        if claude_created
            && claude_directory
                .entries()
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false)
        {
            let _ = target.directory.remove_dir(".claude");
            let _ = sync_dir(&target.directory);
        }
        return Err(error);
    }

    let install_result = (|| {
        if let Some(original) = &original {
            publish_if_matches(
                &claude_directory,
                BACKUP_NAME,
                None,
                Some(original),
                &record.transaction_id,
            )?;
            validate_private_exact(&claude_directory, BACKUP_NAME, original)?;
            record.state = RecordState::BackedUp;
            write_record(&worker_directory, &record)?;
        }
        publish_if_matches(
            &claude_directory,
            SETTINGS_NAME,
            original.as_deref(),
            Some(&overlay),
            &record.transaction_id,
        )?;
        validate_private_exact(&claude_directory, SETTINGS_NAME, &overlay)?;
        record.state = RecordState::Installed;
        write_record(&worker_directory, &record)
    })();

    let mut transaction = OverlayTransaction {
        claude_identity: identity(&claude_metadata),
        target: Some(target),
        worker_directory,
        claude_directory,
        record,
        status: TransactionStatus::Installed,
    };
    if let Err(error) = install_result {
        let restore = restore_transaction(&mut transaction);
        return match restore {
            Ok(()) => Err(error),
            Err(restore_error) => Err(restore_error),
        };
    }
    Ok(transaction)
}

fn restore_transaction(transaction: &mut OverlayTransaction) -> Result<(), OverlayError> {
    match transaction.status {
        TransactionStatus::Restored => return Ok(()),
        TransactionStatus::Conflict => return Err(OverlayError::ConcurrentEdit),
        TransactionStatus::Installed => {}
    }
    let Some(target) = transaction.target.as_ref() else {
        transaction.status = TransactionStatus::Restored;
        return Ok(());
    };
    target.verify()?;
    let current_claude =
        open_dir_path_nofollow(&target.directory, Path::new(".claude")).map_err(|source| {
            OverlayError::Io {
                operation: "reopen target settings directory for restore",
                source,
            }
        })?;
    if identity(
        &current_claude
            .dir_metadata()
            .map_err(|source| OverlayError::Io {
                operation: "inspect target settings directory during restore",
                source,
            })?,
    ) != transaction.claude_identity
    {
        transaction.status = TransactionStatus::Conflict;
        return Err(OverlayError::ConcurrentEdit);
    }

    if transaction.record.state != RecordState::Restoring {
        transaction.record.restore_origin = Some(transaction.record.state);
    }
    transaction.record.state = RecordState::Restoring;
    write_record(&transaction.worker_directory, &transaction.record)?;
    let original = transaction.record.original()?;
    let overlay = transaction.record.overlay()?;
    let current = read_bounded_nofollow(
        &transaction.claude_directory,
        Path::new(SETTINGS_NAME),
        MAX_SETTINGS_BYTES,
    )
    .map_err(|source| OverlayError::Io {
        operation: "read settings during restore",
        source,
    })?;

    if transaction.record.original_existed {
        match current.as_deref() {
            Some(bytes) if bytes == overlay => {
                let backup = read_bounded_nofollow(
                    &transaction.claude_directory,
                    Path::new(BACKUP_NAME),
                    MAX_SETTINGS_BYTES,
                )
                .map_err(|source| OverlayError::Io {
                    operation: "read settings backup during restore",
                    source,
                })?;
                match backup {
                    Some(bytes) if bytes == original => {
                        validate_private_exact(
                            &transaction.claude_directory,
                            BACKUP_NAME,
                            &original,
                        )?;
                        publish_if_matches(
                            &transaction.claude_directory,
                            SETTINGS_NAME,
                            Some(&overlay),
                            Some(&original),
                            &transaction.record.transaction_id,
                        )?;
                        transaction
                            .claude_directory
                            .remove_file(BACKUP_NAME)
                            .map_err(|source| OverlayError::Io {
                                operation: "remove consumed exact settings backup",
                                source,
                            })?;
                    }
                    None => {
                        publish_if_matches(
                            &transaction.claude_directory,
                            SETTINGS_NAME,
                            Some(&overlay),
                            Some(&original),
                            &transaction.record.transaction_id,
                        )?;
                    }
                    Some(_) => return mark_conflict(transaction),
                }
            }
            Some(bytes) if bytes == original => {
                match read_bounded_nofollow(
                    &transaction.claude_directory,
                    Path::new(BACKUP_NAME),
                    MAX_SETTINGS_BYTES,
                )
                .map_err(|source| OverlayError::Io {
                    operation: "inspect backup beside restored original",
                    source,
                })? {
                    Some(backup)
                        if backup == original
                            && matches!(
                                transaction.record.restore_origin,
                                Some(RecordState::Prepared | RecordState::BackedUp)
                            ) =>
                    {
                        validate_private_exact(
                            &transaction.claude_directory,
                            BACKUP_NAME,
                            &original,
                        )?;
                        transaction
                            .claude_directory
                            .remove_file(BACKUP_NAME)
                            .map_err(|source| OverlayError::Io {
                                operation: "remove redundant exact backup",
                                source,
                            })?;
                    }
                    None => {}
                    Some(_) => return mark_conflict(transaction),
                }
            }
            _ => return mark_conflict(transaction),
        }
        validate_private_exact(&transaction.claude_directory, SETTINGS_NAME, &original)?;
    } else {
        match current.as_deref() {
            Some(bytes) if bytes == overlay => publish_if_matches(
                &transaction.claude_directory,
                SETTINGS_NAME,
                Some(&overlay),
                None,
                &transaction.record.transaction_id,
            )?,
            None => {}
            _ => return mark_conflict(transaction),
        }
        if transaction
            .claude_directory
            .symlink_metadata(BACKUP_NAME)
            .is_ok()
        {
            return mark_conflict(transaction);
        }
    }
    sync_dir(&transaction.claude_directory).map_err(|source| OverlayError::Io {
        operation: "sync restored settings directory",
        source,
    })?;

    let restored = read_bounded_nofollow(
        &transaction.claude_directory,
        Path::new(SETTINGS_NAME),
        MAX_SETTINGS_BYTES,
    )
    .map_err(|source| OverlayError::Io {
        operation: "verify restored target settings",
        source,
    })?;
    if (transaction.record.original_existed && restored.as_deref() != Some(original.as_slice()))
        || (!transaction.record.original_existed && restored.is_some())
        || transaction
            .claude_directory
            .symlink_metadata(BACKUP_NAME)
            .is_ok()
    {
        return mark_conflict(transaction);
    }

    if transaction.record.claude_created {
        let empty = transaction
            .claude_directory
            .entries()
            .map_err(|source| OverlayError::Io {
                operation: "inspect created settings directory",
                source,
            })?
            .next()
            .is_none();
        if empty {
            target
                .directory
                .remove_dir(".claude")
                .map_err(|source| OverlayError::Io {
                    operation: "remove transaction-created settings directory",
                    source,
                })?;
            sync_dir(&target.directory).map_err(|source| OverlayError::Io {
                operation: "sync target after settings-directory removal",
                source,
            })?;
        }
    }

    transaction.record.state = RecordState::Restored;
    write_record(&transaction.worker_directory, &transaction.record)?;
    transaction
        .worker_directory
        .remove_file(RECORD_NAME)
        .map_err(|source| OverlayError::Io {
            operation: "remove settings recovery record",
            source,
        })?;
    sync_dir(&transaction.worker_directory).map_err(|source| OverlayError::Io {
        operation: "sync worker after recovery-record removal",
        source,
    })?;
    release_lease(transaction)?;
    transaction.status = TransactionStatus::Restored;
    Ok(())
}

fn mark_conflict<T>(transaction: &mut OverlayTransaction) -> Result<T, OverlayError> {
    transaction.status = TransactionStatus::Conflict;
    Err(OverlayError::ConcurrentEdit)
}

fn release_lease(transaction: &mut OverlayTransaction) -> Result<(), OverlayError> {
    if let Some(target) = transaction.target.take() {
        target
            .extras
            .target_leases
            .lock()
            .map_err(|_| OverlayError::Poisoned)?
            .remove(&target.identity);
    }
    Ok(())
}

fn write_record(directory: &Dir, record: &RecoveryRecord) -> Result<(), OverlayError> {
    let bytes = serde_json::to_vec(record).map_err(OverlayError::Serialize)?;
    atomic_replace_private(directory, Path::new(RECORD_NAME), &bytes).map_err(|source| {
        OverlayError::Io {
            operation: "write settings recovery record",
            source,
        }
    })
}

fn validate_private_exact(
    directory: &Dir,
    name: &str,
    expected: &[u8],
) -> Result<(), OverlayError> {
    let file =
        crate::fs_util::open_file_nofollow(directory, Path::new(name)).map_err(|source| {
            OverlayError::Io {
                operation: "open private settings transaction file",
                source,
            }
        })?;
    let metadata = file.metadata().map_err(|source| OverlayError::Io {
        operation: "inspect private settings transaction file",
        source,
    })?;
    if !metadata.is_file() || mode(&metadata) != 0o600 {
        return Err(OverlayError::UnsafeEntry);
    }
    let actual = read_bounded_nofollow(directory, Path::new(name), MAX_SETTINGS_BYTES)
        .map_err(|source| OverlayError::Io {
            operation: "read private settings transaction file",
            source,
        })?
        .ok_or(OverlayError::UnsafeEntry)?;
    if actual != expected {
        return Err(OverlayError::ConcurrentEdit);
    }
    Ok(())
}

fn ensure_absent(directory: &Dir, name: &str, present: OverlayError) -> Result<(), OverlayError> {
    match directory.symlink_metadata(name) {
        Ok(_) => Err(present),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(OverlayError::Io {
            operation: "inspect transaction path for absence",
            source,
        }),
    }
}

fn publish_if_matches(
    directory: &Dir,
    final_name: &str,
    expected: Option<&[u8]>,
    desired: Option<&[u8]>,
    transaction_id: &str,
) -> Result<(), OverlayError> {
    let stage_name = format!(".{final_name}.orchestrator-stage-{transaction_id}");
    let quarantine_name = format!(".{final_name}.orchestrator-quarantine-{transaction_id}");
    ensure_absent(directory, &stage_name, OverlayError::ExistingTransaction)?;
    ensure_absent(
        directory,
        &quarantine_name,
        OverlayError::ExistingTransaction,
    )?;
    if let Some(bytes) = desired {
        create_private_file(directory, Path::new(&stage_name), bytes, false).map_err(|source| {
            OverlayError::Io {
                operation: "stage no-clobber settings transaction file",
                source,
            }
        })?;
    }

    if let Some(expected) = expected {
        if let Err(source) = directory.rename(
            Path::new(final_name),
            directory,
            Path::new(&quarantine_name),
        ) {
            let _ = directory.remove_file(&stage_name);
            return if source.kind() == std::io::ErrorKind::NotFound {
                Err(OverlayError::ConcurrentEdit)
            } else {
                Err(OverlayError::Io {
                    operation: "quarantine expected settings file",
                    source,
                })
            };
        }
        let quarantined =
            read_bounded_nofollow(directory, Path::new(&quarantine_name), MAX_SETTINGS_BYTES)
                .map_err(|source| OverlayError::Io {
                    operation: "verify quarantined settings file",
                    source,
                })?;
        if quarantined.as_deref() != Some(expected) {
            let restored = directory
                .hard_link(
                    Path::new(&quarantine_name),
                    directory,
                    Path::new(final_name),
                )
                .is_ok();
            if restored {
                let _ = directory.remove_file(&quarantine_name);
            }
            let _ = directory.remove_file(&stage_name);
            let _ = sync_dir(directory);
            return Err(OverlayError::ConcurrentEdit);
        }
    } else {
        match directory.symlink_metadata(final_name) {
            Ok(_) => {
                let _ = directory.remove_file(&stage_name);
                return Err(OverlayError::ConcurrentEdit);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                let _ = directory.remove_file(&stage_name);
                return Err(OverlayError::Io {
                    operation: "verify absent settings transaction target",
                    source,
                });
            }
        }
    }

    if desired.is_some() {
        if let Err(source) =
            directory.hard_link(Path::new(&stage_name), directory, Path::new(final_name))
        {
            let _ = directory.remove_file(&stage_name);
            let _ = directory.remove_file(&quarantine_name);
            let _ = sync_dir(directory);
            return if source.kind() == std::io::ErrorKind::AlreadyExists {
                Err(OverlayError::ConcurrentEdit)
            } else {
                Err(OverlayError::Io {
                    operation: "publish no-clobber settings transaction file",
                    source,
                })
            };
        }
    }
    let _ = directory.remove_file(&stage_name);
    let _ = directory.remove_file(&quarantine_name);
    sync_dir(directory).map_err(|source| OverlayError::Io {
        operation: "sync no-clobber settings transaction",
        source,
    })
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, OverlayError> {
    if value.len() % 2 != 0 || value.len() / 2 > MAX_SETTINGS_BYTES {
        return Err(OverlayError::InvalidRecord(
            "invalid bounded hex payload".to_owned(),
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Result<u8, OverlayError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(OverlayError::InvalidRecord("invalid hex digit".to_owned())),
    }
}
