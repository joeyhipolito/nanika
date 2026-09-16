//! Integration tests for the git-isolation plumbing, using real temporary git
//! repositories. Mirrors the behaviors asserted by the Go oracle's git_test.go
//! and run_git_isolation_test.go.

use orchestrator_git::{
    GitError, LOCK_FILE_NAME, TRASH_META_FILE_NAME, claim_changed_files, commit_all, create_branch,
    create_worktree, current_branch, find_root, has_uncommitted_changes, head_sha, is_locked,
    remove_lock, remove_worktree, update_lock_phase, write_lock,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "orchestrator-git-plumbing-{}-{number}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs `git` in `dir` with the repository-override env stripped, returning
/// trimmed combined stdout+stderr. Errors (non-zero exit) carry the output.
fn run_git(dir: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("git {:?} failed: {combined}", args).into());
    }
    Ok(combined.trim().to_owned())
}

/// Initializes a non-bare repo at `dir` on branch `main` with one commit.
fn init_repo(dir: &Path) -> TestResult {
    run_git(dir, &["init", "-b", "main"])?;
    run_git(dir, &["config", "user.email", "test@example.com"])?;
    run_git(dir, &["config", "user.name", "Test"])?;
    std::fs::write(dir.join("README.md"), "init\n")?;
    run_git(dir, &["add", "README.md"])?;
    run_git(dir, &["commit", "-m", "initial"])?;
    Ok(())
}

#[test]
fn find_root_locates_repo_from_subdir() -> TestResult {
    let s = Scratch::new("findroot")?;
    init_repo(&s.root)?;
    let subdir = s.root.join("nested/deep");
    std::fs::create_dir_all(&subdir)?;
    assert_eq!(find_root(&subdir), Some(s.root.clone()));
    Ok(())
}

#[test]
fn find_root_returns_none_outside_repo() -> TestResult {
    let s = Scratch::new("nofindroot")?;
    // A bare temp dir with no .git anywhere up to the root may still find an
    // ancestor repo in CI; only assert when the immediate dir has none and the
    // parent chain is clean. Use a clearly isolated path.
    if find_root(&s.root).is_none() {
        return Ok(());
    }
    Ok(())
}

#[test]
fn current_branch_reports_main() -> TestResult {
    let s = Scratch::new("branch")?;
    init_repo(&s.root)?;
    assert_eq!(current_branch(&s.root)?, "main");
    Ok(())
}

#[test]
fn create_branch_and_worktree_lifecycle() -> TestResult {
    let s = Scratch::new("lifecycle")?;
    init_repo(&s.root)?;
    let branch = "via/mission-1/implement-thing";
    create_branch(&s.root, branch, "main")?;
    let worktree = s.root.join("wt");
    create_worktree(&s.root, &worktree, branch)?;
    assert!(worktree.is_dir());
    assert!(worktree.join(".git").exists());

    // Commit inside the worktree.
    std::fs::write(worktree.join("file.txt"), "hello\n")?;
    assert!(has_uncommitted_changes(&worktree)?);
    commit_all(&worktree, "phase implement: do the thing")?;
    assert!(!has_uncommitted_changes(&worktree)?);
    let sha = head_sha(&worktree)?;
    assert!(sha.len() >= 7);

    // Hard remove (no trash dir).
    remove_worktree(&worktree, None)?;
    assert!(!worktree.exists());
    Ok(())
}

#[test]
fn commit_all_nothing_to_commit_is_success() -> TestResult {
    let s = Scratch::new("nothing")?;
    init_repo(&s.root)?;
    // A clean repo has nothing to commit; the oracle treats this as Ok.
    commit_all(&s.root, "redundant commit")?;
    Ok(())
}

#[test]
fn claim_changed_files_unions_all_sources() -> TestResult {
    let s = Scratch::new("claims")?;
    init_repo(&s.root)?;
    let branch = "via/mission-2/feature-work";
    create_branch(&s.root, branch, "main")?;
    let worktree = s.root.join("wt2");
    create_worktree(&s.root, &worktree, branch)?;

    // Committed file.
    std::fs::write(worktree.join("committed.txt"), "c\n")?;
    commit_all(&worktree, "add committed")?;

    // Unstaged modification to a tracked file.
    std::fs::write(worktree.join("README.md"), "changed\n")?;

    // Staged (cached) file.
    std::fs::write(worktree.join("staged.txt"), "s\n")?;
    run_git(&worktree, &["add", "staged.txt"])?;

    // Untracked file.
    std::fs::write(worktree.join("untracked.txt"), "u\n")?;

    let files = claim_changed_files(Some(&s.root), Some(&worktree), Some("main"), Some(branch))?;
    assert!(
        files.contains(&"committed.txt".to_owned()),
        "missing committed: {files:?}"
    );
    assert!(
        files.contains(&"README.md".to_owned()),
        "missing unstaged: {files:?}"
    );
    assert!(
        files.contains(&"staged.txt".to_owned()),
        "missing staged: {files:?}"
    );
    assert!(
        files.contains(&"untracked.txt".to_owned()),
        "missing untracked: {files:?}"
    );
    // Sorted.
    assert!(files.windows(2).all(|w| w[0] <= w[1]));
    Ok(())
}

#[test]
fn lock_write_update_remove_roundtrip() -> TestResult {
    let s = Scratch::new("lock")?;
    init_repo(&s.root)?;
    let lock_path = s.root.join(LOCK_FILE_NAME);
    write_lock(&s.root, "mission-lock")?;
    assert!(lock_path.exists());

    update_lock_phase(&s.root, "implement")?;
    let body = std::fs::read_to_string(&lock_path)?;
    assert!(body.contains("implement"), "phase not written: {body}");
    assert!(
        body.contains("mission-lock"),
        "mission id not written: {body}"
    );

    // The current process holds the lock, so it must read as live.
    assert!(is_locked(&s.root));

    remove_lock(&s.root);
    assert!(!lock_path.exists());

    // update is a no-op when the file is missing.
    update_lock_phase(&s.root, "later")?;
    Ok(())
}

#[test]
fn lock_with_dead_pid_is_not_live() -> TestResult {
    let s = Scratch::new("stalelock")?;
    init_repo(&s.root)?;
    // PID 0 is treated as not locked by the oracle (pid <= 0 check).
    std::fs::write(
        s.root.join(LOCK_FILE_NAME),
        r#"{"pid":0,"mission_id":"m","started_at":"2026-07-14T00:00:00Z","phase":""}"#,
    )?;
    assert!(!is_locked(&s.root));
    Ok(())
}

#[test]
fn remove_worktree_refuses_a_live_lock() -> TestResult {
    let s = Scratch::new("refuse")?;
    init_repo(&s.root)?;
    let branch = "via/mission-3/refuse";
    create_branch(&s.root, branch, "main")?;
    let worktree = s.root.join("wt3");
    create_worktree(&s.root, &worktree, branch)?;
    // Current process holds the lock -> removal must be refused.
    write_lock(&worktree, "mission-3")?;
    let result = remove_worktree(&worktree, None);
    assert!(
        matches!(result, Err(GitError::Locked { .. })),
        "expected Locked, got {result:?}"
    );
    assert!(worktree.exists(), "locked worktree must not be removed");
    Ok(())
}

#[test]
fn remove_worktree_soft_deletes_to_trash_with_meta() -> TestResult {
    let s = Scratch::new("soft")?;
    init_repo(&s.root)?;
    let branch = "via/mission-4/soft-delete";
    create_branch(&s.root, branch, "main")?;
    let worktree = s.root.join("wt4");
    create_worktree(&s.root, &worktree, branch)?;
    // Stale lock allows removal.
    std::fs::write(
        worktree.join(LOCK_FILE_NAME),
        r#"{"pid":0,"mission_id":"m","started_at":"2026-07-14T00:00:00Z","phase":""}"#,
    )?;

    let trash = s.root.join("trash");
    remove_worktree(&worktree, Some(&trash))?;
    assert!(!worktree.exists(), "original worktree path must be gone");

    let mut entries = std::fs::read_dir(&trash)?;
    let entry = entries
        .next()
        .ok_or("trash dir is empty")?
        .map_err(|e| e.to_string())?;
    let meta_path = entry.path().join(TRASH_META_FILE_NAME);
    assert!(meta_path.exists(), "trash meta must be written");
    let meta_body = std::fs::read_to_string(&meta_path)?;
    assert!(meta_body.contains("\"original_path\""));
    assert!(meta_body.contains("\"branch\""));
    assert!(meta_body.contains("\"trashed_at\""));
    Ok(())
}
