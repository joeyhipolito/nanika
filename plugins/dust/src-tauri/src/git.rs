//! Tauri commands for git operations.
//!
//! Operations: status, staging, commits, push (gated), file reveal, external URLs.

use serde::{Deserialize, Serialize};
use std::process::Command;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoStatus {
    pub branch: String,
    pub changes: Vec<FileChange>,
    pub has_staged: bool,
    pub has_unstaged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: String,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Get repository status via `git status --porcelain=v2 --branch`.
#[tauri::command]
pub fn get_repo_status(repo_root: String) -> Result<RepoStatus, String> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain=v2")
        .arg("--branch")
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git status: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git status failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("git output not valid UTF-8: {e}"))?;

    parse_git_status(&stdout)
}

/// Stage all changes in the repository.
#[tauri::command]
pub fn git_stage_all(repo_root: String) -> Result<(), String> {
    let output = Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git add: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git add failed: {stderr}"));
    }

    Ok(())
}

/// Create a commit with the given message.
#[tauri::command]
pub fn git_commit(repo_root: String, message: String) -> Result<(), String> {
    let output = Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git commit: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git commit failed: {stderr}"));
    }

    Ok(())
}

/// Push commits to remote. MUST gate on WHIM_ALLOW_PUSH env var.
#[tauri::command]
pub fn git_push(repo_root: String) -> Result<(), String> {
    // Gate on WHIM_ALLOW_PUSH environment variable
    if std::env::var("WHIM_ALLOW_PUSH").is_err() {
        return Err("push not allowed: set WHIM_ALLOW_PUSH environment variable".to_string());
    }

    let output = Command::new("git")
        .arg("push")
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git push: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git push failed: {stderr}"));
    }

    Ok(())
}

/// Reveal a file in Finder (macOS) via `open -R`.
#[tauri::command]
pub fn reveal_in_finder(path: String) -> Result<(), String> {
    let output = Command::new("open")
        .arg("-R")
        .arg(&path)
        .output()
        .map_err(|e| format!("failed to run open -R: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("open -R failed: {stderr}"));
    }

    Ok(())
}

/// Open an external URL via Tauri or `open` command.
#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    let output = Command::new("open")
        .arg(&url)
        .output()
        .map_err(|e| format!("failed to run open: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("open failed: {stderr}"));
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_git_status(output: &str) -> Result<RepoStatus, String> {
    let mut branch = String::new();
    let mut changes = Vec::new();
    let mut has_staged = false;
    let mut has_unstaged = false;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        // porcelain=v2 line types:
        //   1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
        //   2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\t<origPath>
        //   u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
        //   ? <path>
        //   ! <path>
        // All fields before the path are space-separated; the path is the
        // remainder of the line so it can contain spaces.

        if let Some(rest) = line.strip_prefix("1 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path> — peel XY, skip 6.
            if let Some((xy, path)) = split_after_n_spaces(rest, 6) {
                record_xy(xy, path, &mut changes, &mut has_staged, &mut has_unstaged);
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\t<origPath>
            // — peel XY, skip 7, take new path before the embedded tab.
            if let Some((xy, tail)) = split_after_n_spaces(rest, 7) {
                let path = tail.split('\t').next().unwrap_or(tail);
                record_xy(xy, path, &mut changes, &mut has_staged, &mut has_unstaged);
            }
        } else if let Some(rest) = line.strip_prefix("u ") {
            // <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path> — peel XY, skip 8.
            if let Some((xy, path)) = split_after_n_spaces(rest, 8) {
                record_xy(xy, path, &mut changes, &mut has_staged, &mut has_unstaged);
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            has_unstaged = true;
            changes.push(FileChange {
                path: path.to_string(),
                status: "??".to_string(),
            });
        }
        // `! <path>` (ignored) lines are intentionally skipped.
    }

    if branch.is_empty() {
        branch = "unknown".to_string();
    }

    Ok(RepoStatus {
        branch,
        changes,
        has_staged,
        has_unstaged,
    })
}

/// Returns `(first_field, remainder_after_n_spaces)`, where `remainder` is the
/// substring after skipping `n` additional space-separated fields. Used to
/// peel the `<XY>` token off the head and skip past the fixed metadata fields
/// to land on the path.
fn split_after_n_spaces(input: &str, skip: usize) -> Option<(&str, &str)> {
    let (first, mut rest) = input.split_once(' ')?;
    for _ in 0..skip {
        let (_, next) = rest.split_once(' ')?;
        rest = next;
    }
    Some((first, rest))
}

fn record_xy(
    xy: &str,
    path: &str,
    changes: &mut Vec<FileChange>,
    has_staged: &mut bool,
    has_unstaged: &mut bool,
) {
    if xy.len() < 2 || path.is_empty() {
        return;
    }
    let staged = xy.chars().next().unwrap_or('.');
    let unstaged = xy.chars().nth(1).unwrap_or('.');

    if staged != '.' && staged != ' ' {
        *has_staged = true;
    }
    if unstaged != '.' && unstaged != ' ' {
        *has_unstaged = true;
    }

    changes.push(FileChange {
        path: path.to_string(),
        status: format!("{staged}{unstaged}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_head() {
        let out = "# branch.oid 2a618d9e692077736de14b1394a9934887bc9ae0\n\
                   # branch.head main\n";
        let status = parse_git_status(out).unwrap();
        assert_eq!(status.branch, "main");
        assert!(status.changes.is_empty());
        assert!(!status.has_staged);
        assert!(!status.has_unstaged);
    }

    #[test]
    fn parses_modified_and_untracked() {
        let out = "# branch.oid 2a618d9e692077736de14b1394a9934887bc9ae0\n\
                   # branch.head main\n\
                   1 MM N... 100644 100644 100644 e69de29 d95f3ad a.txt\n\
                   ? b.txt\n\
                   ? c.txt\n";
        let status = parse_git_status(out).unwrap();
        assert_eq!(status.branch, "main");
        assert_eq!(status.changes.len(), 3);
        assert_eq!(status.changes[0].path, "a.txt");
        assert_eq!(status.changes[0].status, "MM");
        assert_eq!(status.changes[1].path, "b.txt");
        assert_eq!(status.changes[1].status, "??");
        assert!(status.has_staged);
        assert!(status.has_unstaged);
    }

    #[test]
    fn parses_renamed_entry() {
        // 2 R. N... 100644 100644 100644 <hH> <hI> R100 new-name.txt\told-name.txt
        let out = "# branch.head main\n\
                   2 R. N... 100644 100644 100644 e69de29 e69de29 R100 new.txt\told.txt\n";
        let status = parse_git_status(out).unwrap();
        assert_eq!(status.changes.len(), 1);
        assert_eq!(status.changes[0].path, "new.txt");
        assert_eq!(status.changes[0].status, "R.");
        assert!(status.has_staged);
        assert!(!status.has_unstaged);
    }

    #[test]
    fn missing_branch_falls_back_to_unknown() {
        let status = parse_git_status("").unwrap();
        assert_eq!(status.branch, "unknown");
    }
}
