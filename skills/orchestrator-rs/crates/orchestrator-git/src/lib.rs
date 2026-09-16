//! Git-isolation plumbing for the Rust orchestrator.
//!
//! Ports the Go `internal/git` package behavior tracked by contract
//! ORC-GIT-001. It shells out to the `git` CLI and owns the branch/worktree
//! lifecycle, the `.nanika-lock` liveness lock, per-phase commits, changed-file
//! claims, and soft trash cleanup. The contract remains subject to parity
//! evidence rather than being declared complete by this implementation.
//!
//! Like the Go oracle, every `git` subprocess strips `GIT_DIR`, `GIT_WORK_TREE`,
//! and `GIT_INDEX_FILE` so a linked worktree is never redirected into the
//! parent's repository. Unlike Go's inherit-all-except-three behavior, Rust
//! otherwise forwards only an audited compatibility allowlist from a trusted
//! operator environment; see the process module for the authority boundary.

mod claims;
mod lock;
mod pr;
mod process;
mod repo;
mod time_util;
mod trash;

use std::{fmt, path::Path};

use thiserror::Error;

pub use claims::{ClaimsDb, ClaimsDbError, Conflict, claim_changed_files, open_claims_db};
pub use lock::{LOCK_FILE_NAME, is_locked, remove_lock, update_lock_phase, write_lock};
pub use pr::{
    FIXTURE_PR_PROVIDER, FixturePrAdapter, PrAdapter, PrAdapterError, PrMetadata, PrReceipt,
    PrRequest, add_pr_labels, add_pr_reviewers, build_pr_body, comment_on_pr, create_pr,
    fixture_receipt_id, has_codex, has_gh, run_codex_review,
};
pub use process::{
    GitChildIdentity, forget_supervised_child_identity, last_supervised_child_identity,
};
pub use repo::{
    CommitSha, PushAck, RefreshedBase, RemotePolicy, base_branch_moved, branch_sha, changed_files,
    commit_all, create_branch, create_branch_from_refreshed_base, create_worktree, current_branch,
    delete_branch_at, diff_name_only, diff_name_status, find_root, has_uncommitted_changes,
    head_sha, list_tracked_files, ls_remote_branch, push, push_with_acknowledgement,
    recorded_refreshed_base, repair_worktree, resolve_refreshed_base, worktree_is_registered,
};
pub use trash::{
    ConfinementRefusal, TRASH_META_FILE_NAME, TrashMeta, remove_worktree, remove_worktree_confined,
};

/// Maximum slug length, matching the Go oracle (`maxSlugLen = 40`). The limit
/// applies only to the task-derived slug; the `via/<mission-id>/` prefix is
/// not bounded.
pub const MAX_SLUG_LEN: usize = 40;

/// Failures raised by the git-isolation plumbing.
#[derive(Debug, Error)]
pub enum GitError {
    /// A command exited non-zero. Captured output is bounded and available only
    /// through an explicit accessor; ordinary formatting remains redacted.
    #[error("git command failed (captured output redacted)")]
    Command { output: CapturedCommandOutput },
    /// The git binary could not be spawned.
    #[error("could not run git: {source}")]
    Spawn { source: std::io::Error },
    /// A filesystem operation against a lock, trash entry, or worktree failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// JSON encode/decode of lock or trash metadata failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The worktree is locked by a still-running process and must not be removed.
    #[error("worktree {path} is locked by an active process")]
    Locked { path: std::path::PathBuf },
    /// The remote could not be observed or fetched from. There is deliberately
    /// no fallback to a local ref, and the remote's URL is never included:
    /// remote URLs can carry credentials and this error reaches ordinary logs.
    #[error("remote {remote} could not be reached")]
    RemoteUnreachable { remote: String },
    /// The remote names something other than a local filesystem path under
    /// [`RemotePolicy::LocalOnly`]. Raised before any fetch is attempted.
    #[error("remote {remote} is not a local filesystem remote")]
    RemoteNotLocal { remote: String },
    /// The remote was fetched but the expected ref does not resolve to a commit.
    #[error("ref {reference} does not resolve to a commit")]
    BaseRefMissing { reference: String },
    /// A ref or remote name could be read as a command-line option, or is
    /// otherwise unusable in an argv position.
    #[error("ref name is not usable")]
    UnsafeRefName { name: String },
    /// A cleanup target escapes its confinement root, or is reachable only
    /// through a symlink.
    #[error("refused to clean up {path}: {refusal}")]
    OutsideConfinement {
        path: std::path::PathBuf,
        refusal: ConfinementRefusal,
    },
}

/// Bounded command output retained for programmatic compatibility checks.
///
/// Git itself can include repository paths, remote URLs, or credential-helper
/// diagnostics in stderr, so neither `Debug` nor `Display` reveals the bytes.
pub struct CapturedCommandOutput(String);

impl CapturedCommandOutput {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// Explicitly exposes stdout followed by stderr. Callers must not place the
    /// result in ordinary logs or user-visible error formatting.
    #[must_use]
    pub fn expose_combined(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CapturedCommandOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedCommandOutput")
            .field("bytes", &self.0.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Builds the isolation branch name `via/<mission_id>/<slug>`.
///
/// The mission id is inserted verbatim (it is not slugified), exactly as the Go
/// oracle does. The slug is [`slugify`] applied to the task text.
#[must_use]
pub fn branch_name(mission_id: &str, task: &str) -> String {
    format!("via/{mission_id}/{}", slugify(task))
}

/// Derives a URL/git-safe slug from arbitrary text.
///
/// Faithful byte-level port of the Go `Slugify`:
/// 1. lowercase the whole input;
/// 2. replace every run of non-`[a-z0-9-]` bytes with a single `-`;
/// 3. collapse consecutive hyphens into one;
/// 4. trim leading and trailing hyphens;
/// 5. truncate to [`MAX_SLUG_LEN`] bytes, then strip any trailing hyphen the
///    truncation introduced;
/// 6. fall back to the literal `"task"` when the result is empty.
///
/// Because every non-`[a-z0-9-]` byte collapses to `-`, the emitted slug is
/// always pure ASCII, so byte truncation is UTF-8-safe.
#[must_use]
pub fn slugify(input: &str) -> String {
    let lower = input.to_lowercase();
    let mut out: Vec<u8> = Vec::with_capacity(lower.len());
    for &byte in lower.as_bytes() {
        let keep = byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        if keep && byte == b'-' {
            if out.last() == Some(&b'-') {
                continue;
            }
            out.push(b'-');
        } else if keep {
            out.push(byte);
        } else if out.last() != Some(&b'-') {
            out.push(b'-');
        }
    }

    // Trim leading and trailing hyphens.
    let start = out.iter().position(|&b| b != b'-').unwrap_or(out.len());
    let end = out.len()
        - out
            .iter()
            .rev()
            .position(|&b| b != b'-')
            .unwrap_or(out.len());
    let mut slug: Vec<u8> = if start >= end {
        Vec::new()
    } else {
        out.get(start..end).unwrap_or(&[]).to_vec()
    };

    slug.truncate(MAX_SLUG_LEN);
    while slug.last() == Some(&b'-') {
        slug.pop();
    }
    if slug.is_empty() {
        return "task".to_owned();
    }
    // `slug` only ever contains ASCII bytes (a-z, 0-9, '-'), so it is valid UTF-8.
    String::from_utf8(slug).unwrap_or_else(|_| "task".to_owned())
}

/// Writes `data` to `path` with mode 0600 (owner read/write) on Unix, matching
/// Go's `os.WriteFile(..., 0600)`. Best-effort private on non-Unix.
pub(crate) fn write_private_file(path: &Path, data: &[u8]) -> Result<(), GitError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn slugify_collapses_spaces_to_single_hyphens() {
        assert_eq!(
            slugify("implement login feature"),
            "implement-login-feature"
        );
    }

    #[test]
    fn slugify_truncates_to_max_length() {
        let slug = slugify(&"a".repeat(100));
        assert!(
            slug.len() <= MAX_SLUG_LEN,
            "slug {slug:?} exceeded {MAX_SLUG_LEN} bytes (len={})",
            slug.len()
        );
    }

    #[test]
    fn slugify_has_no_trailing_hyphen_at_truncation_boundary() {
        // 38 a's + "- extra words here": truncation must not land on a hyphen.
        let input = format!("{}- extra words here", "a".repeat(38));
        let slug = slugify(&input);
        assert!(!slug.ends_with('-'), "slug {slug:?} ended with a hyphen");
        assert!(slug.len() <= MAX_SLUG_LEN);
    }

    #[test]
    fn slugify_keeps_only_slug_chars() {
        let slug = slugify("Fix: bug #123 (urgent!)");
        assert!(
            slug.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "slug {slug:?} contained a non-slug character"
        );
    }

    #[test]
    fn slugify_lowercases_input() {
        assert_eq!(slugify("IMPLEMENT OAUTH2 FLOW"), "implement-oauth2-flow");
    }

    #[test]
    fn slugify_empty_input_falls_back_to_task() {
        assert_eq!(slugify(""), "task");
    }

    #[test]
    fn slugify_all_special_chars_fall_back_to_task() {
        let slug = slugify("!@#$%^&*()");
        assert!(!slug.is_empty());
        assert!(!slug.starts_with('-'));
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slugify_collapses_repeated_separators() {
        assert_eq!(slugify("add   user   auth"), "add-user-auth");
    }

    #[test]
    fn slugify_preserves_digits() {
        assert_eq!(slugify("fix issue 42"), "fix-issue-42");
    }

    #[test]
    fn branch_name_formats_with_prefix_and_mission_id() -> TestResult {
        let name = branch_name("20260714-ab12cd34", "implement login feature");
        assert_eq!(name, "via/20260714-ab12cd34/implement-login-feature");
        Ok(())
    }

    #[test]
    fn branch_name_uses_task_fallback_for_empty_slug() {
        assert_eq!(branch_name("mission-1", ""), "via/mission-1/task");
    }

    #[test]
    fn branch_name_does_not_slugify_mission_id() {
        // The mission id is inserted verbatim, even if it contains non-slug chars.
        let name = branch_name("20260714-Ab12CD", "do thing");
        assert_eq!(name, "via/20260714-Ab12CD/do-thing");
    }

    #[test]
    fn captured_command_output_is_redacted_from_error_and_debug() {
        let secret = "credential-canary-do-not-print";
        let error = GitError::Command {
            output: CapturedCommandOutput::new(secret.to_owned()),
        };

        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
        if let GitError::Command { output } = error {
            assert_eq!(output.expose_combined(), secret);
        }
    }
}
