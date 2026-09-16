//! B4-DESIGN §6.2 — the governed-Git run seam, end to end.
//!
//! One fixture mission drives one local helper phase against a fixture
//! repository whose only remote is a local bare repository the test creates.
//! Every case proves a property of the governed path rather than of git:
//! the base is an exact sha recorded before any branch exists, the isolation is
//! created exactly once, unverified work is never committed, an acknowledgement
//! comes from `ls-remote` and not from an exit code, the pull-request receipt is
//! deterministic and offline, each of B4-DESIGN §4's six crash points resumes
//! with zero duplicates, and cleanup cannot escape the fixture root.
//!
//! Nothing here touches a repository outside its own fixture root, reaches a
//! network, or spawns `gh`. The last two are not assertions of intent: the
//! remote is a bare filesystem path, and `no_network_and_no_pull_request_provider_is_contacted`
//! re-enters this binary with a `PATH` whose `git` records every argv and whose
//! `gh` writes a marker, then fails if the marker exists or if any recorded
//! argv names a network location.

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator_app::{
    GitBarrierDecision, GitEffectBarrier, GitEffectCapability, GitEffectError, GitEffectLedger,
    GitEffectService, GitIntent, GitReceipt, IsolatedFixtureRoot, ReconciliationDisposition,
    RollbackAction, SLOT_CLEANUP, SLOT_COMMIT, SLOT_CREATE_ISOLATION, SLOT_OPEN_PR, SLOT_PUSH,
    SLOT_REFRESH_BASE, VerifiedWork,
};
use orchestrator_core::{MissionId, PhaseId};
use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchRequest, EffectKind,
    EffectRequest, EffectServiceErrorKind, EventReceipt, EventSink, EventSinkError,
    EventSinkErrorKind, ExecutionContext, ExecutorRegistry, MechanicalTermination, PartialWork,
    PhaseExecutor, ProcessBudget, ProcessPreflight, ProcessReceipt, ProcessRequest, ProcessService,
    ProcessServiceError, ProcessServiceErrorKind, RuntimeDescriptor, WatchdogDecision,
    WatchdogPolicy, WorkerIdentity,
};
use orchestrator_git::{
    ClaimsDb, FixturePrAdapter, PrAdapter, PrAdapterError, PrReceipt, PrRequest, PushAck,
    open_claims_db,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "20260903-b4git";
const PHASE: &str = "implement";
const TASK: &str = "governed git run";
const REMOTE: &str = "origin";
const BASE_BRANCH: &str = "main";
const TRASH: &str = "trash";
/// `orchestrator_git::branch_name` builds `via/<mission>/<slug>` and the
/// service derives the worktree directory from the same two components.
const BRANCH: &str = "via/20260903-b4git/governed-git-run";
const WORKTREE_DIR: &str = "20260903-b4git-governed-git-run";

static CASE: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Fixture harness
// ---------------------------------------------------------------------------

/// One disposable fixture: a private parent directory holding an isolated
/// root, a bare remote, and a clone. Everything is removed on drop, so a case
/// that fails still leaves nothing behind.
struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
    capability: GitEffectCapability,
    /// The commit `origin/main` points at after setup.
    origin_head: String,
    /// The commit `work`'s own `refs/heads/main` points at — deliberately left
    /// behind `origin_head` so "the refreshed base won" is observable.
    stale_local_head: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Linked worktrees are mode-0700 directories the harness owns; a plain
        // recursive removal is enough because nothing here is read-only.
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-git-run-{}-{number}-{label}",
            std::process::id()
        ));
        private_dir(&parent)?;
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        let capability = GitEffectCapability::in_fixture(&root, &[REMOTE])?;

        let bare = root.path().join("origin.git");
        git(
            &parent,
            &["init", "--bare", "--initial-branch=main", str(&bare)],
        )?;

        // A publisher clone seeds the remote and then advances it, so the work
        // clone's own `main` is provably behind `origin/main` before the run
        // starts. That is the TRK-1116 condition.
        let publisher = root.path().join("publisher");
        git(&parent, &["clone", str(&bare), str(&publisher)])?;
        identify(&publisher)?;
        std::fs::write(publisher.join("README.md"), b"base\n")?;
        git(&publisher, &["add", "-A"])?;
        git(&publisher, &["commit", "-m", "base"])?;
        git(&publisher, &["push", "origin", "main"])?;

        let work = root.path().join("work");
        git(&parent, &["clone", str(&bare), str(&work)])?;
        identify(&work)?;
        let stale_local_head = rev_parse(&work, "HEAD")?;

        std::fs::write(publisher.join("SECOND.md"), b"advanced\n")?;
        git(&publisher, &["add", "-A"])?;
        git(&publisher, &["commit", "-m", "advance the remote"])?;
        git(&publisher, &["push", "origin", "main"])?;
        let origin_head = rev_parse(&publisher, "HEAD")?;
        assert_ne!(
            origin_head, stale_local_head,
            "the fixture must leave the work clone behind the remote"
        );

        // The remote is registered as a bare filesystem path, not a `file://`
        // URL, so no recorded argv can contain `://` at all.
        git(&work, &["remote", "set-url", REMOTE, str(&bare)])?;
        std::fs::create_dir_all(root.path().join("pr-ledger"))?;

        Ok(Self {
            parent,
            root,
            capability,
            origin_head,
            stale_local_head,
        })
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn repo(&self) -> PathBuf {
        self.root().join("work")
    }

    fn bare(&self) -> PathBuf {
        self.root().join("origin.git")
    }

    fn ledger(&self) -> PathBuf {
        self.root().join("pr-ledger")
    }

    fn worktree(&self) -> PathBuf {
        self.root().join("worktrees").join(WORKTREE_DIR)
    }

    fn adapter(&self) -> FixturePrAdapter {
        FixturePrAdapter::new(self.bare(), self.ledger())
    }

    /// Opens a fresh service over the same durable store.
    ///
    /// Dropping the previous service drops the store with it, so calling this
    /// twice models "the process died and a new one resumed" as faithfully as
    /// B4-DESIGN §6.2 specifies: the durable state is whatever the previous
    /// service left, and the new one has no in-memory continuity.
    fn service<'a>(&'a self, adapter: &'a FixturePrAdapter) -> TestResult<GitEffectService<'a>> {
        let ledger = GitEffectLedger::open(&self.capability)?;
        Ok(orchestrator_cli::governed_git_run(
            ledger,
            &self.capability,
            adapter,
            MissionId::new(MISSION)?,
            PhaseId::new(PHASE)?,
            self.repo(),
        )?)
    }

    /// Reads the durable outbox row for one slot straight out of the Git
    /// effect ledger's own database.
    ///
    /// A succeeded row is invisible to every public store read, so the
    /// assertions that must see a resolved plan payload — the recorded base
    /// sha, the claim count — go to the database.
    fn row(&self, slot: &str) -> TestResult<Option<OutboxRow>> {
        let connection = rusqlite::Connection::open_with_flags(
            self.root().join("git-ledger").join("runtime.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement = connection.prepare(
            "SELECT state, attempts, payload_json,
                    (SELECT count(*) FROM outbox_execution_identity AS identity
                      WHERE identity.idempotency_key = outbox.idempotency_key)
             FROM outbox WHERE operation_slot = ?1",
        )?;
        let mut rows = statement.query([slot])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let payload: String = row.get(2)?;
        Ok(Some(OutboxRow {
            state: row.get(0)?,
            attempts: row.get(1)?,
            payload: serde_json::from_str(&payload)?,
            identities: row.get(3)?,
        }))
    }

    fn claims(&self) -> TestResult<ClaimsDb> {
        Ok(open_claims_db(Some(&self.root().join("claims.db")))?)
    }
}

#[derive(Debug)]
struct OutboxRow {
    state: String,
    attempts: u32,
    payload: serde_json::Value,
    identities: u32,
}

impl OutboxRow {
    fn plan(&self, field: &str) -> String {
        self.payload
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn str(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
}

/// Runs a setup git command directly, never through the library, so a case
/// that records library argv sees only what the library itself ran.
fn git(dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new(real_git())
        .current_dir(dir)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The real git binary by absolute path.
///
/// The argv-audit case puts a shim `git` first on the child's `PATH`; fixture
/// setup must bypass it, otherwise the recorded log would be mostly setup.
fn real_git() -> PathBuf {
    for candidate in [
        "/usr/bin/git",
        "/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return path;
        }
    }
    PathBuf::from("git")
}

fn identify(repo: &Path) -> TestResult {
    git(repo, &["config", "user.email", "fixture@example.invalid"])?;
    git(repo, &["config", "user.name", "Fixture"])?;
    Ok(())
}

fn rev_parse(dir: &Path, revision: &str) -> TestResult<String> {
    git(dir, &["rev-parse", revision])
}

/// Every sha the bare remote holds for `branch`.
///
/// `show-ref` exits non-zero when the ref is simply absent, which is a valid
/// answer here rather than a failure, so the exit status is not consulted.
fn ls_remote(bare: &Path, branch: &str) -> TestResult<Vec<String>> {
    let output = Command::new(real_git())
        .current_dir(bare)
        .args(["show-ref", &format!("refs/heads/{branch}")])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect())
}

fn linked_worktrees(repo: &Path) -> TestResult<usize> {
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    // The main worktree is always listed first; every further entry is linked.
    Ok(listing
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
        .saturating_sub(1))
}

fn branches_named(repo: &Path, branch: &str) -> TestResult<usize> {
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    Ok(out.lines().filter(|line| !line.is_empty()).count())
}

fn ledger_entries(ledger: &Path) -> TestResult<usize> {
    Ok(std::fs::read_dir(ledger)?.count())
}

fn commits_on(repo: &Path, branch: &str) -> TestResult<usize> {
    let out = git(repo, &["rev-list", "--count", branch])?;
    Ok(out.trim().parse()?)
}

// ---------------------------------------------------------------------------
// The dispatch half: exactly the inert services `run_one_phase_e2e` builds
// ---------------------------------------------------------------------------

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct FixedWatchdog;

impl WatchdogPolicy for FixedWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

struct NoProcess {
    error: ProcessServiceError,
}

impl NoProcess {
    fn new() -> TestResult<Self> {
        Ok(Self {
            error: ProcessServiceError::new(
                ProcessServiceErrorKind::Unavailable,
                "the local helper phase spawns no process",
            )?,
        })
    }
}

impl Cancellation for NoProcess {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ProcessService for NoProcess {
    fn finish_preflight(
        &self,
        _request: &ProcessRequest,
        _preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        Ok(())
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        Err(self.error.clone())
    }
}

struct SilentSink {
    sequence: AtomicU64,
    error: EventSinkError,
}

impl SilentSink {
    fn new() -> TestResult<Self> {
        Ok(Self {
            sequence: AtomicU64::new(0),
            error: EventSinkError::new(
                EventSinkErrorKind::Rejected,
                "sink could not mint a receipt",
            )?,
        })
    }
}

impl EventSink for SilentSink {
    fn emit(
        &mut self,
        _event: &orchestrator_exec::WorkerEventDraft<'_>,
    ) -> Result<EventReceipt, EventSinkError> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let sequence = i64::try_from(sequence).unwrap_or(i64::MAX);
        EventReceipt::new(format!("evt_{sequence}"), "2026-09-03T00:00:00Z", sequence)
            .map_err(|_| self.error.clone())
    }
}

/// A local executor that writes one file into the worker directory and
/// completes, or completes with nothing, or fails outright.
struct LocalHelper {
    worktree: PathBuf,
    complete: bool,
    /// When set, the helper asks its `ExecutionContext` for one effect of every
    /// kind and records how each was refused. This is the only way to exercise
    /// the worker's effect service from outside `orchestrator-exec`, because
    /// `EffectBudget` can only be minted by `ExecutionContext::run_effect`.
    probe: Option<Arc<Mutex<Vec<EffectServiceErrorKind>>>>,
}

impl PhaseExecutor for LocalHelper {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        if let Some(probe) = self.probe.as_ref() {
            for kind in [
                EffectKind::GitMutation,
                EffectKind::FileWrite,
                EffectKind::NetworkRequest,
                EffectKind::ArtifactWrite,
                EffectKind::PluginAction,
                EffectKind::FileRead,
            ] {
                let Ok(request) = EffectRequest::new(kind, "refs/heads/main", "worker-attempt")
                else {
                    continue;
                };
                if let Err(error) = context.run_effect(&request) {
                    if let Ok(mut recorded) = probe.lock() {
                        recorded.push(error.kind());
                    }
                }
            }
        }
        // Writing into the worktree is what gives the commit something to
        // commit; the helper is deliberately provider-free and does no I/O
        // beyond this one file.
        let _ = std::fs::write(self.worktree.join("WORK.md"), b"helper output\n");
        if !self.complete {
            return AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::from_millis(1),
            );
        }
        AttemptOutcome::completed(
            "local helper complete",
            AttemptEvidence::new(),
            Duration::from_millis(1),
        )
        .unwrap_or_else(|_| {
            AttemptOutcome::incomplete(
                MechanicalTermination::ContractViolation,
                None,
                PartialWork::empty(),
                Duration::from_millis(1),
            )
        })
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        None
    }
}

/// Runs the local helper through the enforced registry seam, exactly as
/// `run_one_phase` does, and returns its terminal outcome.
///
/// The worker's `ExecutionContext` carries `DeniedGitMutationService`, so this
/// is also where "a worker holds no Git authority" is exercised rather than
/// asserted.
fn dispatch(worktree: &Path, complete: bool) -> TestResult<AttemptOutcome> {
    dispatch_with_probe(worktree, complete, None)
}

fn dispatch_with_probe(
    worktree: &Path,
    complete: bool,
    probe: Option<Arc<Mutex<Vec<EffectServiceErrorKind>>>>,
) -> TestResult<AttemptOutcome> {
    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register(
        "claude",
        Arc::new(LocalHelper {
            worktree: worktree.to_path_buf(),
            complete,
            probe,
        }),
    )?;
    let resolved = registry.resolve("claude")?;
    let effect = orchestrator_cli::worker_effect_service()?;
    let clock = SystemClock;
    let watchdog = FixedWatchdog;
    let process = NoProcess::new()?;
    let mut sink = SilentSink::new()?;
    let identity = WorkerIdentity::new(MISSION, PHASE, "worker-1")?;
    let mut context = ExecutionContext::new(
        &process,
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        identity,
        Instant::now() + Duration::from_secs(30),
    );
    let request =
        orchestrator_exec::ExecutionRequest::new(orchestrator_exec::ExecutionRequestDraft {
            mission: MISSION.to_owned(),
            phase: PHASE.to_owned(),
            attempt: 1,
            revision: 1,
            objective: "run the local helper".to_owned(),
            persona: "senior-backend-engineer".to_owned(),
            role: "implementer".to_owned(),
            domain: "dev".to_owned(),
            skills: Vec::new(),
            dependencies: Vec::new(),
            expected_evidence: Vec::new(),
            constraints: Vec::new(),
            prior_context: String::new(),
            runtime: resolved.effective_runtime().clone(),
            model: "sonnet".to_owned(),
            effort: orchestrator_exec::Effort::High,
            max_turns: 0,
            worker_dir: worktree.to_path_buf(),
            target_dir: None,
            resume_from: None,
            hook_script: None,
        })?;
    Ok(resolved.execute(&request, &mut context)?)
}

/// The commit, push, and pull-request receipts one publish produced. All three
/// are `None` when the phase did not complete.
type Published = (Option<GitReceipt>, Option<GitReceipt>, Option<GitReceipt>);

/// The commit / push / pull-request / cleanup half of one governed run.
fn publish(
    git: &mut GitEffectService<'_>,
    outcome: &AttemptOutcome,
) -> Result<Published, GitEffectError> {
    let phase = PhaseId::new(PHASE)
        .unwrap_or_else(|_| PhaseId::new("phase").unwrap_or_else(|_| unreachable!()));
    let Some(work) = VerifiedWork::from_outcome(&phase, 1, outcome) else {
        return Ok((None, None, None));
    };
    let commit = git.apply(GitIntent::Commit {
        message: "governed commit".to_owned(),
        work,
    })?;
    let push = git.apply(GitIntent::Push {
        remote: REMOTE.to_owned(),
        branch: BRANCH.to_owned(),
    })?;
    let acknowledged = matches!(
        push,
        GitReceipt::Pushed {
            ack: PushAck::Acknowledged { .. },
            ..
        }
    );
    let pull_request = if acknowledged {
        Some(git.apply(GitIntent::OpenPr {
            title: "governed run".to_owned(),
            body: "opened by the fixture adapter".to_owned(),
            draft: false,
        })?)
    } else {
        None
    };
    Ok((Some(commit), Some(push), pull_request))
}

/// Drives the whole seam: reconcile, refresh, isolate, dispatch, publish, clean
/// up. Every intent is idempotent, so calling this twice over the same store is
/// the resume path.
fn governed_run(fixture: &Fixture, complete: bool) -> TestResult<RunSummary> {
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    let reconciled = git.reconcile()?;
    let base = git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    let isolation = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    let outcome = dispatch(&fixture.worktree(), complete)?;
    let (commit, push, pull_request) = publish(&mut git, &outcome)?;
    let cleanup = git.apply(GitIntent::Cleanup {
        trash: orchestrator_app::TrashRoot::under(git.capability(), TRASH)?,
    })?;
    Ok(RunSummary {
        reconciled: reconciled
            .into_iter()
            .map(|entry| (entry.slot().to_owned(), entry.disposition()))
            .collect(),
        base,
        isolation,
        commit,
        push,
        pull_request,
        cleanup: Some(cleanup),
    })
}

struct RunSummary {
    reconciled: Vec<(String, ReconciliationDisposition)>,
    base: GitReceipt,
    isolation: GitReceipt,
    commit: Option<GitReceipt>,
    push: Option<GitReceipt>,
    pull_request: Option<GitReceipt>,
    cleanup: Option<GitReceipt>,
}

fn base_sha_of(receipt: &GitReceipt) -> String {
    match receipt {
        GitReceipt::BaseRefreshed { base_sha, .. }
        | GitReceipt::IsolationCreated { base_sha, .. } => base_sha.to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Setup cleanup — admission failures release only their owned ledger
// ---------------------------------------------------------------------------

#[test]
fn missing_repository_setup_closes_its_ledger_for_a_valid_retry() -> TestResult {
    let fixture = Fixture::new("missing-repository-setup")?;
    let adapter = fixture.adapter();
    let ledger = GitEffectLedger::open(&fixture.capability)?;
    let missing_repo = fixture.root().join("missing-repository");

    let refused = orchestrator_cli::governed_git_run(
        ledger,
        &fixture.capability,
        &adapter,
        MissionId::new(MISSION)?,
        PhaseId::new(PHASE)?,
        missing_repo,
    );
    assert!(
        matches!(refused, Err(GitEffectError::Git(_))),
        "a missing repository must retain its setup refusal, got {refused:?}"
    );

    // This is the exact same private ledger path. Reopening it in-process is
    // the regression: an unclean RuntimeStore drop would retain its lease.
    let mut git = fixture.service(&adapter)?;
    let base = git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    assert_eq!(base_sha_of(&base), fixture.origin_head);
    drop(git);
    Ok(())
}

#[test]
fn capability_refusal_closes_its_ledger_for_a_valid_retry() -> TestResult {
    let fixture = Fixture::new("capability-setup")?;
    let adapter = fixture.adapter();
    let ledger = GitEffectLedger::open(&fixture.capability)?;
    let root = fixture.root().to_path_buf();
    let displaced_root = fixture.parent.join("displaced-capability-root");

    // Replace only the capability root inode while keeping the already-open
    // ledger directory at its admitted canonical path. Capability admission
    // must refuse the replacement, while ledger close can still verify and
    // release the exact directory object it owns.
    std::fs::rename(&root, &displaced_root)?;
    private_dir(&root)?;
    std::fs::rename(displaced_root.join("git-ledger"), root.join("git-ledger"))?;

    let refused = orchestrator_cli::governed_git_run(
        ledger,
        &fixture.capability,
        &adapter,
        MissionId::new(MISSION)?,
        PhaseId::new(PHASE)?,
        displaced_root.join("work"),
    );
    assert!(
        matches!(refused, Err(GitEffectError::Capability(_))),
        "a replaced capability root must retain its refusal, got {refused:?}"
    );

    std::fs::rename(root.join("git-ledger"), displaced_root.join("git-ledger"))?;
    std::fs::remove_dir(&root)?;
    std::fs::rename(&displaced_root, &root)?;

    let mut git = fixture.service(&adapter)?;
    let base = git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    assert_eq!(base_sha_of(&base), fixture.origin_head);
    drop(git);
    Ok(())
}

#[test]
fn confinement_refusal_releases_only_the_ledger_it_owned() -> TestResult {
    let fixture = Fixture::new("confined-setup")?;
    let other = Fixture::new("separately-owned-ledger")?;
    let adapter = fixture.adapter();
    let other_adapter = other.adapter();
    let ledger = GitEffectLedger::open(&fixture.capability)?;
    let other_ledger = GitEffectLedger::open(&other.capability)?;

    let refused = orchestrator_cli::governed_git_run(
        ledger,
        &fixture.capability,
        &adapter,
        MissionId::new(MISSION)?,
        PhaseId::new(PHASE)?,
        other.repo(),
    );
    assert!(
        matches!(refused, Err(GitEffectError::UnsafeComponent)),
        "an out-of-root repository must retain its confinement refusal, got {refused:?}"
    );

    // Cleanup is scoped to the ledger consumed by the failed constructor. It
    // must neither retain that lease nor globally release another live owner.
    let still_owned = GitEffectLedger::open(&other.capability);
    assert!(
        matches!(
            still_owned,
            Err(GitEffectError::Store(
                orchestrator_app::RuntimeStoreError::WriterLeased
            ))
        ),
        "setup cleanup must not release a separately owned ledger, got {still_owned:?}"
    );

    let mut git = fixture.service(&adapter)?;
    let base = git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    assert_eq!(base_sha_of(&base), fixture.origin_head);
    drop(git);
    let other_git = orchestrator_cli::governed_git_run(
        other_ledger,
        &other.capability,
        &other_adapter,
        MissionId::new(MISSION)?,
        PhaseId::new(PHASE)?,
        other.repo(),
    )?;
    drop(other_git);
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 1 — the exact base sha is recorded before any branch exists
// ---------------------------------------------------------------------------

#[test]
fn run_records_the_exact_base_sha_before_any_branch() -> TestResult {
    let fixture = Fixture::new("exact-base")?;
    let adapter = fixture.adapter();

    assert_eq!(
        branches_named(&fixture.repo(), BRANCH)?,
        0,
        "no isolation branch may exist before the run"
    );

    let mut git = fixture.service(&adapter)?;
    let base = git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;

    // The base is durable and the branch still does not exist: "record, then
    // branch", the inversion of the Go oracle's ordering defect.
    assert_eq!(
        branches_named(&fixture.repo(), BRANCH)?,
        0,
        "the base must be recorded before any branch is cut"
    );
    assert_eq!(
        base_sha_of(&base),
        fixture.origin_head,
        "the base must be the remote's head"
    );
    assert_ne!(
        base_sha_of(&base),
        fixture.stale_local_head,
        "the stale local main must not be selected (TRK-1116)"
    );

    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    let isolation = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    drop(git);

    // The journaled plan carries the exact sha, not a branch name.
    let row = fixture
        .row(SLOT_CREATE_ISOLATION)?
        .ok_or("no create-isolation row")?;
    assert_eq!(row.plan("base_sha"), fixture.origin_head);
    assert_eq!(row.plan("branch"), BRANCH);
    assert_eq!(base_sha_of(&isolation), fixture.origin_head);
    assert_eq!(
        rev_parse(&fixture.repo(), BRANCH)?,
        fixture.origin_head,
        "the branch must resolve to the refreshed base"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 2 — branch, worktree, lock, and claim exist exactly once
// ---------------------------------------------------------------------------

#[test]
fn isolation_branch_worktree_lock_and_claim_are_created_exactly_once() -> TestResult {
    let fixture = Fixture::new("exactly-once")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    let receipt = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;

    assert_eq!(linked_worktrees(&fixture.repo())?, 1);
    assert_eq!(branches_named(&fixture.repo(), BRANCH)?, 1);
    assert!(orchestrator_git::is_locked(&fixture.worktree()));
    let GitReceipt::IsolationCreated { lock_pid, .. } = &receipt else {
        return Err("expected an isolation receipt".into());
    };
    assert_eq!(*lock_pid, std::process::id(), "the lock must hold our PID");

    let claims = fixture.claims()?;
    let conflicts = claims.check_conflicts(
        MISSION,
        &fixture.repo().to_string_lossy(),
        &["WORK.md".to_owned()],
    )?;
    claims.close()?;
    assert!(conflicts.is_empty(), "no other mission may hold a claim");
    drop(git);

    // The durable proof of "exactly once": the row was claimed a single time.
    // A second claim on the same key is refused as `ExpectedEffectNotClaimable`
    // rather than silently starting a second attempt, so `attempts` can only be
    // 1 if nothing duplicated it.
    let row = fixture
        .row(SLOT_CREATE_ISOLATION)?
        .ok_or("no create-isolation row")?;
    assert_eq!(row.state, "succeeded");
    assert_eq!(row.attempts, 1, "the isolation was claimed more than once");
    assert_eq!(row.identities, 1, "exactly one execution identity");
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 3 — verified work is committed; unverified work is not
// ---------------------------------------------------------------------------

#[test]
fn a_verified_phase_is_committed() -> TestResult {
    let fixture = Fixture::new("verified-commit")?;
    let summary = governed_run(&fixture, true)?;
    assert!(summary.commit.is_some(), "a completed phase must commit");
    let row = fixture.row(SLOT_COMMIT)?.ok_or("no commit row")?;
    assert_eq!(row.state, "succeeded");
    // The base commit plus this run's commit.
    assert_eq!(commits_on(&fixture.repo(), BRANCH)?, 3);
    Ok(())
}

#[test]
fn an_unverified_phase_is_not_committed() -> TestResult {
    let fixture = Fixture::new("unverified-commit")?;
    let phase = PhaseId::new(PHASE)?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    let outcome = dispatch(&fixture.worktree(), false)?;
    assert!(!outcome.is_completed());
    assert!(
        VerifiedWork::from_outcome(&phase, 1, &outcome).is_none(),
        "an incomplete attempt must not mint a witness"
    );
    let published = publish(&mut git, &outcome)?;
    drop(git);

    assert!(published.0.is_none() && published.1.is_none() && published.2.is_none());
    assert!(
        fixture.row(SLOT_COMMIT)?.is_none(),
        "no git-commit row may ever be journaled for unverified work"
    );
    assert!(fixture.row(SLOT_PUSH)?.is_none());
    assert!(fixture.row(SLOT_OPEN_PR)?.is_none());
    // Only the two base commits: the helper's file is present but uncommitted.
    assert_eq!(commits_on(&fixture.repo(), BRANCH)?, 2);
    assert!(fixture.worktree().join("WORK.md").exists());
    Ok(())
}

#[test]
fn a_witness_from_another_phase_cannot_authorize_a_commit() -> TestResult {
    let fixture = Fixture::new("witness-phase")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    let outcome = dispatch(&fixture.worktree(), true)?;
    let other = PhaseId::new("some-other-phase")?;
    let work = VerifiedWork::from_outcome(&other, 1, &outcome).ok_or("no witness")?;
    let refused = git.apply(GitIntent::Commit {
        message: "smuggled".to_owned(),
        work,
    });
    drop(git);
    assert!(
        matches!(refused, Err(GitEffectError::WitnessPhaseMismatch)),
        "a foreign witness must be refused, got {refused:?}"
    );
    assert!(fixture.row(SLOT_COMMIT)?.is_none());
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 4 — an acknowledgement is two facts, not an exit code
// ---------------------------------------------------------------------------

/// Installs a `post-receive` hook that resets whatever ref it just accepted
/// back to the base commit. `git push` still exits zero, so a receipt that says
/// `Acknowledged` could only have come from the exit code alone.
fn install_ref_moving_hook(fixture: &Fixture) -> TestResult {
    let hooks = fixture.bare().join("hooks");
    std::fs::create_dir_all(&hooks)?;
    let hook = hooks.join("post-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nwhile read -r old new ref; do\n  git update-ref \"$ref\" {}\ndone\n",
            fixture.origin_head
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[test]
fn push_acknowledgement_comes_from_ls_remote_not_the_exit_code() -> TestResult {
    let fixture = Fixture::new("ack-observed")?;
    install_ref_moving_hook(&fixture)?;
    let summary = governed_run(&fixture, true)?;

    let push = summary.push.ok_or("no push receipt")?;
    let GitReceipt::Pushed { ack, .. } = &push else {
        return Err("expected a push receipt".into());
    };
    assert_eq!(
        *ack,
        PushAck::Ambiguous,
        "a moved remote ref must never read as acknowledged, got {ack:?}"
    );
    assert!(
        summary.pull_request.is_none(),
        "an unacknowledged push must not open a pull request"
    );
    let row = fixture.row(SLOT_PUSH)?.ok_or("no push row")?;
    assert_eq!(row.state, "uncertain", "the push row must be retained");
    assert_eq!(ledger_entries(&fixture.ledger())?, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 5 — an ambiguous push is never blindly retried
// ---------------------------------------------------------------------------

#[test]
fn an_ambiguous_push_is_not_blindly_retried() -> TestResult {
    let fixture = Fixture::new("ambiguous-push")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    let outcome = dispatch(&fixture.worktree(), true)?;
    let phase = PhaseId::new(PHASE)?;
    let work = VerifiedWork::from_outcome(&phase, 1, &outcome).ok_or("no witness")?;
    git.apply(GitIntent::Commit {
        message: "governed commit".to_owned(),
        work,
    })?;

    // Make the remote unobservable *before* the push, so the transport failure
    // is a genuine "nothing may be concluded" rather than a decided rejection.
    let hidden = fixture.root().join("origin.hidden");
    std::fs::rename(fixture.bare(), &hidden)?;
    let push = git.apply(GitIntent::Push {
        remote: REMOTE.to_owned(),
        branch: BRANCH.to_owned(),
    })?;
    let GitReceipt::Pushed { ack, .. } = &push else {
        return Err("expected a push receipt".into());
    };
    assert_eq!(*ack, PushAck::Ambiguous);
    drop(git);
    assert_eq!(
        fixture.row(SLOT_PUSH)?.ok_or("no push row")?.state,
        "uncertain"
    );

    // Restart. The remote is still unobservable, so reconciliation may only
    // retain the row: there is no path from `Uncertain` to a re-push without an
    // intervening observation.
    let mut resumed = fixture.service(&adapter)?;
    let reconciled = resumed.reconcile()?;
    drop(resumed);
    assert_eq!(
        reconciled
            .iter()
            .filter(|entry| entry.slot() == SLOT_PUSH
                && entry.disposition() == ReconciliationDisposition::RetainedUncertain)
            .count(),
        1,
        "the push row must be retained, got {reconciled:?}"
    );
    assert_eq!(
        fixture.row(SLOT_PUSH)?.ok_or("no push row")?.state,
        "uncertain"
    );

    // Restore the remote and prove nothing was ever published to it.
    std::fs::rename(&hidden, fixture.bare())?;
    assert_eq!(
        ls_remote(&fixture.bare(), BRANCH)?.len(),
        0,
        "no ref may have been published by a retained-uncertain push"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Case 6 — the pull-request receipt is deterministic and offline
// ---------------------------------------------------------------------------

#[test]
fn pr_receipt_is_deterministic_and_offline() -> TestResult {
    let fixture = Fixture::new("pr-deterministic")?;
    let summary = governed_run(&fixture, true)?;
    let opened = summary.pull_request.ok_or("no pull-request receipt")?;
    let GitReceipt::PrOpened {
        receipt_id, url, ..
    } = &opened
    else {
        return Err("expected a pull-request receipt".into());
    };
    assert!(
        url.starts_with("fixture://"),
        "the url scheme must not resolve anywhere, got {url}"
    );
    assert_eq!(ledger_entries(&fixture.ledger())?, 1);

    // Opening the same head again through a fresh adapter yields the same id
    // and writes no second ledger file. The id is a pure function of the head,
    // so a second pull request is not representable.
    let adapter = fixture.adapter();
    let head = orchestrator_git::CommitSha::parse(&rev_parse(&fixture.repo(), BRANCH)?)
        .ok_or("head is not a commit")?;
    let request = PrRequest {
        repo_root: fixture.repo(),
        base_branch: BASE_BRANCH.to_owned(),
        head_branch: BRANCH.to_owned(),
        head_sha: head,
        title: "governed run".to_owned(),
        body: "opened by the fixture adapter".to_owned(),
        draft: false,
    };
    let again = adapter.open(&request)?;
    assert_eq!(&again.receipt_id, receipt_id);
    assert_eq!(ledger_entries(&fixture.ledger())?, 1);
    Ok(())
}

/// Anti-vacuity for C6: a counter-based adapter mints a second receipt for the
/// same head, proving the deterministic id is what prevents a duplicate pull
/// request rather than some incidental property of the flow.
struct CountingPrAdapter {
    ledger: PathBuf,
    next: Mutex<u64>,
}

impl PrAdapter for CountingPrAdapter {
    fn provider(&self) -> &str {
        "counting"
    }

    fn receipt_id(&self, _request: &PrRequest) -> Result<String, PrAdapterError> {
        let mut next = self
            .next
            .lock()
            .map_err(|_| PrAdapterError::RemoteUnobservable)?;
        *next += 1;
        Ok(format!("counting-pr-{}", *next))
    }

    fn open(&self, request: &PrRequest) -> Result<PrReceipt, PrAdapterError> {
        let receipt_id = self.receipt_id(request)?;
        let receipt = PrReceipt {
            provider: self.provider().to_owned(),
            receipt_id: receipt_id.clone(),
            url: format!("counting://{receipt_id}"),
            head_sha: request.head_sha.as_str().to_owned(),
        };
        std::fs::write(
            self.ledger.join(format!("{receipt_id}.json")),
            serde_json::to_vec(&receipt).map_err(|_| PrAdapterError::LedgerCorrupt)?,
        )?;
        Ok(receipt)
    }

    fn lookup(&self, _receipt_id: &str) -> Result<Option<PrReceipt>, PrAdapterError> {
        Ok(None)
    }
}

#[test]
fn control_a_counter_based_adapter_would_open_a_second_pull_request() -> TestResult {
    let fixture = Fixture::new("pr-counter-control")?;
    let counting = CountingPrAdapter {
        ledger: fixture.ledger(),
        next: Mutex::new(0),
    };
    let head = orchestrator_git::CommitSha::parse(&fixture.origin_head)
        .ok_or("origin head is not a commit")?;
    let request = PrRequest {
        repo_root: fixture.repo(),
        base_branch: BASE_BRANCH.to_owned(),
        head_branch: BRANCH.to_owned(),
        head_sha: head,
        title: "control".to_owned(),
        body: "control".to_owned(),
        draft: false,
    };
    let first = counting.open(&request)?;
    let second = counting.open(&request)?;
    assert_ne!(
        first.receipt_id, second.receipt_id,
        "the control must produce two distinct receipts"
    );
    assert_eq!(
        ledger_entries(&fixture.ledger())?,
        2,
        "the control must leave two ledger entries"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Cases 7-12 — one per B4-DESIGN §4 crash point
//
// The crash is a real one. `drop(git)` used to stand in for it, but dropping
// runs `Drop`, which closes the ledger, checkpoints the WAL and releases the
// writer lease — the three things a died process by definition does not do.
// Every case below instead re-execs this binary, lets the child reach the
// boundary, and `SIGKILL`s it there; the parent then resumes against the
// ledger the killed process left uncleanly closed.
// ---------------------------------------------------------------------------

/// Fixture root the crash helper adopts.
const CRASH_HELPER_ROOT: &str = "NANIKA_B4_GIT_CRASH_ROOT";
/// Barrier the crash helper stops at, by [`barrier_name`].
const CRASH_HELPER_POINT: &str = "NANIKA_B4_GIT_CRASH_POINT";
/// Operation slot the crash helper stops on.
const CRASH_HELPER_SLOT: &str = "NANIKA_B4_GIT_CRASH_SLOT";
/// File the crash helper creates once it is parked at the boundary. It lives
/// outside the fixture root so the root's own contents stay exactly what the
/// run put there.
const CRASH_HELPER_MARKER: &str = "NANIKA_B4_GIT_CRASH_MARKER";
const CRASH_HELPER_CASE: &str = "crash_helper_entrypoint";

const fn barrier_name(point: GitEffectBarrier) -> &'static str {
    match point {
        GitEffectBarrier::ClaimedBeforeExecution => "claimed-before-execution",
        GitEffectBarrier::IsolationBranchBeforeWorktree => "isolation-branch-before-worktree",
        GitEffectBarrier::IsolationWorktreeBeforeResolution => {
            "isolation-worktree-before-resolution"
        }
        GitEffectBarrier::ExecutedBeforeResolution => "executed-before-resolution",
        GitEffectBarrier::SlotResolved => "slot-resolved",
    }
}

fn barrier_from_name(name: &str) -> Option<GitEffectBarrier> {
    [
        GitEffectBarrier::ClaimedBeforeExecution,
        GitEffectBarrier::IsolationBranchBeforeWorktree,
        GitEffectBarrier::IsolationWorktreeBeforeResolution,
        GitEffectBarrier::ExecutedBeforeResolution,
        GitEffectBarrier::SlotResolved,
    ]
    .into_iter()
    .find(|point| barrier_name(*point) == name)
}

/// Re-exec entry point for the crash helper.
///
/// In an ordinary run the selector variables are unset and this returns at
/// once. Under the selector it adopts the parent's fixture root, drives the
/// governed sequence, parks at the requested boundary and waits to be killed.
/// It exits non-zero on every path that is not a kill, so the parent can tell
/// "never reached the boundary" from "died there".
#[test]
fn crash_helper_entrypoint() {
    let Ok(root) = std::env::var(CRASH_HELPER_ROOT) else {
        return;
    };
    let code = match crash_helper(Path::new(&root)) {
        Ok(()) => 3,
        Err(error) => {
            eprintln!("crash helper failed: {error}");
            4
        }
    };
    std::process::exit(code);
}

fn crash_helper(root_path: &Path) -> TestResult {
    let point = barrier_from_name(&std::env::var(CRASH_HELPER_POINT).unwrap_or_default())
        .ok_or("the crash helper was given no recognizable barrier")?;
    let slot = std::env::var(CRASH_HELPER_SLOT)?;
    let marker = PathBuf::from(std::env::var(CRASH_HELPER_MARKER)?);

    let root = IsolatedFixtureRoot::identify(root_path)?;
    // The parent created this root and still holds the fresh capability it
    // minted; the helper adopts the same tree through the complementary door.
    let capability = GitEffectCapability::recover_fixture(&root, &[REMOTE])?;
    let adapter = FixturePrAdapter::new(root_path.join("origin.git"), root_path.join("pr-ledger"));
    let ledger = GitEffectLedger::open(&capability)?;
    let mut git = orchestrator_cli::governed_git_run(
        ledger,
        &capability,
        &adapter,
        MissionId::new(MISSION)?,
        PhaseId::new(PHASE)?,
        root_path.join("work"),
    )?;
    git.observe_barriers(Arc::new(move |observed, observed_slot| {
        if observed != point || observed_slot != slot {
            return GitBarrierDecision::Continue;
        }
        // Every durable write this boundary defines is already committed.
        // Announce the arrival and then block: the parent sends `SIGKILL`
        // here, so no `Drop`, no ledger close, and no WAL checkpoint runs.
        let _ = std::fs::write(&marker, b"parked\n");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }));
    let outcome = drive_until_cut(&root_path.join("worktrees").join(WORKTREE_DIR), &mut git);
    Err(format!("the run reached its end without parking at {point:?}: {outcome:?}").into())
}

/// Runs the governed sequence in a child process and `SIGKILL`s it the moment
/// it parks at `point`/`slot`.
///
/// `Child::kill` is `SIGKILL` on Unix, which is uncatchable, so the child gets
/// no chance to flush, close or unwind. The parent proves it went that way by
/// requiring signal 9 rather than an exit code.
fn kill_at_barrier(fixture: &Fixture, point: GitEffectBarrier, slot: &str) -> TestResult {
    let marker = fixture.parent.join(format!("crash-armed-{slot}"));
    let _ = std::fs::remove_file(&marker);
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            CRASH_HELPER_CASE,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CRASH_HELPER_ROOT, fixture.root())
        .env(CRASH_HELPER_POINT, barrier_name(point))
        .env(CRASH_HELPER_SLOT, slot)
        .env(CRASH_HELPER_MARKER, &marker)
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(120);
    while !marker.exists() {
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "the crash helper exited with {status:?} before parking at {point:?} on {slot}"
            )
            .into());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("the crash helper never parked at {point:?} on {slot}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    child.kill()?;
    let status = child.wait()?;
    assert_eq!(
        status.signal(),
        Some(9),
        "the crash helper must die by SIGKILL, got {status:?}"
    );
    Ok(())
}

/// Kills a child at `point`/`slot`, then replays the whole run in this process
/// over whatever durable state the killed one left behind.
fn crash_then_resume(
    fixture: &Fixture,
    point: GitEffectBarrier,
    slot: &'static str,
) -> TestResult<RunSummary> {
    kill_at_barrier(fixture, point, slot)?;
    governed_run(fixture, true)
}

/// The same sequence `governed_run` drives, stopping at the first refusal so a
/// refusal ends the run instead of propagating out of the harness.
fn drive_until_cut(worktree: &Path, git: &mut GitEffectService<'_>) -> Result<(), GitEffectError> {
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let Some(refreshed) = git.take_refreshed_base() else {
        return Ok(());
    };
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION).map_err(|_| GitEffectError::MissingIsolation)?,
        task: TASK.to_owned(),
    })?;
    let Ok(outcome) = dispatch(worktree, true) else {
        return Ok(());
    };
    let phase = PhaseId::new(PHASE).map_err(|_| GitEffectError::MissingIsolation)?;
    let Some(work) = VerifiedWork::from_outcome(&phase, 1, &outcome) else {
        return Ok(());
    };
    git.apply(GitIntent::Commit {
        message: "governed commit".to_owned(),
        work,
    })?;
    git.apply(GitIntent::Push {
        remote: REMOTE.to_owned(),
        branch: BRANCH.to_owned(),
    })?;
    git.apply(GitIntent::OpenPr {
        title: "governed run".to_owned(),
        body: "opened by the fixture adapter".to_owned(),
        draft: false,
    })?;
    Ok(())
}

/// Every "zero duplicates" assertion, in one place, so each crash case states
/// the same standard.
fn assert_no_duplicates(fixture: &Fixture, summary: &RunSummary) -> TestResult {
    assert_eq!(
        branches_named(&fixture.repo(), BRANCH)?,
        1,
        "exactly one isolation branch"
    );
    // Cleanup renamed the worktree into trash, so the repository must hold no
    // linked worktree at all and exactly one trash entry must exist.
    assert_eq!(
        linked_worktrees(&fixture.repo())?,
        0,
        "no linked worktree may survive cleanup"
    );
    assert_eq!(
        std::fs::read_dir(fixture.root().join(TRASH))?.count(),
        1,
        "exactly one trash entry"
    );
    assert_eq!(
        ls_remote(&fixture.bare(), BRANCH)?.len(),
        1,
        "exactly one remote ref"
    );
    assert_eq!(
        ledger_entries(&fixture.ledger())?,
        1,
        "exactly one pull request"
    );
    assert_eq!(
        commits_on(&fixture.repo(), BRANCH)?,
        3,
        "exactly one commit above the two base commits"
    );
    assert_eq!(
        base_sha_of(&summary.isolation),
        fixture.origin_head,
        "the resumed isolation must still name the base the first process declared"
    );
    assert!(summary.commit.is_some() && summary.push.is_some());
    assert!(summary.pull_request.is_some());
    assert!(summary.cleanup.is_some());
    Ok(())
}

#[test]
fn resume_after_crash_at_c1_base_recorded_before_branch() -> TestResult {
    let fixture = Fixture::new("c1")?;
    // C1: the base is durable and the isolation has not been journaled.
    let summary = crash_then_resume(&fixture, GitEffectBarrier::SlotResolved, SLOT_REFRESH_BASE)?;
    assert_eq!(
        summary.reconciled.len(),
        0,
        "a cleanly resolved refresh needs no reconciliation, got {:?}",
        summary.reconciled
    );
    // The base is the one the first process declared and was **not** refetched.
    assert_eq!(base_sha_of(&summary.base), fixture.origin_head);
    let row = fixture.row(SLOT_REFRESH_BASE)?.ok_or("no refresh row")?;
    assert_eq!(row.state, "succeeded");
    assert_eq!(row.attempts, 1, "the base must not have been fetched twice");
    assert_no_duplicates(&fixture, &summary)
}

/// Anti-vacuity for C1: re-fetching after the remote advances yields a
/// different base, which is exactly the hazard "never re-fetch" prevents.
#[test]
fn control_refetching_after_the_remote_advances_moves_the_base() -> TestResult {
    let fixture = Fixture::new("c1-control")?;
    let first = orchestrator_git::resolve_refreshed_base(
        &fixture.repo(),
        REMOTE,
        BASE_BRANCH,
        orchestrator_git::RemotePolicy::LocalOnly,
    )?;
    let publisher = fixture.root().join("publisher");
    std::fs::write(publisher.join("THIRD.md"), b"advanced again\n")?;
    git(&publisher, &["add", "-A"])?;
    git(&publisher, &["commit", "-m", "advance again"])?;
    git(&publisher, &["push", "origin", "main"])?;

    let second = orchestrator_git::resolve_refreshed_base(
        &fixture.repo(),
        REMOTE,
        BASE_BRANCH,
        orchestrator_git::RemotePolicy::LocalOnly,
    )?;
    assert_ne!(
        first.base_sha(),
        second.base_sha(),
        "a second fetch must be able to move the base, or C1 proves nothing"
    );
    // The non-fetching read is what resume uses, and it still reports the base
    // the *last fetch* recorded rather than re-observing the remote.
    let recorded = orchestrator_git::recorded_refreshed_base(&fixture.repo(), REMOTE, BASE_BRANCH)?;
    assert_eq!(recorded.base_sha(), second.base_sha());
    Ok(())
}

#[test]
fn resume_after_crash_at_c2_branch_created_before_worktree() -> TestResult {
    let fixture = Fixture::new("c2")?;
    let summary = crash_then_resume(
        &fixture,
        GitEffectBarrier::IsolationBranchBeforeWorktree,
        SLOT_CREATE_ISOLATION,
    )?;
    assert!(
        summary
            .reconciled
            .iter()
            .any(|(slot, disposition)| slot == SLOT_CREATE_ISOLATION
                && *disposition == ReconciliationDisposition::ObservedAbsentAndRequeued),
        "the isolation row must be requeued, got {:?}",
        summary.reconciled
    );
    assert_no_duplicates(&fixture, &summary)
}

/// Anti-vacuity for C2: a branch that has moved off the recorded base is
/// refused rather than reset, so reconciliation cannot silently discard work.
#[test]
fn control_a_moved_isolation_branch_is_refused_not_reset() -> TestResult {
    let fixture = Fixture::new("c2-control")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    // Someone else's branch already occupies the name, at an unrelated commit.
    let foreign = rev_parse(&fixture.repo(), &format!("{}^", fixture.origin_head))?;
    git_branch(&fixture.repo(), BRANCH, &foreign)?;

    let refused = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    });
    drop(git);
    assert!(
        matches!(refused, Err(GitEffectError::BranchCarriesUnknownWork)),
        "a moved branch must be refused, got {refused:?}"
    );
    assert_eq!(
        rev_parse(&fixture.repo(), BRANCH)?,
        foreign,
        "the foreign branch must be left exactly where it was"
    );
    Ok(())
}

fn git_branch(repo: &Path, name: &str, at: &str) -> TestResult {
    git(repo, &["branch", name, at])?;
    Ok(())
}

#[test]
fn resume_after_crash_at_c3_worktree_created_before_commit() -> TestResult {
    let fixture = Fixture::new("c3")?;
    let summary = crash_then_resume(
        &fixture,
        GitEffectBarrier::IsolationWorktreeBeforeResolution,
        SLOT_CREATE_ISOLATION,
    )?;
    // The worktree was registered and locked before the kill, but the lock
    // names the killed process, and `is_locked` is liveness-checked — so after
    // a *real* crash the isolation reads as registered-but-unowned and is
    // requeued for exactly one re-claim. The simulated crash this case used to
    // run reported `AdoptedObservedSuccess` only because the lock's pid was
    // the still-running test process; that disposition is unreachable from an
    // actual death and asserting it proved nothing about recovery.
    assert!(
        summary
            .reconciled
            .iter()
            .any(|(slot, disposition)| slot == SLOT_CREATE_ISOLATION
                && *disposition == ReconciliationDisposition::ObservedAbsentAndRequeued),
        "the isolation must be requeued for re-adoption, got {:?}",
        summary.reconciled
    );
    // What matters is that the re-claim *adopts*: one branch, one worktree,
    // one re-claim, and the lock now names this process.
    let row = fixture
        .row(SLOT_CREATE_ISOLATION)?
        .ok_or("no isolation row")?;
    assert_eq!(row.state, "succeeded");
    assert_eq!(
        row.attempts, 2,
        "the isolation must be re-claimed exactly once, not created afresh"
    );
    assert_eq!(
        base_sha_of(&summary.isolation),
        fixture.origin_head,
        "the re-adopted isolation must still name the base the killed process declared"
    );
    assert_no_duplicates(&fixture, &summary)
}

/// Anti-vacuity for C3: an unregistered directory is a real hazard — git
/// refuses to add a second worktree over it, so without repair-or-adopt the
/// resumed run would be stuck rather than silently duplicating.
#[test]
fn control_an_unregistered_worktree_directory_blocks_a_second_add() -> TestResult {
    let fixture = Fixture::new("c3-control")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    drop(git);

    let second = fixture.root().join("worktrees").join("elsewhere");
    let refused = Command::new(real_git())
        .current_dir(fixture.repo())
        .args(["worktree", "add", str(&second), BRANCH])
        .output()?;
    assert!(
        !refused.status.success(),
        "git must refuse a second worktree for the same branch"
    );
    assert_eq!(linked_worktrees(&fixture.repo())?, 1);
    Ok(())
}

#[test]
fn resume_after_crash_at_c4_commit_landed_before_push() -> TestResult {
    let fixture = Fixture::new("c4")?;
    let summary = crash_then_resume(
        &fixture,
        GitEffectBarrier::ExecutedBeforeResolution,
        SLOT_COMMIT,
    )?;
    // The commit landed before the cut and the worktree is clean, so the row
    // resolves from observation and the commit is not made twice.
    assert!(
        summary
            .reconciled
            .iter()
            .any(|(slot, disposition)| slot == SLOT_COMMIT
                && *disposition == ReconciliationDisposition::AdoptedObservedSuccess),
        "the commit must be adopted from observation, got {:?}",
        summary.reconciled
    );
    assert_no_duplicates(&fixture, &summary)
}

/// Anti-vacuity for C4: committing twice really does produce two commits, so
/// "exactly one commit above the base" is a property of reconciliation and not
/// of git.
#[test]
fn control_committing_twice_produces_two_commits() -> TestResult {
    let fixture = Fixture::new("c4-control")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    drop(git);

    let worktree = fixture.worktree();
    identify(&worktree)?;
    std::fs::write(worktree.join("ONE.md"), b"one\n")?;
    orchestrator_git::commit_all(&worktree, "first")?;
    std::fs::write(worktree.join("TWO.md"), b"two\n")?;
    orchestrator_git::commit_all(&worktree, "second")?;
    assert_eq!(
        commits_on(&fixture.repo(), BRANCH)?,
        4,
        "the raw primitive must be able to produce a duplicate commit"
    );
    Ok(())
}

#[test]
fn resume_after_crash_at_c5_push_landed_before_receipt() -> TestResult {
    let fixture = Fixture::new("c5")?;
    let summary = crash_then_resume(
        &fixture,
        GitEffectBarrier::ExecutedBeforeResolution,
        SLOT_PUSH,
    )?;
    assert!(
        summary
            .reconciled
            .iter()
            .any(|(slot, disposition)| slot == SLOT_PUSH
                && *disposition == ReconciliationDisposition::AdoptedObservedSuccess),
        "the push must be adopted from ls-remote, got {:?}",
        summary.reconciled
    );
    assert_no_duplicates(&fixture, &summary)
}

/// Anti-vacuity for C5: a second push really does move the remote ref, so
/// resolving an executed-but-unresolved push by *observing* `ls-remote` rather
/// than by pushing again is load-bearing. If a blind requeue were harmless,
/// C5 would be proving a property of git instead of one of reconciliation.
#[test]
fn control_a_second_push_moves_the_remote_ref() -> TestResult {
    let fixture = Fixture::new("c5-control")?;
    let adapter = fixture.adapter();
    // Named `service` rather than `git`, which is the raw-git helper this case
    // needs by name below.
    let mut service = fixture.service(&adapter)?;
    service.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = service.take_refreshed_base().ok_or("no refreshed base")?;
    service.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    drop(service);

    let worktree = fixture.worktree();
    identify(&worktree)?;
    std::fs::write(worktree.join("ONE.md"), b"one\n")?;
    orchestrator_git::commit_all(&worktree, "first")?;
    git(&worktree, &["push", REMOTE, BRANCH])?;
    let first = ls_remote(&fixture.bare(), BRANCH)?;
    assert_eq!(first.len(), 1, "the first push must land one remote ref");

    std::fs::write(worktree.join("TWO.md"), b"two\n")?;
    orchestrator_git::commit_all(&worktree, "second")?;
    git(&worktree, &["push", REMOTE, BRANCH])?;
    let second = ls_remote(&fixture.bare(), BRANCH)?;
    assert_ne!(
        first, second,
        "the raw primitive must be able to move the remote ref, or C5 proves nothing"
    );
    Ok(())
}

#[test]
fn resume_after_crash_at_c6_pr_receipt_before_resolution() -> TestResult {
    let fixture = Fixture::new("c6")?;
    let summary = crash_then_resume(
        &fixture,
        GitEffectBarrier::ExecutedBeforeResolution,
        SLOT_OPEN_PR,
    )?;
    assert!(
        summary
            .reconciled
            .iter()
            .any(|(slot, disposition)| slot == SLOT_OPEN_PR
                && *disposition == ReconciliationDisposition::AdoptedObservedSuccess),
        "the pull request must be adopted from its ledger, got {:?}",
        summary.reconciled
    );
    assert_no_duplicates(&fixture, &summary)
}

/// A cut leaves the durable shape a died-mid-effect process leaves, and that
/// shape is unreachable from any graceful return: the row is `Executing` and
/// carries no execution identity, because the identity is only recorded once
/// the child's outcome is in hand.
#[test]
fn a_cut_effect_leaves_an_executing_row_with_no_execution_identity() -> TestResult {
    let fixture = Fixture::new("cut-shape")?;
    kill_at_barrier(
        &fixture,
        GitEffectBarrier::ExecutedBeforeResolution,
        SLOT_COMMIT,
    )?;

    let row = fixture.row(SLOT_COMMIT)?.ok_or("no commit row")?;
    assert_eq!(row.state, "executing");
    assert_eq!(
        row.identities, 0,
        "a crash-interrupted git child never records an execution identity"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Cleanup confinement and rollback
// ---------------------------------------------------------------------------

#[test]
fn cleanup_renames_into_a_confined_trash_root() -> TestResult {
    let fixture = Fixture::new("cleanup")?;
    let summary = governed_run(&fixture, true)?;
    let Some(GitReceipt::CleanedUp { trash_entry }) = summary.cleanup else {
        return Err("expected a cleanup receipt".into());
    };
    let canonical_root = std::fs::canonicalize(fixture.root())?;
    assert!(
        std::fs::canonicalize(&trash_entry)?.starts_with(&canonical_root),
        "the trash entry must canonicalize inside the fixture root"
    );
    assert!(
        trash_entry.join(".nanika-trash-meta.json").exists(),
        "cleanup must leave recoverable metadata"
    );
    assert!(!fixture.worktree().exists());
    Ok(())
}

#[test]
fn cleanup_refuses_a_target_outside_the_fixture_root() -> TestResult {
    let fixture = Fixture::new("cleanup-outside")?;
    let outside = fixture.parent.join("outside");
    private_dir(&outside)?;
    let trash = fixture.root().join(TRASH);
    std::fs::create_dir_all(&trash)?;

    let refused = orchestrator_git::remove_worktree_confined(&outside, &trash, fixture.root());
    assert!(
        matches!(
            refused,
            Err(orchestrator_git::GitError::OutsideConfinement { .. })
        ),
        "an outside target must be refused, got {refused:?}"
    );
    assert!(outside.exists(), "the outside directory must be untouched");
    Ok(())
}

#[test]
fn rollback_trashes_the_worktree_and_deletes_an_unmoved_branch() -> TestResult {
    let fixture = Fixture::new("rollback")?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    git.apply(GitIntent::RefreshBase {
        remote: REMOTE.to_owned(),
        base_branch: BASE_BRANCH.to_owned(),
    })?;
    let refreshed = git.take_refreshed_base().ok_or("no refreshed base")?;
    let isolation = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: MissionId::new(MISSION)?,
        task: TASK.to_owned(),
    })?;
    let rolled = git.apply(GitIntent::Rollback { of: isolation })?;
    drop(git);

    let GitReceipt::RolledBack { action, .. } = &rolled else {
        return Err("expected a rollback receipt".into());
    };
    assert!(matches!(
        action,
        RollbackAction::WorktreeTrashedAndBranchDeleted { .. }
    ));
    assert_eq!(branches_named(&fixture.repo(), BRANCH)?, 0);
    assert_eq!(linked_worktrees(&fixture.repo())?, 0);
    Ok(())
}

#[test]
fn rollback_refuses_a_commit_a_push_and_a_pull_request() -> TestResult {
    let fixture = Fixture::new("rollback-refusals")?;
    let summary = governed_run(&fixture, true)?;
    let adapter = fixture.adapter();
    let mut git = fixture.service(&adapter)?;
    for receipt in [
        summary.commit.ok_or("no commit")?,
        summary.push.ok_or("no push")?,
        summary.pull_request.ok_or("no pull request")?,
    ] {
        let slot = receipt.slot();
        let refused = git.apply(GitIntent::Rollback { of: receipt });
        assert!(
            matches!(refused, Err(GitEffectError::NotRollbackable)),
            "{slot} must not be rollbackable, got {refused:?}"
        );
    }
    drop(git);
    // A refused rollback is decided before anything is journaled.
    assert!(fixture.row("git-rollback")?.is_none());
    Ok(())
}

// ---------------------------------------------------------------------------
// A worker holds no Git authority
// ---------------------------------------------------------------------------

#[test]
fn the_worker_effect_service_denies_every_git_mutation() -> TestResult {
    let fixture = Fixture::new("worker-authority")?;
    let refusals = Arc::new(Mutex::new(Vec::new()));
    // The helper runs in the repository itself; the point of the case is the
    // effect service the context carries, not the isolation.
    let outcome = dispatch_with_probe(&fixture.repo(), true, Some(Arc::clone(&refusals)))?;
    let recorded = refusals.lock().map_err(|_| "refusals poisoned")?.clone();
    assert_eq!(
        recorded.len(),
        6,
        "every effect kind must have been refused, got {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .all(|kind| *kind == EffectServiceErrorKind::Denied),
        "a worker effect was admitted or failed for another reason: {recorded:?}"
    );
    assert!(
        !outcome.is_completed(),
        "a denied effect is authoritative and must not read as a completed attempt"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The standing offline / no-`gh` negative
// ---------------------------------------------------------------------------

const CHILD_MARKER: &str = "NANIKA_GIT_RUN_E2E_AUDIT";
const ARGV_LOG: &str = "NANIKA_GIT_RUN_E2E_ARGV_LOG";
const GH_MARKER: &str = "NANIKA_GIT_RUN_E2E_GH_MARKER";

/// Re-enters this binary with a `PATH` whose `git` records every argv and whose
/// `gh` writes a marker, runs one whole governed run there, and then audits
/// what the library actually executed.
///
/// `std::env::set_var` is `unsafe` and the workspace forbids `unsafe_code`, so
/// a test cannot shim its own `PATH`; setting it on a child is safe and is what
/// makes this an observation rather than an assertion of intent.
#[test]
fn no_network_and_no_pull_request_provider_is_contacted() -> TestResult {
    if std::env::var_os(CHILD_MARKER).is_some() {
        return audit_child();
    }

    let harness = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
        "orchestrator-rs-git-run-audit-{}",
        std::process::id()
    ));
    private_dir(&harness)?;
    let shims = harness.join("shims");
    private_dir(&shims)?;
    let log = harness.join("git-argv.log");
    let marker = harness.join("gh-invoked");

    // Both destinations are baked into the scripts rather than read from the
    // environment: the library clears the child environment down to an audited
    // allowlist, so a shim that looked its log path up in `$SOMETHING` would
    // silently write nowhere and every assertion below would pass vacuously.
    write_shim(
        &shims.join("git"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            str(&log),
            str(&real_git())
        ),
    )?;
    write_shim(
        &shims.join("gh"),
        &format!("#!/bin/sh\nprintf 'gh\\n' >> '{}'\nexit 1\n", str(&marker)),
    )?;

    let output = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "no_network_and_no_pull_request_provider_is_contacted",
            "--nocapture",
        ])
        .env(CHILD_MARKER, "1")
        .env(ARGV_LOG, &log)
        .env(GH_MARKER, &marker)
        .env("PATH", format!("{}:/usr/bin:/bin", shims.display()))
        .output()?;

    let recorded = std::fs::read_to_string(&log).unwrap_or_default();
    let gh_ran = marker.exists();
    let _ = std::fs::remove_dir_all(&harness);

    assert!(
        output.status.success(),
        "the audited child run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!gh_ran, "the run spawned gh");

    // The shim must actually have captured something, or a silent no-op would
    // pass every assertion below.
    for shape in [
        "fetch",
        "branch",
        "worktree add",
        "commit",
        "push",
        "ls-remote",
    ] {
        assert!(
            recorded.lines().any(|line| line.contains(shape)),
            "the argv recorder captured no {shape:?} command; log was:\n{recorded}"
        );
    }
    for line in recorded.lines() {
        assert!(
            !line.contains("://"),
            "a git invocation named a URL scheme: {line}"
        );
        assert!(
            !scp_like(line),
            "a git invocation named a remote host: {line}"
        );
    }
    Ok(())
}

/// git's own scp-like remote shorthand, detected the way git detects it: a
/// colon appearing before the first `/` in a token.
fn scp_like(line: &str) -> bool {
    line.split_whitespace().any(|token| {
        if token.starts_with('-') {
            return false;
        }
        token
            .find(':')
            .is_some_and(|colon| !token[..colon].is_empty() && !token[..colon].contains('/'))
    })
}

fn write_shim(path: &Path, script: &str) -> TestResult {
    std::fs::write(path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The audited half: one whole governed run, executed with the shim `PATH`.
fn audit_child() -> TestResult {
    let fixture = Fixture::new("audited")?;
    let summary = governed_run(&fixture, true)?;
    assert!(summary.pull_request.is_some());
    let GitReceipt::PrOpened { url, .. } = summary.pull_request.ok_or("no pull request")? else {
        return Err("expected a pull-request receipt".into());
    };
    assert!(url.starts_with("fixture://"));
    Ok(())
}

// ---------------------------------------------------------------------------
// A resolved slot's key is reconstructed, not stored
// ---------------------------------------------------------------------------

#[test]
fn every_slot_lands_exactly_one_durable_row() -> TestResult {
    let fixture = Fixture::new("row-shape")?;
    let _summary = governed_run(&fixture, true)?;
    let mut states = BTreeMap::new();
    for slot in [
        SLOT_REFRESH_BASE,
        SLOT_CREATE_ISOLATION,
        SLOT_COMMIT,
        SLOT_PUSH,
        SLOT_OPEN_PR,
        SLOT_CLEANUP,
    ] {
        let row = fixture.row(slot)?.ok_or(format!("no row for {slot}"))?;
        assert_eq!(row.attempts, 1, "{slot} was claimed more than once");
        assert_eq!(row.identities, 1, "{slot} recorded a wrong identity count");
        states.insert(slot, row.state);
    }
    assert!(
        states.values().all(|state| state == "succeeded"),
        "every slot must succeed on a clean run, got {states:?}"
    );
    Ok(())
}
