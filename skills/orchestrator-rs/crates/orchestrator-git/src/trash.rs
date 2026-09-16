//! Soft-delete trash and metadata for removed worktrees.
//!
//! Faithful port of the Go oracle's `RemoveWorktree` + `TrashMeta`: hard-delete
//! when no trash dir is given, otherwise move the worktree into a timestamped
//! trash entry, write a 0600 `.nanika-trash-meta.json` beside it, and prune the
//! dangling git reference.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lock::is_locked;
use crate::process::run;
use crate::repo::{current_branch, main_repo_root};
use crate::time_util;
use crate::{GitError, write_private_file};

/// The trash metadata file name, matching the Go oracle constant.
pub const TRASH_META_FILE_NAME: &str = ".nanika-trash-meta.json";

/// Metadata recorded inside a trash entry so restore can work standalone.
#[derive(Debug, Serialize, Deserialize)]
pub struct TrashMeta {
    pub workspace_id: String,
    pub original_path: String,
    pub branch: String,
    pub repo_root: String,
    pub trashed_at: String,
}

/// Removes the worktree at `path`. With `trash_dir = None` it hard-deletes
/// (`git worktree remove --force` + prune); with `Some(dir)` it soft-moves the
/// directory into a timestamped trash entry, writes restore metadata, and prunes
/// the dangling reference (prune failure is non-fatal). Refuses a locked
/// worktree.
pub fn remove_worktree(path: &Path, trash_dir: Option<&Path>) -> Result<(), GitError> {
    if is_locked(path) {
        return Err(GitError::Locked {
            path: path.to_path_buf(),
        });
    }
    let main_root = main_repo_root(path).unwrap_or_else(|_| path.to_path_buf());

    let Some(trash_dir) = trash_dir else {
        run(
            &main_root,
            &[
                "git",
                "worktree",
                "remove",
                "--force",
                &path.to_string_lossy(),
            ],
        )?;
        run(&main_root, &["git", "worktree", "prune"])?;
        return Ok(());
    };

    std::fs::create_dir_all(trash_dir)?;

    let stamp = time_util::utc_compact_stamp_now();
    let workspace_id = path
        .file_name()
        .map(std::ffi::OsStr::to_string_lossy)
        .unwrap_or_default()
        .into_owned();
    let trash_entry: PathBuf = trash_dir.join(format!("{workspace_id}_{stamp}"));
    let branch = current_branch(path).unwrap_or_default();

    let meta = TrashMeta {
        workspace_id: workspace_id.clone(),
        original_path: path.to_string_lossy().into_owned(),
        branch,
        repo_root: main_root.to_string_lossy().into_owned(),
        trashed_at: time_util::utc_rfc3339_now(),
    };

    std::fs::rename(path, &trash_entry)?;

    if let Ok(meta_data) = serde_json::to_vec(&meta) {
        let _ = write_private_file(&trash_entry.join(TRASH_META_FILE_NAME), &meta_data);
    }

    if let Err(error) = run(&main_root, &["git", "worktree", "prune"]) {
        // Non-fatal: the worktree is already in trash, only the git ref is stale.
        eprintln!("warning: git worktree prune: {error}");
    }

    println!("worktree moved to trash: {}", trash_entry.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// B4-DESIGN §5.1 — trash confinement
// ---------------------------------------------------------------------------

/// Why a cleanup target was refused. Carried by
/// [`GitError::OutsideConfinement`] so a refusal is diagnosable without
/// widening the error enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfinementRefusal {
    /// The final path component is a symlink. It is refused before
    /// canonicalization, so a link pointing back inside the root is still
    /// refused rather than followed.
    Symlink,
    /// The canonical target does not live under the canonical confinement root.
    OutsideRoot,
    /// The computed trash entry does not live under the confinement root.
    TrashEntryOutsideRoot,
}

impl std::fmt::Display for ConfinementRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Symlink => "the final path component is a symlink",
            Self::OutsideRoot => "the target is outside the confinement root",
            Self::TrashEntryOutsideRoot => "the trash entry is outside the confinement root",
        })
    }
}

/// Refuses `path` unless its final component is a real directory entry (not a
/// symlink) and it canonicalizes to somewhere under `root`.
///
/// The symlink check comes first and is deliberately lexical-final rather than
/// resolved: a link is refused even when it points back inside the root,
/// because renaming *through* a link moves something the caller did not name.
fn confine(path: &Path, root: &Path) -> Result<PathBuf, GitError> {
    let refuse = |refusal| GitError::OutsideConfinement {
        path: path.to_path_buf(),
        refusal,
    };
    if std::fs::symlink_metadata(path)?.is_symlink() {
        return Err(refuse(ConfinementRefusal::Symlink));
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let canonical = std::fs::canonicalize(path)?;
    if !canonical.starts_with(&canonical_root) {
        return Err(refuse(ConfinementRefusal::OutsideRoot));
    }
    Ok(canonical)
}

/// Soft-deletes the worktree at `path` into `trash_dir`, refusing any target
/// that escapes `confinement_root` (B4-DESIGN §5.1).
///
/// Order of checks, all before any filesystem mutation:
/// 1. a live `.nanika-lock` refuses removal outright;
/// 2. `path`'s final component must not be a symlink;
/// 3. `path` must canonicalize under `confinement_root`;
/// 4. the trash directory and the computed entry get checks 2 and 3 too.
///
/// Removal is always a `rename` into the trash entry — never a recursive
/// delete — so cleanup stays recoverable and a cross-device move fails loudly
/// instead of degrading into copy-then-unlink. Returns the trash entry path.
///
/// Unlike [`remove_worktree`], there is no hard-delete branch: a caller that
/// holds a confinement root always gets a recoverable trash entry.
pub fn remove_worktree_confined(
    path: &Path,
    trash_dir: &Path,
    confinement_root: &Path,
) -> Result<PathBuf, GitError> {
    if is_locked(path) {
        return Err(GitError::Locked {
            path: path.to_path_buf(),
        });
    }

    let canonical_worktree = confine(path, confinement_root)?;
    // The main repository root is both the recorded provenance of the trash
    // entry and the directory `git worktree prune` is later run in. Falling
    // back to `path` produced neither: `path` is about to be renamed away, so
    // the prune would run against a directory that no longer exists and the
    // recorded `repo_root` would name the worktree rather than its repository.
    // Nothing has been mutated yet at this point, so failing closed is free.
    let main_root = main_repo_root(&canonical_worktree)?;
    let branch = current_branch(&canonical_worktree).unwrap_or_default();

    std::fs::create_dir_all(trash_dir)?;
    let canonical_trash_dir = confine(trash_dir, confinement_root)?;

    let workspace_id = canonical_worktree
        .file_name()
        .map(std::ffi::OsStr::to_string_lossy)
        .unwrap_or_default()
        .into_owned();
    let stamp = time_util::utc_compact_stamp_now();
    let trash_entry = canonical_trash_dir.join(format!("{workspace_id}_{stamp}"));
    // The entry does not exist yet, so it cannot be canonicalized. Its parent
    // is already confined, so confining the entry reduces to refusing a name
    // that could climb back out of it.
    if trash_entry.parent() != Some(canonical_trash_dir.as_path()) {
        return Err(GitError::OutsideConfinement {
            path: trash_entry,
            refusal: ConfinementRefusal::TrashEntryOutsideRoot,
        });
    }

    let meta = TrashMeta {
        workspace_id,
        original_path: canonical_worktree.to_string_lossy().into_owned(),
        branch,
        repo_root: main_root.to_string_lossy().into_owned(),
        trashed_at: time_util::utc_rfc3339_now(),
    };

    std::fs::rename(&canonical_worktree, &trash_entry)?;
    write_private_file(
        &trash_entry.join(TRASH_META_FILE_NAME),
        &serde_json::to_vec(&meta)?,
    )?;

    // Non-fatal: the worktree is already in trash, only the git ref is stale.
    let _ = run(&main_root, &["git", "worktree", "prune"]);
    Ok(trash_entry)
}
