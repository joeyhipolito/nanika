//! B5-DESIGN §4, Gate 1c — one authored two-phase mission through the exact
//! `orchestrator run` entrypoint.
//!
//! The argv vector is the one a user types: `run <mission>.md`. It goes through
//! `orchestrator_cli::run_system_with_enrollment`, which is
//! [`orchestrator_cli::run_system`] with §1.1's enrollment parameter exposed —
//! same `split_root_command`, same `run::parse`, same `validate_invocation`,
//! same `composition::seal` call site, same `run::render`. The gate differs
//! from the shipped binary in exactly one respect: it passes
//! `Some(&enrollment)` where `run_system` passes `None`.
//!
//! A3 also spawns the shipped binary with the *same* argv, so "the entrypoint,
//! not the seam" is asserted against a real process as well as against the
//! library path, and A5 asserts that that process — which can only pass `None`
//! — refuses with `ExecutionNotEnrolled` and writes nothing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use orchestrator_app::{
    FixtureAdmissionPolicy, FreshFixtureAuthority, GitEffectCapability, IsolatedFixtureRoot,
};
use orchestrator_cli::{
    FixtureEnrollment, FixtureGitEnrollment, GitRunPlan, RunFlags, run_system_with_enrollment, seal,
};
use orchestrator_core::{MissionId, PhaseId};
use orchestrator_git::FixturePrAdapter;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURE_RUNTIME: &str = "codex";
const HELPER: &str = "attested-helper";
const HELPER_ARGUMENTS: &[&str] = &["--exact", "helper_quick_entrypoint", "--nocapture"];
const REMOTE: &str = "origin";

static CASE: AtomicU64 = AtomicU64::new(1);

/// The authored mission every case runs: two phases, one `DEPENDS` edge.
const MISSION_SOURCE: &str = concat!(
    "# An authored two-phase mission\n",
    "\n",
    "PHASE: build | OBJECTIVE: run the attested helper | RUNTIME: codex\n",
    "PHASE: verify | OBJECTIVE: run the attested helper again | RUNTIME: codex | DEPENDS: build\n",
);

// ---------------------------------------------------------------------------
// Fixture harness
// ---------------------------------------------------------------------------

struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
    authority: FreshFixtureAuthority,
    helper: Vec<u8>,
    mission: MissionId,
    mission_file: PathBuf,
    git_capability: GitEffectCapability,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-b5-authored-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        // The attested helper is this test binary: a thin, in-tree build
        // artifact whose exact bytes the admission policy pins. `--exact
        // helper_quick_entrypoint` returns immediately and writes nothing.
        let helper_path = std::env::current_exe()?;
        let helper = std::fs::read(&helper_path)?;
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
            .with_expected_fixture_helper(&helper);

        let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
        let path = fresh.path().to_path_buf();
        let git_capability = GitEffectCapability::in_fixture(&fresh, &[REMOTE])?;
        let authority = FreshFixtureAuthority::admit(fresh, &policy)?;
        let root = IsolatedFixtureRoot::identify(&path)?;

        let mission_file = parent.join("mission.md");
        std::fs::write(&mission_file, MISSION_SOURCE)?;

        Ok(Self {
            parent,
            root,
            authority,
            helper,
            mission: MissionId::new(format!("b5-authored-{number}"))?,
            mission_file,
            git_capability,
        })
    }

    /// The exact argv a user types.
    fn argv(&self) -> Vec<String> {
        vec![
            "run".to_owned(),
            self.mission_file.to_string_lossy().into_owned(),
        ]
    }

    fn enrollment(&self) -> TestResult<FixtureEnrollment<'_>> {
        Ok(FixtureEnrollment {
            root: &self.root,
            authority: &self.authority,
            helper_label: HELPER,
            helper_bytes: &self.helper,
            helper_arguments: HELPER_ARGUMENTS,
            mission: self.mission.clone(),
            phase: PhaseId::new("phase-1")?,
            runtime: FIXTURE_RUNTIME,
            git: None,
            knowledge: None,
            evidence: None,
        })
    }

    fn events(&self) -> Vec<(String, String)> {
        let path = self
            .root
            .path()
            .join("events")
            .join(format!("{}.jsonl", self.mission.as_str()));
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .map(|value| {
                (
                    string_field(&value, "type"),
                    string_field(&value, "phase_id"),
                )
            })
            .collect()
    }

    /// Ordered `missions` rows, as `(id, status, phases_total, phases_completed)`.
    fn mission_rows(&self) -> TestResult<Vec<(String, String, i64, i64)>> {
        let database = self.root.path().join("metrics.db");
        if !database.exists() {
            return Ok(Vec::new());
        }
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement = connection.prepare(
            "SELECT id, status, phases_total, phases_completed FROM missions ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        Ok(collected)
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
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

/// Drives the argv through the entrypoint with the enrollment supplied.
fn run_entrypoint(fixture: &Fixture, enrollment: &FixtureEnrollment<'_>) -> TestResult<String> {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    run_system_with_enrollment(fixture.argv(), &mut output, &mut errors, Some(enrollment))?;
    Ok(String::from_utf8(output)?)
}

// ---------------------------------------------------------------------------
// A1 — the authored plan runs to terminal in dependency order
// ---------------------------------------------------------------------------

#[test]
fn a1_an_authored_two_phase_mission_runs_to_terminal_in_dependency_order() -> TestResult {
    let fixture = Fixture::new("a1")?;
    let enrollment = fixture.enrollment()?;
    let rendered = run_entrypoint(&fixture, &enrollment)?;

    // The rendered plan names both phases and the mission file, so the run
    // went through `run::render` on the resolved plan, not through a shortcut.
    assert!(
        rendered.contains("phases: 2 (sequential)"),
        "the entrypoint must render the authored plan, observed {rendered:?}"
    );
    assert!(rendered.contains("mission: "), "observed {rendered:?}");
    assert!(
        rendered.contains("execution: enrolled"),
        "an enrolled run that dispatched its phases must not report itself as \
         planning-only: {rendered:?}"
    );

    // Both phases reached a terminal outcome, in `DEPENDS` order.
    assert_eq!(
        fixture.events(),
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
            ("worker.spawned".to_owned(), "phase-2".to_owned()),
            ("worker.completed".to_owned(), "phase-2".to_owned()),
        ],
        "phase order must match DEPENDS, and both phases must terminate"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A2 — the durable state after A1
// ---------------------------------------------------------------------------

#[test]
fn a2_the_durable_state_is_compared_as_ordered_rows() -> TestResult {
    let fixture = Fixture::new("a2")?;
    let enrollment = fixture.enrollment()?;
    let _rendered = run_entrypoint(&fixture, &enrollment)?;

    // The canonical event sequence, as ordered rows.
    assert_eq!(
        fixture.events(),
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
            ("worker.spawned".to_owned(), "phase-2".to_owned()),
            ("worker.completed".to_owned(), "phase-2".to_owned()),
        ]
    );

    // The `metrics.db` mission row, as an ordered row rather than a count.
    // `phases_total`/`phases_completed` are derived from the `phases` table,
    // which this run does not populate: a phase row needs a reaping witness
    // (`PhaseMetricIntent::new`), and only the observer of the phase's process
    // group can produce one. The mission row is what `SealedRun::execute`
    // itself writes.
    assert_eq!(
        fixture.mission_rows()?,
        vec![(
            fixture.mission.as_str().to_owned(),
            "completed".to_owned(),
            0,
            0
        )]
    );
    Ok(())
}

#[test]
fn a2_a_planned_git_effect_leaves_ordered_receipts() -> TestResult {
    let fixture = Fixture::new("a2-git")?;
    let repository = GitRepository::new(&fixture)?;
    let adapter = repository.adapter();
    let enrollment = FixtureEnrollment {
        git: Some(FixtureGitEnrollment {
            capability: &fixture.git_capability,
            adapter: &adapter,
            repo_root: repository.repo.clone(),
            plan: GitRepository::plan(),
        }),
        ..fixture.enrollment()?
    };
    let _rendered = run_entrypoint(&fixture, &enrollment)?;

    // The governed seam is bound to one phase, so exactly one isolation branch
    // exists on the remote beside `main`, and exactly one pull request was
    // opened — compared as ordered rows, not as counts.
    let branches = repository.remote_branches()?;
    assert_eq!(
        branches
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["main", repository.isolation_branch(&fixture).as_str()],
        "observed {branches:?}"
    );
    assert_eq!(repository.receipts(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// A3 — the entrypoint, not the seam
// ---------------------------------------------------------------------------

#[test]
fn a3_the_run_goes_through_the_orchestrator_run_entrypoint() -> TestResult {
    let fixture = Fixture::new("a3")?;

    // The argv is exactly what a user types, and it is the same vector the
    // shipped binary is spawned with below.
    let argv = fixture.argv();
    assert_eq!(argv[0], "run");
    assert!(argv[1].ends_with("mission.md"));

    let enrollment = fixture.enrollment()?;
    let _rendered = run_entrypoint(&fixture, &enrollment)?;
    assert_eq!(fixture.events().len(), 4);

    // The same argv against the shipped binary: `argv[0]` is named, and it is
    // the `orchestrator` binary this workspace builds.
    let binary = std::fs::canonicalize(env!("CARGO_BIN_EXE_orchestrator"))?;
    assert_eq!(
        binary.file_name().and_then(|name| name.to_str()),
        Some("orchestrator"),
        "the spawned entrypoint must be the shipped binary, observed {binary:?}"
    );
    let output = Command::new(&binary).args(&argv).output()?;
    assert!(
        !output.status.success(),
        "the shipped binary passes None and must refuse this run"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A4 — replay is idempotent
// ---------------------------------------------------------------------------

#[test]
fn a4_replaying_the_same_authored_mission_adds_no_duplicate_row() -> TestResult {
    let fixture = Fixture::new("a4")?;
    {
        let enrollment = fixture.enrollment()?;
        let _first = run_entrypoint(&fixture, &enrollment)?;
    }
    let after_first = fixture.mission_rows()?;
    assert_eq!(after_first.len(), 1);

    {
        let enrollment = fixture.enrollment()?;
        let _second = run_entrypoint(&fixture, &enrollment)?;
    }
    assert_eq!(
        fixture.mission_rows()?,
        after_first,
        "the mission row is keyed on the mission id, so a replay upserts it"
    );
    // The replay is a second attempt, so it appends its own events; what must
    // not happen is a second mission row or a second phase row.
    let events = fixture.events();
    assert_eq!(
        events.len(),
        8,
        "the replay's own attempt is appended, not merged: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|(kind, _)| kind == "worker.completed")
            .count(),
        4
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A6 — CF-M4a-4: the checkpoint is Go-readable at every instant
// ---------------------------------------------------------------------------

/// The seeded checkpoint carries the empty placeholder plan, the executed run
/// replaces it with the authored phases, and a third plan cannot be published
/// over either.
///
/// Go's `internal/cmd/status.go:63` dereferences `cp.Plan` with no nil check,
/// so "the field is optional" is not what the reader that matters believes. The
/// composition root admits the workspace at seal time and resolves the plan
/// afterwards, so the two states are seeded-placeholder and authored — and both
/// must be readable. `go_rust_upgrade_rollback_e2e` proves the frozen oracle
/// actually reads the second one; this case pins the shape and the monotonicity
/// without needing the oracle.
#[test]
fn a6_the_seeded_checkpoint_is_go_readable_before_and_after_the_authored_plan() -> TestResult {
    let fixture = Fixture::new("a6")?;
    let checkpoint = fixture
        .root
        .path()
        .join("workspaces")
        .join(fixture.mission.as_str())
        .join("checkpoint.json");

    // Seal alone: the workspace is admitted, nothing has run.
    let enrollment = fixture.enrollment()?;
    let sealed = seal(&RunFlags::default(), Some(&enrollment))?;
    let seeded = orchestrator_core::decode_checkpoint(&std::fs::read(&checkpoint)?)?.projection;
    let placeholder = seeded
        .plan
        .as_ref()
        .ok_or("the seeded checkpoint has no plan, so a crash before dispatch crashes Go")?;
    assert!(
        placeholder.phases.is_empty() && placeholder.id.is_empty(),
        "the seed published something other than the empty placeholder: {placeholder:?}",
    );
    drop(sealed);

    // The run publishes the authored phases over that placeholder.
    {
        let enrollment = fixture.enrollment()?;
        let _report = run_entrypoint(&fixture, &enrollment)?;
    }
    let published = orchestrator_core::decode_checkpoint(&std::fs::read(&checkpoint)?)?.projection;
    let plan = published
        .plan
        .as_ref()
        .ok_or("the executed run left the checkpoint plan-less")?;
    assert_eq!(plan.id, fixture.mission.as_str());
    assert_eq!(
        plan.phases
            .iter()
            .map(|phase| (phase.id.as_str(), phase.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("phase-1", "build"), ("phase-2", "verify")],
        "the published plan is not the authored one",
    );
    assert_eq!(plan.decomp_source, "authored");

    // A *different* plan over an authored one is refused, and changes nothing.
    let before = std::fs::read(&checkpoint)?;
    let workspace = fixture
        .authority
        .open_workspace(fixture.mission.clone())
        .map_err(|error| format!("the admitted workspace should reopen: {error}"))?;
    let refusal = workspace
        .publish_authored_plan(&orchestrator_core::CheckpointPlan {
            id: "a-different-mission".to_owned(),
            phases: vec![orchestrator_core::CheckpointPhase::default()],
            ..orchestrator_core::CheckpointPlan::default()
        })
        .err()
        .ok_or("a second, different plan must be refused, not published")?;
    assert!(
        refusal
            .to_string()
            .contains("already carries an authored plan"),
        "unexpected refusal {refusal}",
    );
    assert_eq!(
        std::fs::read(&checkpoint)?,
        before,
        "the refused publish still wrote to the checkpoint",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A5 — `run_not_enrolled` is not contradicted
// ---------------------------------------------------------------------------

#[test]
fn a5_the_shipped_binary_refuses_the_same_authored_mission_and_writes_nothing() -> TestResult {
    let fixture = Fixture::new("a5")?;

    // Neither `--offline` nor `--dry-run`: the ordinary path, through the
    // process that can only pass `None`.
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(fixture.argv())
        .env("NANIKA_RUN_EXECUTE_DEV", "1")
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not enrolled"),
        "the refusal must be the enrollment gate, observed {stderr}"
    );

    // It wrote nothing into the fixture home.
    assert!(
        fixture.events().is_empty(),
        "a refused run must append no worker event"
    );
    assert!(fixture.mission_rows()?.is_empty());
    assert!(
        !fixture.root.path().join("metrics.db").exists(),
        "a refused run must not create metrics.db"
    );

    // And `--dry-run` through the same process still renders the plan, which
    // is the behaviour `run_not_enrolled.rs` and the dry-run gates rely on.
    let mut dry = fixture.argv();
    dry.push("--dry-run".to_owned());
    let rendered = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(&dry)
        .output()?;
    assert!(rendered.status.success());
    let stdout = String::from_utf8_lossy(&rendered.stdout);
    assert!(
        stdout.contains("phases: 2 (sequential)"),
        "observed {stdout}"
    );
    assert!(
        stdout.contains("execution: dry-run"),
        "the shipped binary's dry run is unchanged by the flip: {stdout}"
    );
    assert!(
        fixture.events().is_empty(),
        "a dry run must still append no worker event"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The Git fixture A2's second case uses
// ---------------------------------------------------------------------------

struct GitRepository {
    repo: PathBuf,
    bare: PathBuf,
    ledger: PathBuf,
}

impl GitRepository {
    fn new(fixture: &Fixture) -> TestResult<Self> {
        let root = fixture.root.path();
        let bare = root.join("origin.git");
        git(
            &fixture.parent,
            &["init", "--bare", "--initial-branch=main", text(&bare)],
        )?;
        let publisher = root.join("publisher");
        git(&fixture.parent, &["clone", text(&bare), text(&publisher)])?;
        identify(&publisher)?;
        std::fs::write(publisher.join("README.md"), b"base\n")?;
        git(&publisher, &["add", "-A"])?;
        git(&publisher, &["commit", "-m", "base"])?;
        git(&publisher, &["push", REMOTE, "main"])?;

        let repo = root.join("work");
        git(&fixture.parent, &["clone", text(&bare), text(&repo)])?;
        identify(&repo)?;
        git(&repo, &["remote", "set-url", REMOTE, text(&bare)])?;
        let ledger = root.join("pr-ledger");
        std::fs::create_dir_all(&ledger)?;
        Ok(Self { repo, bare, ledger })
    }

    fn adapter(&self) -> FixturePrAdapter {
        FixturePrAdapter::new(self.bare.clone(), self.ledger.clone())
    }

    fn plan() -> GitRunPlan {
        GitRunPlan {
            remote: REMOTE.to_owned(),
            base_branch: "main".to_owned(),
            task: "authored-run".to_owned(),
            commit_message: "authored run".to_owned(),
            pr_title: "authored run".to_owned(),
            pr_body: "opened by the sealed composition root".to_owned(),
            pr_draft: true,
            trash_component: "trash".to_owned(),
        }
    }

    fn isolation_branch(&self, fixture: &Fixture) -> String {
        format!("via/{}/authored-run", fixture.mission.as_str())
    }

    fn remote_branches(&self) -> TestResult<Vec<(String, String)>> {
        let mut branches = Vec::new();
        for line in git(
            &self.bare,
            &[
                "for-each-ref",
                "--format=%(refname:short) %(objectname)",
                "refs/heads",
            ],
        )?
        .lines()
        .filter(|line| !line.trim().is_empty())
        {
            let mut parts = line.split_whitespace();
            branches.push((
                parts.next().unwrap_or_default().to_owned(),
                parts.next().unwrap_or_default().to_owned(),
            ));
        }
        branches.sort();
        Ok(branches)
    }

    fn receipts(&self) -> usize {
        std::fs::read_dir(&self.ledger)
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or_default()
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
}

fn git(dir: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn identify(repo: &Path) -> TestResult {
    git(repo, &["config", "user.email", "fixture@example.invalid"])?;
    git(repo, &["config", "user.name", "Fixture"])?;
    Ok(())
}

/// The attested helper's entry point: it returns at once and writes nothing.
///
/// Reached only when this binary is installed as the fixture helper and run
/// with [`HELPER_ARGUMENTS`]; an ordinary run of this gate executes it as an
/// ordinary (empty) test.
#[test]
fn helper_quick_entrypoint() {}
