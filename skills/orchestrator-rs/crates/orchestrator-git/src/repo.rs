//! Repository query and mutation operations: discovery, branch/worktree
//! lifecycle, commits, and diffs. Faithful port of the Go oracle's repo ops.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::GitError;
use crate::process::run;

/// Walks up from `dir` looking for a `.git` entry. Returns the directory
/// containing `.git`, or `None` if none is found up to the filesystem root.
#[must_use]
pub fn find_root(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        let parent = current.parent()?;
        current = parent.to_path_buf();
    }
}

/// Returns the short name of the currently checked-out branch in `repo_root`.
/// Errors if HEAD is detached.
pub fn current_branch(repo_root: &Path) -> Result<String, GitError> {
    let out = run(repo_root, &["git", "symbolic-ref", "--short", "HEAD"])?;
    Ok(out.trim().to_owned())
}

/// Creates a new branch `name` in `repo_root` off `base` (a branch, tag, or SHA).
pub fn create_branch(repo_root: &Path, name: &str, base: &str) -> Result<(), GitError> {
    run(repo_root, &["git", "branch", name, base])?;
    Ok(())
}

/// Adds a linked worktree at `path` checking out `branch`. The branch must
/// already exist.
pub fn create_worktree(repo_root: &Path, path: &Path, branch: &str) -> Result<(), GitError> {
    run(
        repo_root,
        &["git", "worktree", "add", &path.to_string_lossy(), branch],
    )?;
    Ok(())
}

/// Returns the root directory of the main (non-linked) worktree that owns the
/// repository at `dir`. Works from both the main and linked worktrees.
pub(crate) fn main_repo_root(dir: &Path) -> Result<PathBuf, GitError> {
    let out = run(dir, &["git", "rev-parse", "--git-common-dir"])?;
    let common = out.trim();
    let common_path = Path::new(common);
    let resolved = if common_path.is_absolute() {
        common_path.to_path_buf()
    } else {
        dir.join(common_path)
    };
    Ok(resolved
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.to_path_buf()))
}

/// Returns the full commit hash of HEAD in `dir`.
pub fn head_sha(dir: &Path) -> Result<String, GitError> {
    let out = run(dir, &["git", "rev-parse", "HEAD"])?;
    Ok(out.trim().to_owned())
}

/// Pushes `branch` to `remote` from within `worktree_path`.
pub fn push(worktree_path: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
    run(worktree_path, &["git", "push", remote, branch])?;
    Ok(())
}

/// Stages all changes (`git add -A`) and commits with `message`. Returns `Ok`
/// when there is nothing to commit, matching the Go oracle.
pub fn commit_all(worktree_path: &Path, message: &str) -> Result<(), GitError> {
    run(worktree_path, &["git", "add", "-A"])?;
    match run(worktree_path, &["git", "commit", "-m", message]) {
        Ok(_) => Ok(()),
        Err(GitError::Command { output })
            if output.expose_combined().contains("nothing to commit") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Reports whether `path` has any staged, unstaged, or untracked changes.
pub fn has_uncommitted_changes(path: &Path) -> Result<bool, GitError> {
    let out = run(path, &["git", "status", "--porcelain"])?;
    Ok(!out.trim().is_empty())
}

/// Returns the files that differ between the current worktree state and `base`.
pub fn diff_name_only(dir: &Path, base: &str) -> Result<Vec<String>, GitError> {
    name_only(dir, &["git", "diff", "--name-only", base])
}

/// Returns a map of file path to status letter for files differing from `base`.
/// The status letter is the first char of git's status field (e.g. 'R' for
/// `R100`), and the path is the last field (the new path for renames).
pub fn diff_name_status(dir: &Path, base: &str) -> Result<BTreeMap<String, String>, GitError> {
    let out = run(dir, &["git", "diff", "--name-status", base])?;
    let mut result = BTreeMap::new();
    for line in out.trim().lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let status = fields[0]
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_default();
        let path = fields[fields.len() - 1];
        result.insert(path.to_owned(), status);
    }
    Ok(result)
}

/// Returns the files changed on `head` relative to `base` (three-dot,
/// merge-base diff), run in `repo_root`.
pub fn changed_files(repo_root: &Path, base: &str, head: &str) -> Result<Vec<String>, GitError> {
    name_only(
        repo_root,
        &["git", "diff", "--name-only", &format!("{base}...{head}")],
    )
}

/// Returns the repo-relative tracked files in `repo_root`.
pub fn list_tracked_files(repo_root: &Path) -> Result<Vec<String>, GitError> {
    name_only(repo_root, &["git", "ls-files"])
}

/// Reports whether `base_branch` contains commits not reachable from
/// `feature_branch`, returning those one-line summaries. A non-empty list means
/// the base has advanced since the feature branch was created.
pub fn base_branch_moved(
    repo_root: &Path,
    base_branch: &str,
    feature_branch: &str,
) -> Result<(bool, Vec<String>), GitError> {
    let out = run(
        repo_root,
        &[
            "git",
            "log",
            "--oneline",
            base_branch,
            "--not",
            feature_branch,
        ],
    )?;
    Ok((!out.trim().is_empty(), lines_from(&out)))
}

/// Re-registers a linked worktree whose git metadata was lost.
pub fn repair_worktree(repo_root: &Path, worktree_path: &Path) -> Result<(), GitError> {
    run(
        repo_root,
        &[
            "git",
            "worktree",
            "repair",
            &worktree_path.to_string_lossy(),
        ],
    )?;
    Ok(())
}

/// Runs a name-listing git command and returns the non-empty trimmed lines.
pub(crate) fn name_only(dir: &Path, args: &[&str]) -> Result<Vec<String>, GitError> {
    let out = run(dir, args)?;
    Ok(lines_from(&out))
}

fn lines_from(output: &str) -> Vec<String> {
    output
        .trim()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// B4-DESIGN §2 — refreshed-base resolution
// ---------------------------------------------------------------------------

/// A full 40-character lowercase-hex commit hash.
///
/// The type exists so a base can never be a *name*: `RefreshedBase` is the only
/// producer of one for the isolation path, and [`create_branch_from_refreshed_base`]
/// is the only consumer, so a branch cannot be cut from a ref that another
/// process can move underneath it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    /// Parses a trimmed 40-character lowercase-hex hash. Abbreviated,
    /// uppercase, and non-hex inputs are refused.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.len() != 40 {
            return None;
        }
        if !trimmed
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        Some(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitSha {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether a remote may name anything other than a local filesystem path.
///
/// `LocalOnly` is the B4 posture: fixtures and dogfooding runs must be provably
/// offline, so a `scheme://` or `user@host:path` remote is refused *before* any
/// fetch is attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePolicy {
    /// Only a bare filesystem path or a `file://` URL is admitted.
    LocalOnly,
    /// Any remote URL git itself accepts.
    Any,
}

/// The exact base a mission is entitled to build on, observed after a fetch.
///
/// Construction is only possible through [`resolve_refreshed_base`], so a
/// caller cannot fabricate a base from a branch name it happens to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedBase {
    remote: String,
    base_branch: String,
    base_sha: CommitSha,
    local_sha: Option<CommitSha>,
}

impl RefreshedBase {
    #[must_use]
    pub fn remote(&self) -> &str {
        &self.remote
    }

    #[must_use]
    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }

    /// The commit `<remote>/<base_branch>` resolved to after the fetch. This is
    /// the value every downstream operation must use.
    #[must_use]
    pub fn base_sha(&self) -> &CommitSha {
        &self.base_sha
    }

    /// What the repository's own `refs/heads/<base_branch>` pointed at, when it
    /// exists. Recorded for the receipt; never used as a base.
    #[must_use]
    pub fn local_sha(&self) -> Option<&CommitSha> {
        self.local_sha.as_ref()
    }

    /// The TRK-1116 condition: the local base branch is behind, ahead of, or
    /// otherwise different from the remote one. Informational, never fatal —
    /// the refreshed sha wins either way.
    #[must_use]
    pub fn local_base_is_stale(&self) -> bool {
        self.local_sha
            .as_ref()
            .is_none_or(|local| local != &self.base_sha)
    }
}

/// Reports whether a git remote URL names something on this filesystem.
///
/// Accepts a bare path or a `file://` URL. Refuses every `scheme://` form and
/// git's scp-like `[user@]host:path` shorthand, which is detected the way git
/// detects it: a colon appearing before the first `/`.
fn remote_url_is_local(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    if let Some(rest) = url.strip_prefix("file://") {
        return !rest.is_empty();
    }
    if url.contains("://") {
        return false;
    }
    !url.split('/').next().unwrap_or(url).contains(':')
}

/// Classifies `remote`'s configured URL and refuses it when `policy` is
/// [`RemotePolicy::LocalOnly`] and the URL names anything but a local path.
///
/// Every command that would reach a remote runs this first, so a non-local
/// remote is refused *before* git is asked to make a connection. Passing the
/// remote by name and letting git resolve the URL at connect time is what
/// makes that impossible to check afterwards, which is why the resolution is
/// pulled forward to here.
///
/// The error carries the remote's name and never its URL: remote URLs can
/// embed credentials and this error is expected to reach ordinary logs.
fn classify_remote(dir: &Path, remote: &str, policy: RemotePolicy) -> Result<(), GitError> {
    let url = run(dir, &["git", "remote", "get-url", remote]).map_err(|_| {
        GitError::RemoteUnreachable {
            remote: remote.to_owned(),
        }
    })?;
    if policy == RemotePolicy::LocalOnly && !remote_url_is_local(&url) {
        return Err(GitError::RemoteNotLocal {
            remote: remote.to_owned(),
        });
    }
    Ok(())
}

/// Refuses a ref or remote name that could be read as an option or that git
/// would reject anyway. Applied before the name reaches an argv position.
fn validate_ref_component(name: &str) -> Result<(), GitError> {
    let unsafe_name = name.is_empty()
        || name.starts_with('-')
        || name
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace());
    if unsafe_name {
        return Err(GitError::UnsafeRefName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Fetches `base_branch` from `remote` and resolves the exact commit it now
/// points at (B4-DESIGN §2).
///
/// Every step fails closed and **there is no fallback to the local ref**: a
/// mission that cannot observe the remote base does not get to build on a
/// stale local one. That is the fix for TRK-1116, where mission worktrees
/// branched from a local `main` several merges behind `origin/main`.
///
/// The remote's URL is classified before the fetch, so under
/// [`RemotePolicy::LocalOnly`] a non-local remote is refused without any
/// network reach being attempted.
///
/// Errors deliberately do not carry the remote URL or git's captured output:
/// remote URLs can embed credentials, and this error is expected to reach
/// ordinary logs.
pub fn resolve_refreshed_base(
    repo_root: &Path,
    remote: &str,
    base_branch: &str,
    policy: RemotePolicy,
) -> Result<RefreshedBase, GitError> {
    validate_ref_component(remote)?;
    validate_ref_component(base_branch)?;

    // 1. Classify the remote, before any fetch.
    classify_remote(repo_root, remote, policy)?;

    // 2. Fetch exactly the one branch into its remote-tracking ref.
    let refspec = format!("+refs/heads/{base_branch}:refs/remotes/{remote}/{base_branch}");
    run(
        repo_root,
        &["git", "fetch", "--no-tags", "--prune", remote, &refspec],
    )
    .map_err(|_| GitError::RemoteUnreachable {
        remote: remote.to_owned(),
    })?;

    // 3 and 4. Resolve the tracking ref and observe the local one.
    read_recorded_base(repo_root, remote, base_branch)
}

/// Re-reads the base a prior [`resolve_refreshed_base`] already fetched,
/// **without contacting the remote** (B4-DESIGN §4 C1).
///
/// The remote-tracking ref `refs/remotes/<remote>/<base>` *is* the durable
/// record of a completed fetch: it was written by that fetch and nothing in
/// this system moves it except another fetch. A process resuming after a crash
/// therefore recovers the exact base its predecessor declared by reading that
/// ref, and must not fetch again — a second fetch could observe an advanced
/// remote and silently move the base out from under a mission that already
/// declared one.
///
/// # Errors
/// Returns [`GitError::BaseRefMissing`] when the tracking ref does not resolve
/// to a commit, which means no fetch ever completed for this base.
pub fn recorded_refreshed_base(
    repo_root: &Path,
    remote: &str,
    base_branch: &str,
) -> Result<RefreshedBase, GitError> {
    validate_ref_component(remote)?;
    validate_ref_component(base_branch)?;
    read_recorded_base(repo_root, remote, base_branch)
}

/// Steps 3 and 4 of the refreshed-base algorithm, shared by the fetching and
/// the non-fetching entry points. Callers have already validated both names.
fn read_recorded_base(
    repo_root: &Path,
    remote: &str,
    base_branch: &str,
) -> Result<RefreshedBase, GitError> {
    // The fully qualified ref is used so a local branch literally named
    // `<remote>/<base>` cannot shadow it.
    let tracking_ref = format!("refs/remotes/{remote}/{base_branch}");
    let missing = || GitError::BaseRefMissing {
        reference: tracking_ref.clone(),
    };
    let resolved = run(
        repo_root,
        &[
            "git",
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tracking_ref}^{{commit}}"),
        ],
    )
    .map_err(|_| missing())?;
    let base_sha = CommitSha::parse(&resolved).ok_or_else(missing)?;

    // The local ref is observed for the record only. Absence is tolerated.
    let local_sha = run(
        repo_root,
        &[
            "git",
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{base_branch}^{{commit}}"),
        ],
    )
    .ok()
    .and_then(|out| CommitSha::parse(&out));

    Ok(RefreshedBase {
        remote: remote.to_owned(),
        base_branch: base_branch.to_owned(),
        base_sha,
        local_sha,
    })
}

/// Resolves `refs/heads/<branch>` in `repo_root` to an exact commit.
///
/// `Ok(None)` means the branch genuinely does not exist. Crash reconciliation
/// (B4-DESIGN §4 C2/C4) depends on that distinction: an absent branch means the
/// effect never landed, while a branch at an unexpected sha carries work this
/// system must never discard.
pub fn branch_sha(repo_root: &Path, branch: &str) -> Result<Option<CommitSha>, GitError> {
    validate_ref_component(branch)?;
    Ok(run(
        repo_root,
        &[
            "git",
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )
    .ok()
    .and_then(|out| CommitSha::parse(&out)))
}

/// Reports whether `path` is registered as a linked worktree of `repo_root`.
///
/// Registration is read from `git worktree list --porcelain` rather than from
/// the directory's existence, because B4-DESIGN §4 C3 must distinguish an
/// adoptable worktree from an orphaned directory that only looks like one.
pub fn worktree_is_registered(repo_root: &Path, path: &Path) -> Result<bool, GitError> {
    let listing = run(repo_root, &["git", "worktree", "list", "--porcelain"])?;
    let target = std::fs::canonicalize(path).ok();
    Ok(listing.lines().any(|line| {
        line.strip_prefix("worktree ").is_some_and(|entry| {
            let entry = Path::new(entry.trim());
            entry == path
                || target
                    .as_deref()
                    .is_some_and(|target| std::fs::canonicalize(entry).is_ok_and(|e| e == target))
        })
    }))
}

/// Deletes `refs/heads/<branch>` from `repo_root`, but only when it still
/// points at `expected`.
///
/// The sha check is inside this function on purpose: a check performed by the
/// caller and a deletion performed here would be a check-then-act on a ref any
/// other process can move. Returns `false` without touching anything when the
/// branch is absent or has moved (B4-DESIGN §5.2 — never discard work).
pub fn delete_branch_at(
    repo_root: &Path,
    branch: &str,
    expected: &CommitSha,
) -> Result<bool, GitError> {
    validate_ref_component(branch)?;
    if branch_sha(repo_root, branch)?.as_ref() != Some(expected) {
        return Ok(false);
    }
    // `--force-with-lease` has no `git branch` equivalent, so the window
    // between the observation above and this deletion is closed by passing the
    // expected object to `-D`'s successor form: `git branch -d` refuses an
    // unmerged branch, and `update-ref -d <ref> <oldvalue>` deletes only when
    // the ref still holds `oldvalue`. The latter is the atomic form.
    run(
        repo_root,
        &[
            "git",
            "update-ref",
            "-d",
            &format!("refs/heads/{branch}"),
            expected.as_str(),
        ],
    )?;
    Ok(true)
}

/// Creates `name` in `repo_root` at the exact commit a [`RefreshedBase`]
/// resolved to.
///
/// This is the isolation path's only branch constructor. [`create_branch`] is
/// retained for the Go-parity plumbing, but it takes a caller-supplied string
/// and therefore cannot make the guarantee this function makes: the third argv
/// element here is always a 40-hex sha.
pub fn create_branch_from_refreshed_base(
    repo_root: &Path,
    name: &str,
    base: &RefreshedBase,
) -> Result<(), GitError> {
    validate_ref_component(name)?;
    run(repo_root, &["git", "branch", name, base.base_sha.as_str()])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// B4-DESIGN §4.3 — push acknowledgement classification
// ---------------------------------------------------------------------------

/// What is actually known about a push after it returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAck {
    /// Two independent facts held: the client exited zero **and** a subsequent
    /// `ls-remote` showed the remote branch at exactly the pushed sha.
    Acknowledged { remote_sha: CommitSha },
    /// The remote decidedly refused the update.
    Rejected,
    /// Nothing may be concluded. The push may or may not have landed; a caller
    /// must observe before acting, and must never blindly re-push.
    Ambiguous,
}

/// Reports whether git's captured output states an actual ref rejection, as
/// opposed to a transport or process failure.
fn output_states_a_rejection(output: &str) -> bool {
    output.contains("[rejected]")
        || output.contains("[remote rejected]")
        || output.contains("non-fast-forward")
}

/// Reads the remote's current `refs/heads/<branch>`.
///
/// `Ok(None)` means the remote was reached and the ref is genuinely absent;
/// `Err` means the remote state could not be observed at all. Recovery depends
/// on that distinction (B4-DESIGN §4 C5), so the two are never merged.
///
/// The remote is classified against `policy` first, so under
/// [`RemotePolicy::LocalOnly`] a non-local remote yields
/// [`GitError::RemoteNotLocal`] without `ls-remote` ever running.
pub fn ls_remote_branch(
    dir: &Path,
    remote: &str,
    branch: &str,
    policy: RemotePolicy,
) -> Result<Option<CommitSha>, GitError> {
    validate_ref_component(remote)?;
    validate_ref_component(branch)?;
    classify_remote(dir, remote, policy)?;
    let out = run(
        dir,
        &["git", "ls-remote", remote, &format!("refs/heads/{branch}")],
    )?;
    Ok(out
        .lines()
        .find_map(|line| CommitSha::parse(line.split_whitespace().next().unwrap_or(""))))
}

/// Pushes `branch` to `remote` and classifies the acknowledgement
/// (B4-DESIGN §4.3).
///
/// An exit code alone never yields [`PushAck::Acknowledged`]; the remote is
/// re-observed with `ls-remote` and must show exactly `pushed_sha`. Anything
/// else — a moved ref, an absent ref, an unobservable remote, a signal, a
/// deadline, or a stall — is [`PushAck::Ambiguous`], which the caller must
/// resolve by observing rather than by pushing again.
///
/// The remote is classified against `policy` before the push, so under
/// [`RemotePolicy::LocalOnly`] a non-local remote is refused with
/// [`GitError::RemoteNotLocal`] and no connection is attempted. A refusal is
/// an error rather than a [`PushAck`] variant on purpose: it says nothing
/// about the remote's state, and every `PushAck` is a claim about that state.
pub fn push_with_acknowledgement(
    worktree_path: &Path,
    remote: &str,
    branch: &str,
    pushed_sha: &CommitSha,
    policy: RemotePolicy,
) -> Result<PushAck, GitError> {
    validate_ref_component(remote)?;
    validate_ref_component(branch)?;
    classify_remote(worktree_path, remote, policy)?;

    match crate::process::run_outcome(worktree_path, &["git", "push", remote, branch])? {
        crate::process::RunOutcome::Success { .. } => {}
        crate::process::RunOutcome::Failed { output } => {
            return Ok(if output_states_a_rejection(output.expose_combined()) {
                PushAck::Rejected
            } else {
                PushAck::Ambiguous
            });
        }
        crate::process::RunOutcome::Anomalous => return Ok(PushAck::Ambiguous),
    }

    Ok(
        match ls_remote_branch(worktree_path, remote, branch, policy) {
            Ok(Some(remote_sha)) if remote_sha == *pushed_sha => {
                PushAck::Acknowledged { remote_sha }
            }
            Ok(_) | Err(_) => PushAck::Ambiguous,
        },
    )
}

#[cfg(test)]
mod refreshed_base_tests {
    use super::*;

    #[test]
    fn commit_sha_admits_only_full_lowercase_hex() {
        assert!(CommitSha::parse("0123456789abcdef0123456789abcdef01234567").is_some());
        assert!(CommitSha::parse(" 0123456789abcdef0123456789abcdef01234567\n").is_some());
        assert!(CommitSha::parse("0123456789ABCDEF0123456789abcdef01234567").is_none());
        assert!(CommitSha::parse("0123456").is_none());
        assert!(CommitSha::parse("main").is_none());
    }

    #[test]
    fn local_remote_urls_are_paths_and_file_urls_only() {
        assert!(remote_url_is_local("/tmp/origin.git"));
        assert!(remote_url_is_local("./relative/origin.git"));
        assert!(remote_url_is_local("file:///tmp/origin.git"));

        assert!(!remote_url_is_local("https://example.invalid/x.git"));
        assert!(!remote_url_is_local("ssh://git@example.invalid/x.git"));
        assert!(!remote_url_is_local("git@example.invalid:owner/x.git"));
        assert!(!remote_url_is_local("example.invalid:owner/x.git"));
        assert!(!remote_url_is_local(""));
        assert!(!remote_url_is_local("file://"));
    }

    #[test]
    fn option_shaped_ref_names_are_refused() {
        assert!(validate_ref_component("--upload-pack=touch /tmp/x").is_err());
        assert!(validate_ref_component("").is_err());
        assert!(validate_ref_component("has space").is_err());
        assert!(validate_ref_component("origin").is_ok());
        assert!(validate_ref_component("via/mission-1/slug").is_ok());
    }

    #[test]
    fn only_an_explicit_ref_rejection_counts_as_rejected() {
        assert!(output_states_a_rejection(
            " ! [rejected]        main -> main (non-fast-forward)"
        ));
        assert!(output_states_a_rejection(
            " ! [remote rejected] main -> main (pre-receive hook declined)"
        ));
        assert!(!output_states_a_rejection(
            "fatal: could not read from remote repository."
        ));
    }
}
