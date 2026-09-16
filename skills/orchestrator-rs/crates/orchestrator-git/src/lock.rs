//! The `.nanika-lock` worktree liveness lock.
//!
//! Faithful port of the Go oracle: a 0600 JSON file at the worktree root
//! recording the holding PID, mission id, start time, and current phase. A
//! worktree is considered locked only when its recorded PID is still running.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::time_util;
use crate::{GitError, write_private_file};

/// The lock file name, matching the Go oracle constant.
pub const LOCK_FILE_NAME: &str = ".nanika-lock";

#[derive(Serialize, Deserialize)]
struct WorktreeLock {
    pid: i64,
    mission_id: String,
    started_at: String,
    phase: String,
}

/// Writes a fresh `.nanika-lock` with the current PID, mission id, an RFC3339
/// start time, and an empty initial phase.
pub fn write_lock(worktree_path: &Path, mission_id: &str) -> Result<(), GitError> {
    let lock = WorktreeLock {
        pid: i64::from(std::process::id()),
        mission_id: mission_id.to_owned(),
        started_at: time_util::utc_rfc3339_now(),
        phase: String::new(),
    };
    let data = serde_json::to_vec(&lock)?;
    write_private_file(&worktree_path.join(LOCK_FILE_NAME), &data)
}

/// Updates the `phase` field of an existing lock file. No-op when absent.
pub fn update_lock_phase(worktree_path: &Path, phase: &str) -> Result<(), GitError> {
    let path = worktree_path.join(LOCK_FILE_NAME);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(GitError::Io(error)),
    };
    let mut lock: WorktreeLock = serde_json::from_slice(&data)?;
    lock.phase = phase.to_owned();
    let updated = serde_json::to_vec(&lock)?;
    write_private_file(&path, &updated)
}

/// Best-effort removes the lock file; errors are ignored.
pub fn remove_lock(worktree_path: &Path) {
    let _ = std::fs::remove_file(worktree_path.join(LOCK_FILE_NAME));
}

/// Returns true when `path` holds a lock whose recorded PID is a still-running
/// process. A missing, unparseable, or stale (PID <= 0 / dead) lock is not live.
#[must_use]
pub fn is_locked(path: &Path) -> bool {
    let data = match std::fs::read(path.join(LOCK_FILE_NAME)) {
        Ok(data) => data,
        Err(_) => return false,
    };
    let lock: WorktreeLock = match serde_json::from_slice(&data) {
        Ok(lock) => lock,
        Err(_) => return false,
    };
    if lock.pid <= 0 {
        return false;
    }
    pid_alive(lock.pid)
}

/// Tests whether `pid` refers to a running process via a null signal (signal 0),
/// matching Go's `proc.Signal(syscall.Signal(0))`. EPERM (process exists but is
/// not signalable by us) is treated as not-running, exactly like the oracle.
#[cfg(unix)]
fn pid_alive(pid: i64) -> bool {
    use rustix::process::{Pid, test_kill_process};
    let Some(pid) = Pid::from_raw(pid as i32) else {
        return false;
    };
    test_kill_process(pid).is_ok()
}

#[cfg(not(unix))]
fn pid_alive(_pid: i64) -> bool {
    false
}
