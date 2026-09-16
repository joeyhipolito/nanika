//! The env-sanitized git command runner.
//!
//! This intentionally differs from the Go oracle's inherit-all-except-three
//! environment behavior: Rust clears the child environment and copies an
//! audited compatibility allowlist from the trusted operator environment.
//! Common Git configuration, locale, authentication, and repository-override
//! behavior is retained, but unknown ambient variables are refused. Inline
//! `GIT_CONFIG_COUNT` entries are bounded and fail closed instead of being
//! partially forwarded. This hardening boundary is not a claim that the Git
//! parity contract is complete. Commands run through the shared
//! [`orchestrator_process`] supervisor so a hung or chatty child cannot block or
//! exhaust the orchestrator. `GIT_DIR`, `GIT_WORK_TREE`, and `GIT_INDEX_FILE`
//! remain denied so linked-worktree discovery cannot be redirected.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use orchestrator_process::{ProcessError, ProcessReport, ProcessSpec, ProcessTermination};

use crate::{CapturedCommandOutput, GitError};

/// Hard wall-clock deadline for a single git invocation. Git commands are fast;
/// this catches genuine hangs (credential prompts, hung pack processes) without
/// disrupting normal operation.
const GIT_RUN_DEADLINE: Duration = Duration::from_secs(60);
/// Per-stream output cap. Generous for any normal git command while bounding
/// memory against runaway output.
const GIT_RUN_MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// `ProcessSpec` admits at most 64 environment operations. Eight inline
/// configuration entries preserve normal `git -c`-style environment use while
/// leaving room for the explicit compatibility allowlist and three denied
/// repository-override variables. Larger counts fail closed before spawn rather
/// than silently dropping only part of the caller's configuration.
const MAX_INLINE_GIT_CONFIG_ENTRIES: usize = 8;

/// Compatibility environment for normal Git configuration, authentication,
/// SSH, proxies, and commit identity.
///
/// This is a deliberate security deviation from Go's ambient inheritance, not
/// isolation from an untrusted parent environment. Values are operator-trusted:
/// entries such as `GIT_SSH_COMMAND`, askpass hooks, and
/// `GIT_CONFIG_PARAMETERS` can select executable helpers or Git configuration
/// with credential-helper authority. The child environment remains clear by
/// default, and arbitrary `GIT_*` variables are not inherited as a
/// defense-in-depth measure.
const GIT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TERM",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "GPG_TTY",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_TERMINAL_PROMPT",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_ATTR_NOSYSTEM",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
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

fn parse_inline_git_config_count(value: Option<&OsStr>) -> Result<Option<usize>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let count = value.to_str().ok_or(())?.parse::<usize>().map_err(|_| ())?;
    if count > MAX_INLINE_GIT_CONFIG_ENTRIES {
        return Err(());
    }
    Ok(Some(count))
}

fn git_environment_keys(inline_config_count: Option<usize>) -> Vec<OsString> {
    let extra = inline_config_count.map_or(0, |count| count.saturating_mul(2).saturating_add(1));
    let mut keys = Vec::with_capacity(GIT_ENV_ALLOWLIST.len().saturating_add(extra));
    keys.extend(GIT_ENV_ALLOWLIST.iter().map(OsString::from));
    if let Some(count) = inline_config_count {
        keys.push(OsString::from("GIT_CONFIG_COUNT"));
        for index in 0..count {
            keys.push(OsString::from(format!("GIT_CONFIG_KEY_{index}")));
            keys.push(OsString::from(format!("GIT_CONFIG_VALUE_{index}")));
        }
    }
    keys
}

fn invalid_inline_git_config() -> GitError {
    GitError::Spawn {
        source: std::io::Error::other(
            "inline Git configuration is malformed or exceeds the bounded compatibility limit",
        ),
    }
}

pub(crate) fn classify_unsuccessful_termination(
    termination: ProcessTermination,
    stdout: &[u8],
    stderr: &[u8],
) -> GitError {
    match termination {
        ProcessTermination::Exited(_) => {
            let stdout = String::from_utf8_lossy(stdout);
            let stderr = String::from_utf8_lossy(stderr);
            GitError::Command {
                output: CapturedCommandOutput::new(format!("{stdout}{stderr}")),
            }
        }
        ProcessTermination::Signaled(_) => GitError::Spawn {
            source: std::io::Error::other("supervised command was terminated by a signal"),
        },
        ProcessTermination::Timeout => GitError::Spawn {
            source: std::io::Error::other(
                "supervised command exceeded its deadline and was killed",
            ),
        },
        ProcessTermination::Stalled => GitError::Spawn {
            source: std::io::Error::other("supervised command stalled and was killed"),
        },
        ProcessTermination::Cancelled => GitError::Spawn {
            source: std::io::Error::other("supervised command was cancelled"),
        },
        ProcessTermination::OutputLimit
        | ProcessTermination::InfrastructureError
        | ProcessTermination::UnresolvedOwnership => GitError::Spawn {
            source: std::io::Error::other("supervised command process ownership failed"),
        },
    }
}

// ---------------------------------------------------------------------------
// B4-DESIGN §1.4 step 4 — the identity of the git child that ran
// ---------------------------------------------------------------------------

/// The operating-system facts one supervised git child left behind.
///
/// **This is deliberately not a [`orchestrator_process::KernelProcessIdentity`].**
/// That type is minted by `observe`, which requires the process to still be
/// live and re-reads the kernel's own start record; the ambient
/// [`orchestrator_process::run`] used here reaps the child before returning, so
/// no live observation is possible and `ProcessReport::kernel_identity` is
/// always `None` on this path (it is populated only by the gated
/// production-launcher lane). Fabricating a kernel start identity that was
/// never read would be worse than recording what was actually observed, so
/// this type carries the two identifiers the supervisor really reported plus a
/// per-process monotonic sequence that distinguishes two children even when the
/// operating system recycles a PID inside one orchestrator run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChildIdentity {
    pid: u32,
    process_group_id: u32,
    start_identity: String,
}

impl GitChildIdentity {
    /// The direct child's PID, as reported by the supervisor.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The dedicated process-group ID the supervisor owned the child under.
    #[must_use]
    pub const fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    /// A per-process monotonic marker distinguishing this child from any other
    /// this process supervised, including one that reused its PID. It is not a
    /// kernel start identity and never claims to be.
    #[must_use]
    pub fn start_identity(&self) -> &str {
        &self.start_identity
    }
}

/// Monotonic across the whole process so two threads cannot mint the same
/// marker for two different children.
static CHILD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The most recent git child *this thread* supervised.
    ///
    /// Thread-local rather than global on purpose: the caller reads it
    /// immediately after the library call it made, so a concurrent git command
    /// on another thread can never be mistaken for its own child.
    static LAST_SUPERVISED_CHILD: RefCell<Option<GitChildIdentity>> =
        const { RefCell::new(None) };
}

/// Records the identity of a child that really spawned. A report with no PID or
/// process group never spawned, so nothing is recorded and the previous value
/// is left alone — [`last_supervised_child_identity`] callers must therefore
/// treat a stale value as possible and only read it immediately after a call
/// they know spawned.
fn record_supervised_child(report: &ProcessReport) {
    let (Some(pid), Some(process_group_id)) = (report.pid, report.pgid) else {
        return;
    };
    if !report.spawned || pid == 0 || process_group_id == 0 {
        return;
    }
    let sequence = CHILD_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let identity = GitChildIdentity {
        pid,
        process_group_id,
        start_identity: format!("git-child-{sequence:016x}"),
    };
    LAST_SUPERVISED_CHILD.with(|cell| {
        *cell.borrow_mut() = Some(identity);
    });
}

/// The identity of the most recent git child this thread supervised, if any.
///
/// B4-DESIGN §1.4 step 4 records an execution identity against the outbox row
/// for a git effect. Because a git effect may be several commands (`fetch`,
/// then `rev-parse`), the identity recorded is that of the last child the
/// effect's execution spawned — the one whose exit the caller observed.
#[must_use]
pub fn last_supervised_child_identity() -> Option<GitChildIdentity> {
    LAST_SUPERVISED_CHILD.with(|cell| cell.borrow().clone())
}

/// Forgets any previously observed child, so a caller can prove that the call
/// it is about to make either spawned a child or produced no identity at all.
pub fn forget_supervised_child_identity() {
    LAST_SUPERVISED_CHILD.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// How a supervised git invocation ended, retaining the distinction between a
/// normal non-zero exit and every other termination.
///
/// [`run`] collapses both into [`GitError`], which is right for callers that
/// only need success or failure. Push acknowledgement (B4-DESIGN §4.3) must not
/// collapse them: a rejected push is a decided outcome, whereas a signalled,
/// timed-out, or stalled push leaves the remote state unknown and may only be
/// classified as ambiguous.
pub(crate) enum RunOutcome {
    /// The child exited zero. Carries stdout.
    Success { stdout: String },
    /// The child exited non-zero. Carries bounded, redacted stdout+stderr.
    Failed { output: CapturedCommandOutput },
    /// Signal, deadline, stall, cancellation, or an incomplete supervision
    /// receipt. Nothing may be concluded about what the child did.
    Anomalous,
}

/// Runs `args[0]` with `args[1..]` in `dir`, returning stdout on success.
///
/// On non-zero exit returns [`GitError::Command`] with bounded, redacted output
/// that crate-internal compatibility logic may inspect explicitly (for example,
/// Git's "nothing to commit" result).
pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    match run_outcome(dir, args)? {
        RunOutcome::Success { stdout } => Ok(stdout),
        RunOutcome::Failed { output } => Err(GitError::Command { output }),
        RunOutcome::Anomalous => Err(GitError::Spawn {
            source: std::io::Error::other(
                "git process did not exit normally or produced an incomplete supervision receipt",
            ),
        }),
    }
}

/// Runs a git command and classifies how it ended.
///
/// `Err` is reserved for failures that happened *before* the child could act —
/// an invalid specification or a spawn failure — so a caller may safely treat
/// every `Ok` variant as a statement about a child that really ran.
pub(crate) fn run_outcome(dir: &Path, args: &[&str]) -> Result<RunOutcome, GitError> {
    let argv: Vec<OsString> = args.iter().map(|arg| OsString::from(*arg)).collect();
    let mut spec = ProcessSpec::new(argv, GIT_RUN_DEADLINE)
        .map_err(|_| GitError::Spawn {
            source: std::io::Error::other("invalid git process specification"),
        })?
        .with_max_output_bytes(GIT_RUN_MAX_OUTPUT)
        .with_env_remove("GIT_DIR")
        .with_env_remove("GIT_WORK_TREE")
        .with_env_remove("GIT_INDEX_FILE");
    let inline_config_count =
        parse_inline_git_config_count(std::env::var_os("GIT_CONFIG_COUNT").as_deref())
            .map_err(|()| invalid_inline_git_config())?;
    for key in git_environment_keys(inline_config_count) {
        spec = spec.with_inherited_env(key);
    }

    let report = orchestrator_process::run(&spec, dir).map_err(|error| match error {
        ProcessError::Spawn(source) => GitError::Spawn { source },
        ProcessError::InvalidSpec => GitError::Spawn {
            source: std::io::Error::other("invalid git process specification"),
        },
    })?;
    record_supervised_child(&report);

    if report.is_success() {
        return Ok(RunOutcome::Success {
            stdout: String::from_utf8_lossy(&report.stdout).into_owned(),
        });
    }

    let receipt_is_complete =
        report.cleanup_complete && !report.truncated && report.infrastructure_failures.is_empty();
    if !receipt_is_complete {
        return Ok(RunOutcome::Anomalous);
    }

    Ok(
        match classify_unsuccessful_termination(report.termination, &report.stdout, &report.stderr)
        {
            GitError::Command { output } => RunOutcome::Failed { output },
            _ => RunOutcome::Anomalous,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_environment_preserves_config_and_message_locale_within_bound() {
        let keys = git_environment_keys(Some(2));

        for expected in [
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_KEY_1",
            "GIT_CONFIG_VALUE_1",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "LC_MESSAGES",
        ] {
            assert!(keys.iter().any(|key| key == expected), "missing {expected}");
        }

        assert!(!keys.iter().any(|key| key == "GIT_UNREVIEWED_CANARY"));
    }

    #[test]
    fn inline_git_config_limit_fits_process_environment_contract() {
        let keys = git_environment_keys(Some(MAX_INLINE_GIT_CONFIG_ENTRIES));
        let denied_repository_overrides = 3;

        assert_eq!(keys.len() + denied_repository_overrides, 64);
        assert!(parse_inline_git_config_count(Some(OsStr::new("8"))).is_ok());
        assert!(parse_inline_git_config_count(Some(OsStr::new("9"))).is_err());
        assert!(parse_inline_git_config_count(Some(OsStr::new("invalid"))).is_err());
    }

    #[test]
    fn only_normal_exit_is_a_command_failure() {
        let exited = classify_unsuccessful_termination(
            ProcessTermination::Exited(1),
            b"nothing to commit",
            b"",
        );
        let signaled = classify_unsuccessful_termination(
            ProcessTermination::Signaled(15),
            b"nothing to commit",
            b"",
        );
        let stalled = classify_unsuccessful_termination(ProcessTermination::Stalled, b"", b"");

        assert!(matches!(exited, GitError::Command { .. }));
        assert!(matches!(signaled, GitError::Spawn { .. }));
        assert!(matches!(stalled, GitError::Spawn { .. }));
    }
}
