//! PR creation helpers via the `gh` and `codex` CLIs.
//!
//! Ports the Go oracle's `pr.go` command and body behavior while intentionally
//! replacing its ambient environment inheritance with explicit compatibility
//! allowlists. This is a hardening deviation, not a claim that the PR parity
//! contract is complete. `gh` and `codex` run through the shared
//! [`orchestrator_process`] supervisor, so a hung child or a chatty review
//! prompt cannot block or exhaust the orchestrator. PR/comment bodies and
//! review prompts travel over stdin rather than process argv.

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use orchestrator_process::{ProcessError, ProcessSpec};
use thiserror::Error;

use crate::{GitError, process::classify_unsuccessful_termination, repo::CommitSha};

/// Hard wall-clock deadline for a single `gh`/`codex` invocation. Codex review
/// can take longer than a git command, but this still catches genuine hangs.
const EXTERNAL_RUN_DEADLINE: Duration = Duration::from_secs(120);
/// Per-stream output cap for external CLI output.
const EXTERNAL_RUN_MAX_OUTPUT: usize = 8 * 1024 * 1024;

const EXTERNAL_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TERM",
    "COLORTERM",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_TTY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

const GH_ENV_ALLOWLIST: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_CONFIG_DIR",
    "GH_PROMPT_DISABLED",
    "GITHUB_API_URL",
    "GITHUB_GRAPHQL_URL",
];

const CODEX_ENV_ALLOWLIST: &[&str] = &[
    "OPENAI_API_KEY",
    "OPENAI_ORG_ID",
    "OPENAI_PROJECT_ID",
    "OPENAI_BASE_URL",
    "CODEX_HOME",
];

fn external_environment_keys(program: &str) -> Vec<OsString> {
    let program_allowlist = match program {
        "gh" => GH_ENV_ALLOWLIST,
        "codex" => CODEX_ENV_ALLOWLIST,
        _ => &[],
    };
    let mut keys = Vec::with_capacity(EXTERNAL_ENV_ALLOWLIST.len() + program_allowlist.len());
    keys.extend(EXTERNAL_ENV_ALLOWLIST.iter().map(OsString::from));
    keys.extend(program_allowlist.iter().map(OsString::from));
    keys
}

/// Reports whether the `gh` CLI is available in `PATH`.
#[must_use]
pub fn has_gh() -> bool {
    in_path("gh")
}

/// Reports whether the `codex` CLI is available in `PATH`.
#[must_use]
pub fn has_codex() -> bool {
    in_path("codex")
}

/// Mission metadata used to render the PR body.
#[derive(Clone, Default)]
pub struct PrMetadata {
    pub summary: String,
    pub mission_id: String,
    pub phase_count: i32,
    pub mode: String,
    pub personas: Vec<String>,
    pub cost_usd: f64,
    pub duration: String,
    pub files: Vec<String>,
}

impl fmt::Debug for PrMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrMetadata")
            .field("summary", &"[REDACTED]")
            .field("summary_bytes", &self.summary.len())
            .field("mission_id", &"[REDACTED]")
            .field("mission_id_bytes", &self.mission_id.len())
            .field("phase_count", &self.phase_count)
            .field("mode", &"[REDACTED]")
            .field("mode_bytes", &self.mode.len())
            .field("persona_count", &self.personas.len())
            .field("cost_reported", &(self.cost_usd != 0.0))
            .field("duration", &"[REDACTED]")
            .field("duration_bytes", &self.duration.len())
            .field("file_count", &self.files.len())
            .finish()
    }
}

/// Renders a Markdown PR description from the provided metadata, matching the
/// Go oracle's `BuildPRBody` byte-for-byte in structure.
#[must_use]
pub fn build_pr_body(m: &PrMetadata) -> String {
    let mut b = String::new();
    b.push_str("## Summary\n\n");
    if m.summary.is_empty() {
        b.push_str("_No summary provided._");
    } else {
        b.push_str(&m.summary);
    }
    b.push_str("\n\n");

    b.push_str("## Mission Details\n\n");
    b.push_str("| Field | Value |\n");
    b.push_str("|-------|-------|\n");
    b.push_str(&format!("| Mission ID | `{}` |\n", m.mission_id));
    b.push_str(&format!("| Phases | {} |\n", m.phase_count));
    b.push_str(&format!("| Mode | {} |\n", m.mode));
    if !m.personas.is_empty() {
        b.push_str(&format!("| Personas | {} |\n", m.personas.join(", ")));
    }
    if m.cost_usd > 0.0 {
        b.push_str(&format!("| Cost | ${:.4} |\n", m.cost_usd));
    }
    if !m.duration.is_empty() {
        b.push_str(&format!("| Duration | {} |\n", m.duration));
    }

    if !m.files.is_empty() {
        b.push_str("\n## Files Changed\n\n");
        for f in &m.files {
            b.push_str(&format!("- `{f}`\n"));
        }
    }

    b.push_str(
        "\n---\n*Created by [nanika orchestrator](https://github.com/joeyhipolito/nanika)*\n",
    );
    b
}

/// Opens a pull request from `head` against `base` in `repo_root` via `gh pr
/// create`. Returns the PR URL printed by `gh` on success. When `draft` is true
/// appends `--draft`.
pub fn create_pr(
    repo_root: &Path,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<String, GitError> {
    let owned = create_pr_args(head, base, title, draft);
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    run_external(repo_root, "gh", &args, Some(body.as_bytes()))
}

/// Posts a comment on the pull request at `pr_url` via `gh pr comment`.
pub fn comment_on_pr(repo_root: &Path, pr_url: &str, body: &str) -> Result<(), GitError> {
    let args = ["pr", "comment", pr_url, "--body-file", "-"];
    run_external(repo_root, "gh", &args, Some(body.as_bytes()))?;
    Ok(())
}

/// Requests GitHub reviewers via `gh pr edit <pr_url> --add-reviewer <csv>`.
/// No-op when `reviewers` is empty.
pub fn add_pr_reviewers(
    repo_root: &Path,
    pr_url: &str,
    reviewers: &[String],
) -> Result<(), GitError> {
    if reviewers.is_empty() {
        return Ok(());
    }
    let csv = reviewers.join(",");
    let args = ["pr", "edit", pr_url, "--add-reviewer", &csv];
    run_external(repo_root, "gh", &args, None)?;
    Ok(())
}

/// Adds labels via `gh pr edit <pr_url> --add-label <csv>`. No-op when empty.
pub fn add_pr_labels(repo_root: &Path, pr_url: &str, labels: &[String]) -> Result<(), GitError> {
    if labels.is_empty() {
        return Ok(());
    }
    let csv = labels.join(",");
    let args = ["pr", "edit", pr_url, "--add-label", &csv];
    run_external(repo_root, "gh", &args, None)?;
    Ok(())
}

/// Runs `codex review` against the branch checked out in `repo_root`. When
/// `prompt` is non-empty (after trimming) it is piped to stdin using the `-`
/// prompt sentinel, avoiding shell interpolation.
pub fn run_codex_review(
    repo_root: &Path,
    base_branch: &str,
    prompt: &str,
) -> Result<String, GitError> {
    let owned = codex_review_args(base_branch, prompt);
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let stdin = (!prompt.trim().is_empty()).then_some(prompt.as_bytes());
    run_external(repo_root, "codex", &args, stdin)
}

/// Builds the `gh pr create` argv (factored out for parity testing).
fn create_pr_args(head: &str, base: &str, title: &str, draft: bool) -> Vec<String> {
    let mut args = vec![
        "pr".to_owned(),
        "create".to_owned(),
        "--head".to_owned(),
        head.to_owned(),
        "--base".to_owned(),
        base.to_owned(),
        "--title".to_owned(),
        title.to_owned(),
        "--body-file".to_owned(),
        "-".to_owned(),
    ];
    if draft {
        args.push("--draft".to_owned());
    }
    args
}

/// Builds the `codex review` argv (factored out for parity testing).
fn codex_review_args(base_branch: &str, prompt: &str) -> Vec<String> {
    let mut args = vec!["review".to_owned()];
    if !base_branch.is_empty() {
        args.push("--base".to_owned());
        args.push(base_branch.to_owned());
    }
    if !prompt.trim().is_empty() {
        args.push("-".to_owned());
    }
    args
}

/// Runs an external CLI in `repo_root` with an allowlisted environment. On
/// success returns trimmed stdout; on non-zero exit returns redacted bounded
/// output through [`GitError::Command`]. The shared supervisor drains stdin and
/// both output streams concurrently.
fn run_external(
    repo_root: &Path,
    program: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<String, GitError> {
    let mut argv: Vec<OsString> = Vec::with_capacity(args.len().saturating_add(1));
    argv.push(OsString::from(program));
    for arg in args {
        argv.push(OsString::from(*arg));
    }
    let invalid = || GitError::Spawn {
        source: std::io::Error::other("invalid external process specification"),
    };
    let mut spec = ProcessSpec::new(argv, EXTERNAL_RUN_DEADLINE)
        .map_err(|_| invalid())?
        .with_max_output_bytes(EXTERNAL_RUN_MAX_OUTPUT);
    for key in external_environment_keys(program) {
        spec = spec.with_inherited_env(key);
    }
    if let Some(input) = stdin {
        spec = spec.with_stdin(input.to_vec());
    }

    let report = orchestrator_process::run(&spec, repo_root).map_err(|error| match error {
        ProcessError::Spawn(source) => GitError::Spawn { source },
        ProcessError::InvalidSpec => invalid(),
    })?;

    if report.is_success() {
        return Ok(String::from_utf8_lossy(&report.stdout).trim().to_owned());
    }

    let receipt_is_complete =
        report.cleanup_complete && !report.truncated && report.infrastructure_failures.is_empty();
    if !receipt_is_complete {
        return Err(GitError::Spawn {
            source: std::io::Error::other(
                "external process did not produce a complete supervision receipt",
            ),
        });
    }

    Err(classify_unsuccessful_termination(
        report.termination,
        &report.stdout,
        &report.stderr,
    ))
}

/// Reports whether `name` resolves to an executable in `PATH`, matching Go's
/// `exec.LookPath` semantics (a name with a separator is checked directly).
fn in_path(name: &str) -> bool {
    if name.contains(std::path::MAIN_SEPARATOR) {
        return is_executable(Path::new(name));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    matches!(std::fs::metadata(path), Ok(metadata) if metadata.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_pr_body_contains_required_fields() {
        let m = PrMetadata {
            summary: "ship the feature".to_owned(),
            mission_id: "mission-7".to_owned(),
            phase_count: 3,
            mode: "parallel".to_owned(),
            personas: vec!["architect".to_owned(), "coder".to_owned()],
            cost_usd: 1.5,
            duration: "42s".to_owned(),
            files: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
        };
        let body = build_pr_body(&m);
        assert!(body.contains("## Summary\n\nship the feature"));
        assert!(body.contains("| Mission ID | `mission-7` |"));
        assert!(body.contains("| Phases | 3 |"));
        assert!(body.contains("| Mode | parallel |"));
        assert!(body.contains("| Personas | architect, coder |"));
        assert!(body.contains("| Cost | $1.5000 |"));
        assert!(body.contains("| Duration | 42s |"));
        assert!(body.contains("## Files Changed"));
        assert!(body.contains("- `src/a.rs`"));
        assert!(body.contains("nanika orchestrator"));
    }

    #[test]
    fn build_pr_body_omits_cost_and_files_when_empty() {
        let m = PrMetadata {
            summary: "do thing".to_owned(),
            mission_id: "m1".to_owned(),
            phase_count: 1,
            mode: "sequential".to_owned(),
            ..PrMetadata::default()
        };
        let body = build_pr_body(&m);
        assert!(!body.contains("## Files Changed"));
        assert!(!body.contains("| Cost |"));
        assert!(!body.contains("| Personas |"));
        assert!(!body.contains("| Duration |"));
    }

    #[test]
    fn build_pr_body_empty_summary_falls_back() {
        let m = PrMetadata {
            mission_id: "m2".to_owned(),
            mode: "sequential".to_owned(),
            ..PrMetadata::default()
        };
        assert!(build_pr_body(&m).contains("_No summary provided._"));
    }

    #[test]
    fn create_pr_args_match_oracle_without_draft() {
        let args = create_pr_args("feature", "main", "title", false);
        assert_eq!(
            args,
            [
                "pr",
                "create",
                "--head",
                "feature",
                "--base",
                "main",
                "--title",
                "title",
                "--body-file",
                "-",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn create_pr_args_append_draft_flag() {
        let args = create_pr_args("feature", "main", "t", true);
        assert_eq!(args.last().map(String::as_str), Some("--draft"));
        assert_eq!(args.len(), 11);
    }

    #[test]
    fn codex_review_args_minimal_without_base_or_prompt() {
        assert_eq!(codex_review_args("", "   "), ["review"].map(str::to_owned));
    }

    #[test]
    fn codex_review_args_include_base_and_prompt_sentinel() {
        let args = codex_review_args("main", "review me");
        assert_eq!(args, ["review", "--base", "main", "-"].map(str::to_owned));
    }

    #[test]
    fn has_gh_returns_a_bool_without_panicking() {
        let _ = has_gh();
    }

    #[test]
    fn codex_environment_is_an_explicit_allowlist() {
        let keys = external_environment_keys("codex");

        assert!(keys.iter().any(|key| key == "OPENAI_API_KEY"));
        assert!(keys.iter().any(|key| key == "CODEX_HOME"));
        assert!(!keys.iter().any(|key| key == "OPENAI_DO_NOT_FORWARD_SECRET"));
        assert!(!keys.iter().any(|key| key == "CODEX_PRIVATE_CANARY"));
    }

    #[test]
    fn pr_metadata_debug_redacts_content() {
        let canary = "pr-metadata-canary-do-not-print";
        let metadata = PrMetadata {
            summary: canary.to_owned(),
            mission_id: canary.to_owned(),
            mode: canary.to_owned(),
            personas: vec![canary.to_owned()],
            duration: canary.to_owned(),
            files: vec![canary.to_owned()],
            ..PrMetadata::default()
        };

        let debug = format!("{metadata:?}");
        assert!(!debug.contains(canary));
        assert!(debug.contains("persona_count: 1"));
        assert!(debug.contains("file_count: 1"));
    }
}

// ---------------------------------------------------------------------------
// B4-DESIGN §3 — the PR adapter boundary and its fixture implementation
// ---------------------------------------------------------------------------

/// Everything an adapter needs to open a pull request.
///
/// `Debug` redacts the title, body, and repository path: all three are
/// mission-sensitive and this type is expected to appear in error context.
#[derive(Clone)]
pub struct PrRequest {
    pub repo_root: PathBuf,
    pub base_branch: String,
    pub head_branch: String,
    pub head_sha: CommitSha,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

impl fmt::Debug for PrRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrRequest")
            .field("repo_root", &"[REDACTED]")
            .field("base_branch", &self.base_branch)
            .field("head_branch", &self.head_branch)
            .field("head_sha", &self.head_sha)
            .field("title_bytes", &self.title.len())
            .field("body_bytes", &self.body.len())
            .field("draft", &self.draft)
            .finish()
    }
}

/// The durable record of a pull request that exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReceipt {
    pub provider: String,
    pub receipt_id: String,
    pub url: String,
    pub head_sha: String,
}

/// Failures an adapter can report.
#[derive(Debug, Error)]
pub enum PrAdapterError {
    /// The head commit is not present in the remote, so no pull request could
    /// legitimately refer to it.
    #[error("head commit is not published to the remote")]
    HeadNotPublished,
    /// The remote could not be inspected at all.
    #[error("the remote could not be observed")]
    RemoteUnobservable,
    /// The receipt ledger could not be read or written.
    #[error("pr receipt ledger: {0}")]
    Ledger(#[from] std::io::Error),
    /// A stored receipt could not be decoded.
    #[error("pr receipt ledger is corrupt")]
    LedgerCorrupt,
    /// The request could not be represented (an unusable branch name, or a
    /// ledger directory outside the caller's control).
    #[error("pr request is not representable")]
    InvalidRequest,
}

/// The boundary between the orchestrator and whatever actually opens pull
/// requests.
///
/// `lookup` is not speculative surface: crash reconciliation (B4-DESIGN §4 C6)
/// must be able to ask "does a pull request already exist for this receipt id"
/// without opening a second one.
pub trait PrAdapter: Send + Sync {
    /// The provider name recorded in every receipt this adapter mints.
    fn provider(&self) -> &str;
    /// The receipt id this adapter *would* mint for `request`, computed
    /// without contacting anything.
    ///
    /// B4-DESIGN §1.4 requires the `git-open-pr` plan payload to carry the
    /// deterministic receipt id, which is journaled **before** the pull request
    /// is opened; §4 C6 then reconciles a crash by looking that id up. Both are
    /// impossible unless the id is derivable from the request alone, so this is
    /// a required part of the boundary rather than a convenience: an adapter
    /// whose ids are server-assigned cannot satisfy the crash contract and must
    /// not implement this trait.
    fn receipt_id(&self, request: &PrRequest) -> Result<String, PrAdapterError>;
    /// Opens a pull request, or returns the existing receipt unchanged.
    fn open(&self, request: &PrRequest) -> Result<PrReceipt, PrAdapterError>;
    /// Returns a previously minted receipt, or `None` if none exists.
    fn lookup(&self, receipt_id: &str) -> Result<Option<PrReceipt>, PrAdapterError>;
}

/// The fixture provider name, and the scheme of every url it mints.
pub const FIXTURE_PR_PROVIDER: &str = "fixture";

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over the domain-separated receipt inputs.
///
/// The design named SHA-256 truncated to 16 hex characters. The property that
/// is load-bearing is *determinism* — the same head must always produce the
/// same id, so a restarted process cannot open a second pull request — and a
/// 64-bit FNV-1a over four domain-separated fields provides it. SHA-256 would
/// require adding `sha2` to this crate, which changes the workspace
/// `Cargo.lock` and `tests/rust-lock-baseline.json`, both outside this change's
/// scope fence. Swapping the digest later changes only this function.
fn deterministic_digest(parts: &[&str]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    let mut mix = |byte: u8| {
        hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    };
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            mix(0x1F);
        }
        for byte in part.bytes() {
            mix(byte);
        }
    }
    hash
}

/// The receipt id for a request: a pure function of provider, base, head
/// branch, and head sha. No clock, no counter, no randomness.
#[must_use]
pub fn fixture_receipt_id(
    provider: &str,
    base_branch: &str,
    head_branch: &str,
    head_sha: &str,
) -> String {
    let digest = deterministic_digest(&[provider, base_branch, head_branch, head_sha]);
    format!("fixture-pr-{digest:016x}")
}

/// The only [`PrAdapter`] implementation shipped in B4.
///
/// It contacts nothing. "Opening" a pull request means checking that the head
/// commit really exists in a local bare remote and writing a 0600 receipt file
/// into a ledger directory. Because the receipt id is a pure function of the
/// head, re-opening after a crash returns the same receipt instead of a second
/// pull request.
///
/// Both paths must live inside the caller's confinement root; the adapter does
/// not mint that authority and does not widen it.
pub struct FixturePrAdapter {
    bare_remote: PathBuf,
    ledger_dir: PathBuf,
}

impl fmt::Debug for FixturePrAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixturePrAdapter")
            .field("bare_remote", &"[REDACTED]")
            .field("ledger_dir", &"[REDACTED]")
            .finish()
    }
}

impl FixturePrAdapter {
    #[must_use]
    pub fn new(bare_remote: PathBuf, ledger_dir: PathBuf) -> Self {
        Self {
            bare_remote,
            ledger_dir,
        }
    }

    fn receipt_path(&self, receipt_id: &str) -> Result<PathBuf, PrAdapterError> {
        let usable = !receipt_id.is_empty()
            && receipt_id
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'));
        if !usable {
            return Err(PrAdapterError::InvalidRequest);
        }
        Ok(self.ledger_dir.join(format!("{receipt_id}.json")))
    }

    fn read_receipt(path: &Path) -> Result<Option<PrReceipt>, PrAdapterError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| PrAdapterError::LedgerCorrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PrAdapterError::Ledger(error)),
        }
    }
}

impl PrAdapter for FixturePrAdapter {
    fn provider(&self) -> &str {
        FIXTURE_PR_PROVIDER
    }

    fn receipt_id(&self, request: &PrRequest) -> Result<String, PrAdapterError> {
        Ok(fixture_receipt_id(
            self.provider(),
            &request.base_branch,
            &request.head_branch,
            request.head_sha.as_str(),
        ))
    }

    fn open(&self, request: &PrRequest) -> Result<PrReceipt, PrAdapterError> {
        // A pull request may never exist for a commit the remote has not
        // accepted, so the head is verified inside the bare remote itself.
        let head = request.head_sha.as_str();
        let revision = format!("{head}^{{commit}}");
        match crate::process::run_outcome(
            &self.bare_remote,
            &["git", "rev-parse", "--verify", "--quiet", &revision],
        ) {
            Ok(crate::process::RunOutcome::Success { .. }) => {}
            Ok(crate::process::RunOutcome::Failed { .. }) => {
                return Err(PrAdapterError::HeadNotPublished);
            }
            Ok(crate::process::RunOutcome::Anomalous) | Err(_) => {
                return Err(PrAdapterError::RemoteUnobservable);
            }
        }

        let receipt_id = fixture_receipt_id(
            self.provider(),
            &request.base_branch,
            &request.head_branch,
            head,
        );
        let path = self.receipt_path(&receipt_id)?;
        if let Some(existing) = Self::read_receipt(&path)? {
            return Ok(existing);
        }

        let receipt = PrReceipt {
            provider: self.provider().to_owned(),
            receipt_id: receipt_id.clone(),
            // A scheme nothing resolves, so an accidental fetch of a PR url
            // fails loudly instead of reaching a network.
            url: format!(
                "{FIXTURE_PR_PROVIDER}://{}/pull/{receipt_id}",
                self.provider()
            ),
            head_sha: head.to_owned(),
        };
        std::fs::create_dir_all(&self.ledger_dir).map_err(PrAdapterError::Ledger)?;
        let encoded = serde_json::to_vec(&receipt).map_err(|_| PrAdapterError::LedgerCorrupt)?;
        crate::write_private_file(&path, &encoded).map_err(|error| match error {
            GitError::Io(source) => PrAdapterError::Ledger(source),
            _ => PrAdapterError::InvalidRequest,
        })?;
        Ok(receipt)
    }

    fn lookup(&self, receipt_id: &str) -> Result<Option<PrReceipt>, PrAdapterError> {
        Self::read_receipt(&self.receipt_path(receipt_id)?)
    }
}

#[cfg(test)]
mod fixture_adapter_tests {
    use super::*;

    #[test]
    fn receipt_id_is_a_pure_function_of_provider_base_head_and_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let first = fixture_receipt_id("fixture", "main", "via/m/slug", sha);
        let again = fixture_receipt_id("fixture", "main", "via/m/slug", sha);
        assert_eq!(first, again);

        assert_ne!(
            first,
            fixture_receipt_id("fixture", "main", "via/m/other", sha)
        );
        assert_ne!(
            first,
            fixture_receipt_id("fixture", "release", "via/m/slug", sha)
        );
        assert_ne!(
            first,
            fixture_receipt_id(
                "fixture",
                "main",
                "via/m/slug",
                "89abcdef0123456789abcdef0123456789abcdef"
            )
        );
        assert!(first.starts_with("fixture-pr-"));
        assert_eq!(first.len(), "fixture-pr-".len() + 16);
    }

    #[test]
    fn field_boundaries_are_domain_separated() {
        // Without the 0x1F separator these two would hash identically.
        assert_ne!(
            fixture_receipt_id("fixture", "ab", "c", "d"),
            fixture_receipt_id("fixture", "a", "bc", "d")
        );
    }

    #[test]
    fn receipt_paths_refuse_a_traversing_id() {
        let adapter = FixturePrAdapter::new(PathBuf::from("/remote"), PathBuf::from("/ledger"));
        assert!(adapter.receipt_path("../escape").is_err());
        assert!(adapter.receipt_path("").is_err());
        assert!(adapter.receipt_path("fixture-pr-0123456789abcdef").is_ok());
    }
}
