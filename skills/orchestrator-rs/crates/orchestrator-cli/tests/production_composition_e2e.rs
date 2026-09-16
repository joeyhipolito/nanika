//! B5-DESIGN §4, Gate 1a — the composition root, positively and negatively.
//!
//! Every case reaches `composition::seal` the same way `run_system` does, and
//! differs from it in exactly one respect: it passes `Some(&enrollment)` where
//! `run_system` passes `None`. Nothing here reads an environment variable to
//! select an authority, and nothing constructs an authority
//! `orchestrator-app` does not already gate — the enrollment is assembled from
//! `IsolatedFixtureRoot::create_fresh`, `FreshFixtureAuthority::admit` under a
//! `FixtureAdmissionPolicy` built `with_expected_fixture_helper`, and B4's
//! fixture `PrAdapter`.
//!
//! The attested helper is the shipped `orchestrator` binary itself, pinned by
//! its exact bytes through `CARGO_BIN_EXE_orchestrator` and run with `--help`:
//! a real native executable that this workspace builds, reaches no network,
//! writes nothing, and terminates deterministically. B5-DESIGN §5.1 declines
//! K0.1B's three helper mains as superseded, and this gate adds none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orchestrator_app::{
    AppKnowledgeGateway, DispatchDecisionRequest, ExactProcessGroupAbsence, FixedAuthorityInputs,
    FixtureAdmissionPolicy, FreshFixtureAuthority, GitEffectCapability, IsolatedFixtureRoot,
    MetricsOwnerCapability, OwnerLeaseError, PhaseMetricIntent, RouteIdentity,
    RoutingDispatchError, RuntimeStoreError, UsageMultiplexerPolicy, fixture_production_boundary,
};
use orchestrator_cli::{
    ExecutionEnrollment, FixtureEnrollment, FixtureEvidenceEnrollment, FixtureGitEnrollment,
    GitRunPlan, PersistentFlags, ResolvedRun, RunFlags, SealedRun, SealedRunError,
    UnenrolledReason, resolve_with_context, seal,
};
use orchestrator_core::{
    AdaptiveCandidateV1, AdaptiveEvaluationModeV1, AdaptivePolicyV1, AutomaticEligibilityV1,
    BasisPointsV1, CandidateIdV1, DurationMillisV1, MissionId, ModelTier, PhaseId, ProviderIdV1,
    ProviderReserveV1, QualityTierV1, RouteRequirementsV1, RouteTargetV1, RoutingMap, RuntimeIdV1,
    RuntimeResolutionInput, ScoreWeightsV1, TaskPriorityV1, UnknownUsagePolicyV1, UsageAccountIdV1,
    UtcMillisV1,
};
use orchestrator_git::FixturePrAdapter;
use orchestrator_knowledge::{
    Delegation, FieldMask, KnowledgeCapability, Namespace, Operation, PrimitiveKind, SchemaVersion,
    Sensitivity, TypeManifest, TypeName, TypeRegistry, Validity,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The runtime family the one local fixture executor is registered under.
///
/// It is a supported Go-compatible family (so an authored `RUNTIME:` line
/// survives `orchestrator_core::resolve_runtime`'s clamp) and it has no live
/// executor anywhere in this workspace.
const FIXTURE_RUNTIME: &str = "codex";
const HELPER: &str = "attested-helper";
const REMOTE: &str = "origin";

static CASE: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Fixture harness
// ---------------------------------------------------------------------------

/// One disposable fixture: a private parent directory holding an isolated
/// root, an admitted authority over it, and the exact helper bytes the
/// admission policy pinned.
struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
    authority: FreshFixtureAuthority,
    helper: Vec<u8>,
    mission: MissionId,
    /// Minted from the *freshly created* handle, before `admit` consumed it:
    /// `GitEffectCapability::in_fixture` refuses an adopted root, so the Git
    /// capability has to be taken while the root's provenance is still fresh.
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
            "orchestrator-rs-b5-composition-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        let helper = std::fs::read(env!("CARGO_BIN_EXE_orchestrator"))?;
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
            .with_expected_fixture_helper(&helper);

        let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
        let path = fresh.path().to_path_buf();
        let git_capability = GitEffectCapability::in_fixture(&fresh, &[REMOTE])?;
        let authority = FreshFixtureAuthority::admit(fresh, &policy)?;
        // `admit` consumed the freshly created handle; `identify` yields the
        // second handle seals 2, 3 and 9 take, over the same canonical path.
        let root = IsolatedFixtureRoot::identify(&path)?;

        Ok(Self {
            parent,
            root,
            authority,
            helper,
            mission: MissionId::new(format!("b5-composition-{number}"))?,
            git_capability,
        })
    }

    /// The enrollment `run_system` cannot produce and this gate can.
    fn enrollment(&self) -> TestResult<FixtureEnrollment<'_>> {
        Ok(FixtureEnrollment {
            root: &self.root,
            authority: &self.authority,
            helper_label: HELPER,
            helper_bytes: &self.helper,
            helper_arguments: &["--help"],
            mission: self.mission.clone(),
            phase: phase_one()?,
            runtime: FIXTURE_RUNTIME,
            git: None,
            knowledge: None,
            evidence: None,
        })
    }
}

/// The first authored phase's identity.
///
/// `parse_authored_phases` names authored phases `phase-1`, `phase-2`, ... in
/// source order, so this is what an enrollment binds Git and evidence to.
fn phase_one() -> TestResult<PhaseId> {
    Ok(PhaseId::new("phase-1")?)
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

/// An ordinary run: neither `--dry-run` nor `--offline`, so it is exactly the
/// invocation `lib.rs` used to refuse unconditionally.
fn ordinary_flags() -> RunFlags {
    RunFlags {
        persistent: PersistentFlags {
            dry_run: false,
            ..PersistentFlags::default()
        },
        offline: false,
        ..RunFlags::default()
    }
}

fn argv(task: &str) -> Vec<String> {
    vec![task.to_owned()]
}

fn resolve(sealed: &SealedRun<'_>, task: &str) -> TestResult<ResolvedRun> {
    Ok(resolve_with_context(&argv(task), sealed.resolution())?)
}

/// One authored phase pinned to the enrolled runtime family.
fn one_phase_mission() -> String {
    format!("PHASE: build | OBJECTIVE: run the attested helper | RUNTIME: {FIXTURE_RUNTIME}\n")
}

/// Two authored phases with a `DEPENDS` edge from the second to the first.
fn two_phase_mission() -> String {
    format!(
        "PHASE: build | OBJECTIVE: run the attested helper | RUNTIME: {FIXTURE_RUNTIME}\n\
         PHASE: verify | OBJECTIVE: run the attested helper again | RUNTIME: {FIXTURE_RUNTIME} | DEPENDS: build\n"
    )
}

/// Reads the canonical event log this mission wrote, as `(type, phase)` rows.
fn canonical_events(fixture: &Fixture) -> TestResult<Vec<(String, String)>> {
    let path = fixture
        .root
        .path()
        .join("events")
        .join(format!("{}.jsonl", fixture.mission.as_str()));
    let text = std::fs::read_to_string(path)?;
    let mut rows = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)?;
        rows.push((
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            value
                .get("phase_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ));
    }
    Ok(rows)
}

/// Counts rows in one `metrics.db` table for this mission.
fn metrics_rows(fixture: &Fixture, table: &str, column: &str) -> TestResult<i64> {
    let connection = rusqlite::Connection::open_with_flags(
        fixture.root.path().join("metrics.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let sql = format!("SELECT count(*) FROM {table} WHERE {column} = ?1");
    let count: i64 = connection.query_row(&sql, [fixture.mission.as_str()], |row| row.get(0))?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// P1 — one fixture-backed ordinary run reaches a terminal outcome
// ---------------------------------------------------------------------------

#[test]
fn p1_one_fixture_backed_ordinary_run_reaches_a_terminal_outcome() -> TestResult {
    let fixture = Fixture::new("p1")?;
    let enrollment = fixture.enrollment()?;
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;

    let resolved = resolve(&sealed, &one_phase_mission())?;
    assert_eq!(resolved.phases.len(), 1);
    assert_eq!(resolved.phases[0].effective_runtime, FIXTURE_RUNTIME);
    assert!(
        sealed.enrollment_for_plan(&resolved).is_enrolled(),
        "an enrolled fixture runtime must resolve to the sealed executor"
    );

    let report = sealed.execute(&resolved)?;
    assert_eq!(report.phases.len(), 1);
    assert!(
        report.phases[0].result.outcome().is_completed(),
        "the attested helper must reach a terminal completed outcome"
    );
    assert!(report.phases[0].result.git().is_none());

    // Exactly one `worker.spawned` and one terminal `worker.completed`, on the
    // seal-4 canonical log.
    assert_eq!(
        canonical_events(&fixture)?,
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
        ]
    );

    // One `missions` row, written by seal 10 through `SealedRun::execute`.
    assert_eq!(metrics_rows(&fixture, "missions", "id")?, 1);

    // One `phases` row, recorded through the same sealed writer. The reaping
    // witness is the caller's because `PhaseMetricIntent::new` demands proof
    // the phase's process group is gone, and only its observer has that.
    let mut intent =
        PhaseMetricIntent::new(fixture.mission.clone(), "phase-1", 1, reaped_witness()?);
    intent.persona = resolved.phases[0].persona.clone();
    intent.provider = FIXTURE_RUNTIME.to_owned();
    sealed.record_phase(&intent)?;
    assert_eq!(metrics_rows(&fixture, "phases", "mission_id")?, 1);
    let recorded = sealed
        .metrics()
        .ok_or("the sealed root must own metrics")?
        .recorded_phase_names(fixture.mission.as_str())?;
    assert_eq!(recorded, vec!["phase-1".to_owned()]);
    Ok(())
}

// ---------------------------------------------------------------------------
// P2 — the same run with a planned Git effect
// ---------------------------------------------------------------------------

/// A fixture repository whose only remote is a local bare repository.
struct GitFixture {
    repo: PathBuf,
    bare: PathBuf,
    ledger: PathBuf,
}

impl GitFixture {
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
            task: "sealed-git-run".to_owned(),
            commit_message: "sealed git run".to_owned(),
            pr_title: "sealed git run".to_owned(),
            pr_body: "opened by the sealed composition root".to_owned(),
            pr_draft: true,
            trash_component: "trash".to_owned(),
        }
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
}

fn git(dir: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = std::process::Command::new("git")
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

/// What the repository looks like after a run, so "zero duplicates" is an
/// observation of the repository rather than of a return value.
#[derive(Debug, Eq, PartialEq)]
struct GitShape {
    /// Every branch on the remote, with the sha it points at.
    remote_branches: Vec<(String, String)>,
    /// Pull-request receipts the offline adapter wrote.
    receipts: usize,
}

fn git_shape(repository: &GitFixture) -> TestResult<GitShape> {
    let mut remote_branches = Vec::new();
    for line in git(
        &repository.bare,
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
        let name = parts.next().unwrap_or_default().to_owned();
        let sha = parts.next().unwrap_or_default().to_owned();
        remote_branches.push((name, sha));
    }
    remote_branches.sort();
    let receipts = std::fs::read_dir(&repository.ledger)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or_default();
    Ok(GitShape {
        remote_branches,
        receipts,
    })
}

/// Live worktree directories beneath the fixture root.
fn worktree_count(fixture: &Fixture) -> usize {
    std::fs::read_dir(fixture.root.path().join("worktrees"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or_default()
}

#[test]
fn p2_a_planned_git_effect_produces_receipts_and_replay_adds_nothing() -> TestResult {
    let fixture = Fixture::new("p2")?;
    let repository = GitFixture::new(&fixture)?;
    let adapter = repository.adapter();

    let shape = {
        let enrollment = FixtureEnrollment {
            git: Some(FixtureGitEnrollment {
                capability: &fixture.git_capability,
                adapter: &adapter,
                repo_root: repository.repo.clone(),
                plan: GitFixture::plan(),
            }),
            ..fixture.enrollment()?
        };
        let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
        assert_eq!(sealed.git_phase(), Some(&phase_one()?));
        let resolved = resolve(&sealed, &one_phase_mission())?;
        let report = sealed.execute(&resolved)?;

        let receipts = report.phases[0]
            .result
            .git()
            .ok_or("a planned Git effect must produce receipts")?;
        assert!(report.phases[0].result.outcome().is_completed());
        assert!(
            receipts.commit.is_some(),
            "verified work must produce a commit receipt"
        );
        assert!(receipts.push.is_some(), "a commit must be pushed");
        assert!(
            receipts.pull_request.is_some(),
            "an acknowledged push must open a pull request"
        );
        assert!(receipts.cleanup.is_some());
        assert!(
            worktree_count(&fixture) <= 1,
            "one run must leave at most one isolation worktree"
        );
        git_shape(&repository)?
    };
    assert_eq!(
        shape.receipts, 1,
        "one governed run opens exactly one pull request"
    );
    assert_eq!(
        shape.remote_branches.len(),
        2,
        "the remote holds main plus exactly one isolation branch, observed {:?}",
        shape.remote_branches
    );

    // Replay: a second seal over the same durable ledger reconciles rather
    // than re-applying. The shape of the repository must not move.
    {
        let enrollment = FixtureEnrollment {
            git: Some(FixtureGitEnrollment {
                capability: &fixture.git_capability,
                adapter: &adapter,
                repo_root: repository.repo.clone(),
                plan: GitFixture::plan(),
            }),
            ..fixture.enrollment()?
        };
        let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
        let resolved = resolve(&sealed, &one_phase_mission())?;
        let _report = sealed.execute(&resolved)?;
    }
    assert_eq!(
        git_shape(&repository)?,
        shape,
        "replay must add no branch, move no branch, and open no second pull request"
    );
    assert!(
        worktree_count(&fixture) <= 1,
        "replay must not cut a second isolation worktree"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// P3 — one sealed root across a multi-phase release
// ---------------------------------------------------------------------------

#[test]
fn p3_one_sealed_root_serves_every_phase_of_a_multi_phase_release() -> TestResult {
    let fixture = Fixture::new("p3")?;
    let enrollment = fixture.enrollment()?;
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;

    let boundary_before = sealed
        .boundary()
        .map(std::sync::Arc::as_ptr)
        .ok_or("an enrolled seal must hold seal 2's boundary")?;

    let resolved = resolve(&sealed, &two_phase_mission())?;
    assert_eq!(resolved.phases.len(), 2);
    assert_eq!(
        resolved.phases[1].dependencies,
        vec![resolved.phases[0].id.clone()],
        "the second authored phase must depend on the first"
    );

    let report = sealed.execute(&resolved)?;
    assert_eq!(
        report
            .phases
            .iter()
            .map(|run| run.phase.to_string())
            .collect::<Vec<_>>(),
        vec!["phase-1".to_owned(), "phase-2".to_owned()],
        "dependency order, not authored order by accident"
    );
    assert!(
        report
            .phases
            .iter()
            .all(|run| run.result.outcome().is_completed())
    );

    // Both phases were served by the same seal-2 boundary and the same seal-4
    // event owner: one file, one monotonic sequence, both phases present.
    let boundary_after = sealed
        .boundary()
        .map(std::sync::Arc::as_ptr)
        .ok_or("the boundary must survive the release")?;
    assert_eq!(
        boundary_before, boundary_after,
        "no second seal occurred inside one process"
    );
    assert_eq!(
        canonical_events(&fixture)?,
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
            ("worker.spawned".to_owned(), "phase-2".to_owned()),
            ("worker.completed".to_owned(), "phase-2".to_owned()),
        ]
    );

    // A second `seal` over the same root inside this process cannot succeed
    // while the first is alive — seal 4's writer lease and seal 10's kernel
    // lease are both held.
    let second = fixture.enrollment()?;
    assert!(
        seal(&ordinary_flags(), Some(&second)).is_err(),
        "a second seal must not stand up beside a live one"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// N1 — two seals cannot hold the metrics lease at once
// ---------------------------------------------------------------------------

#[test]
fn n1_two_seals_cannot_hold_the_metrics_lease_at_once() -> TestResult {
    let fixture = Fixture::new("n1")?;
    let enrollment = fixture.enrollment()?;
    let _sealed = seal(&ordinary_flags(), Some(&enrollment))?;

    // The seal holds seal 10's lease, so the same door refuses a second owner
    // with the typed `Held` failure rather than with a generic error.
    let boundary = fixture_production_boundary(&fixture.root)?;
    match MetricsOwnerCapability::in_fixture_boundary(boundary) {
        Err(OwnerLeaseError::Held { .. }) => Ok(()),
        Err(other) => Err(format!("expected a held lease, observed {other:?}").into()),
        Ok(_) => Err("a second metrics owner was admitted beside a live seal".into()),
    }
}

// ---------------------------------------------------------------------------
// N2 — the metrics owner cannot be minted before the boundary
// ---------------------------------------------------------------------------

const COMPOSITION_SOURCE: &str = include_str!("../src/composition.rs");
const LIB_SOURCE: &str = include_str!("../src/lib.rs");

#[test]
fn n2_the_metrics_owner_is_minted_only_from_seal_twos_boundary() {
    // The compile-level proof that `MetricsOwnerCapability` has no
    // path-taking constructor lives in `orchestrator-app`'s
    // `authority_compile_fail` gate, which owns the downstream fixture crate
    // and the captured rustc it needs. What is checkable *here* is the rule
    // this root must obey: seal 10 is minted once, from seal 2's boundary,
    // and the live `under_writer` door is never named in the CLI.
    let mints: Vec<&str> = COMPOSITION_SOURCE
        .lines()
        .filter(|line| line.contains("MetricsOwnerCapability::"))
        .collect();
    assert_eq!(
        mints.len(),
        1,
        "seal 10 must be minted exactly once, observed {mints:?}"
    );
    assert!(
        mints[0].contains("in_fixture_boundary(Arc::clone(&boundary))"),
        "seal 10 must take seal 2's boundary by handle, observed {:?}",
        mints[0]
    );
    // Prose may *name* the live door to say why it is not taken; code may not
    // call it. Comment lines are excluded so the rule is about the branch,
    // not about the vocabulary.
    let code: Vec<&str> = COMPOSITION_SOURCE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .collect();
    for live in ["under_writer", "ProductionWriterAuthority::acquire"] {
        let calls: Vec<&&str> = code.iter().filter(|line| line.contains(live)).collect();
        assert!(
            calls.is_empty(),
            "B5 must not call the live door {live}, observed {calls:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// N3 — a live-provider runtime is refused by the same sealed root
// ---------------------------------------------------------------------------

#[test]
fn n3_a_live_provider_runtime_is_refused_by_the_same_sealed_root() -> TestResult {
    let fixture = Fixture::new("n3")?;
    let enrollment = fixture.enrollment()?;
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;

    // No authored RUNTIME, so the phase resolves to the live provider family.
    let resolved = resolve(
        &sealed,
        "PHASE: ship | OBJECTIVE: reach a live provider | RUNTIME: claude\n",
    )?;
    assert_eq!(resolved.phases[0].effective_runtime, "claude");

    // The refusal is `ExecutorRegistry::resolve` finding nothing — an
    // enrollment *is* present, so this is not the `None` branch.
    match sealed.enrollment_for_plan(&resolved) {
        ExecutionEnrollment::Unenrolled { runtime, reason } => {
            assert_eq!(runtime, "claude");
            assert_eq!(reason, UnenrolledReason::NoExecutorForRuntime);
        }
        ExecutionEnrollment::Enrolled { .. } => {
            return Err("a live provider runtime was enrolled".into());
        }
    }

    // Nothing ran: no spawn, no journal append.
    assert!(sealed.execute(&resolved).is_err());
    let events = fixture
        .root
        .path()
        .join("events")
        .join(format!("{}.jsonl", fixture.mission.as_str()));
    let recorded = std::fs::read_to_string(&events).unwrap_or_default();
    assert!(
        recorded.trim().is_empty(),
        "a refused runtime must append no worker event, observed {recorded:?}"
    );

    // And an enrollment that *names* the live family is refused at the seal.
    let live = FixtureEnrollment {
        runtime: orchestrator_cli::LIVE_PROVIDER_RUNTIME,
        ..fixture.enrollment()?
    };
    assert!(
        seal(&ordinary_flags(), Some(&live)).is_err(),
        "the live provider family has no registration site"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// N4 — `SealedRun`'s drop order releases the metrics lease before the boundary
// ---------------------------------------------------------------------------

#[test]
fn n4_sealed_run_drop_order_releases_the_metrics_lease_before_the_boundary() -> TestResult {
    let fixture = Fixture::new("n4")?;
    {
        let enrollment = fixture.enrollment()?;
        let sealed = seal(&ordinary_flags(), Some(&enrollment))?;
        // While it lives the lease is held, so this is not vacuous.
        let held = fixture_production_boundary(&fixture.root)?;
        assert!(matches!(
            MetricsOwnerCapability::in_fixture_boundary(held),
            Err(OwnerLeaseError::Held { .. })
        ));
        drop(sealed);
    }
    // After the drop a fresh owner succeeds. If `SealedRun` declared seal 2
    // before seal 10, the boundary would be released while the lease it was
    // taken under was still live, and the release below would be observing a
    // different object than the one it was taken from.
    let boundary = fixture_production_boundary(&fixture.root)?;
    let reacquired = MetricsOwnerCapability::in_fixture_boundary(boundary);
    assert!(
        reacquired.is_ok(),
        "dropping the sealed run must release the metrics lease"
    );
    drop(reacquired);

    // The declaration order itself, read from the source, so a reordering is
    // a red test and not a silent behaviour change.
    let metrics_at = COMPOSITION_SOURCE
        .find("    metrics: Option<MetricsOwner>,")
        .ok_or("seal 10's field must exist")?;
    let boundary_at = COMPOSITION_SOURCE
        .find("    boundary: Option<Arc<ProductionBoundary>>,")
        .ok_or("seal 2's field must exist")?;
    assert!(
        metrics_at < boundary_at,
        "SealedRun must declare seal 10 before seal 2 so the lease drops first"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// N5 — no canary-only constructor, no env-selected authority
// ---------------------------------------------------------------------------

#[test]
fn n5_no_canary_only_constructor_and_no_env_selected_authority_is_reachable() {
    // One `seal`, and `run_system` is the only caller in `src/` — with `None`.
    assert_eq!(
        COMPOSITION_SOURCE
            .matches("pub fn seal<'enrolment>(")
            .count(),
        1,
        "there must be exactly one composition root"
    );
    assert_eq!(
        LIB_SOURCE.matches("composition::seal(").count(),
        1,
        "seal must have exactly one call site in src/"
    );
    assert!(
        LIB_SOURCE.contains("composition::seal(&flags, enrollment)"),
        "the one call site must pass the entry point's enrollment through"
    );
    assert!(
        LIB_SOURCE.contains("run_system_with_enrollment(arguments, output, error_output, None)"),
        "run_system must pass None"
    );

    // `ExecutionNotEnrolled` is produced from that branch and nowhere else.
    assert_eq!(
        LIB_SOURCE
            .matches("return Err(CliError::ExecutionNotEnrolled);")
            .count(),
        1,
        "ExecutionNotEnrolled must have exactly one producer"
    );

    // Every fixture door the root calls is cfg-gated, and the root reads no
    // environment variable to choose one.
    // Seals 2-13 are *uncallable* without the fixture doors, not merely
    // refused: in a build with neither `cfg(test)` nor `test-support`,
    // `FixtureEnrollment` has a field of an uninhabited type, so no value of
    // it can exist, `Option::Some(&it)` cannot be produced, and the enrolled
    // branch is an empty match the compiler accepts only because the scrutinee
    // is uninhabited. That pair of definitions is the whole structural claim.
    assert!(
        COMPOSITION_SOURCE.contains(concat!(
            "#[cfg(not(any(test, feature = \"test-support\")))]\n",
            "pub struct FixtureEnrollment<'enrolment> {\n",
            "    never: NoFixtureDoor,",
        )),
        "FixtureEnrollment must be uninhabited without the fixture doors"
    );
    assert!(
        COMPOSITION_SOURCE.contains("enum NoFixtureDoor {}"),
        "the uninhabiting type must have no variants"
    );
    assert!(
        COMPOSITION_SOURCE.contains("    match enrollment.never {}"),
        "the unenrolled profile's seal body must be the compiler's own \
         proof that it is unreachable"
    );
    assert!(
        COMPOSITION_SOURCE.contains(concat!(
            "#[cfg(any(test, feature = \"test-support\"))]\n",
            "pub struct FixtureEnrollment<'enrolment> {",
        )),
        "the inhabited FixtureEnrollment must be gated"
    );
    // Every door the enrolled branch calls is imported only inside the gated
    // import block, so a build without it cannot even name them.
    let gated_imports = COMPOSITION_SOURCE
        .split("#[cfg(any(test, feature = \"test-support\"))]\nuse orchestrator_app::{")
        .nth(1)
        .and_then(|rest| rest.split("};").next())
        .unwrap_or_default();
    for door in [
        "fixture_production_boundary",
        "open_fixture_runtime_store",
        "MetricsOwnerCapability",
    ] {
        assert!(
            gated_imports.contains(door),
            "{door} must be imported only inside the test-support gate"
        );
    }
    for reader in ["std::env::var(", "env::var("] {
        let uses: Vec<&str> = COMPOSITION_SOURCE
            .lines()
            .filter(|line| line.contains(reader))
            .collect();
        assert!(
            uses.is_empty(),
            "the composition root must not read {reader} at all, observed {uses:?}"
        );
    }
    // `optional_path`/`required_path` read `HOME` and friends for seal 1's
    // *path*, never for an authority. Keep that boundary explicit.
    assert!(
        COMPOSITION_SOURCE.contains("fn optional_path(name: &'static str) -> Option<PathBuf>"),
        "seal 1's env reader must stay the only environment surface"
    );
}

// ---------------------------------------------------------------------------
// Seal 12 — the independent evidence authority
// ---------------------------------------------------------------------------

#[test]
fn seal_twelve_binds_the_enrolled_artifact_and_yields_a_verifier() -> TestResult {
    let fixture = Fixture::new("evidence")?;

    // Without an evidence enrollment the reconciler is sealed and the verifier
    // is absent, because `FixtureArtifactEffectService::new` needs the exact
    // expected bytes and a composition root cannot invent them.
    {
        let bare = fixture.enrollment()?;
        let sealed = seal(&ordinary_flags(), Some(&bare))?;
        assert!(sealed.verifier().is_none());
        assert_eq!(sealed.reconciler().sources().len(), 0);
    }

    let enrollment = FixtureEnrollment {
        evidence: Some(FixtureEvidenceEnrollment {
            artifact: "evidence.json",
            expected: b"{\"verified\":true}",
        }),
        ..fixture.enrollment()?
    };
    let sealed = seal(&ordinary_flags(), Some(&enrollment))?;
    assert!(
        sealed.verifier().is_some(),
        "an enrolled artifact must yield the independent verifier"
    );

    // The binding is durable: the phase's artifact directory exists beneath the
    // mission workspace the process authority confines every spawn to.
    let artifacts = fixture
        .root
        .path()
        .join("workspaces")
        .join(fixture.mission.as_str())
        .join("artifacts")
        .join("phase-1");
    assert!(
        artifacts.is_dir(),
        "the artifact binding must create {}",
        artifacts.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Seal 7 and seal 9 — the store is the journal, and the drain is durable
// ---------------------------------------------------------------------------

#[test]
fn seal_nine_drains_the_durable_publication_queue_through_the_sealed_store() -> TestResult {
    let fixture = Fixture::new("drain")?;
    let gateway = knowledge_gateway()?;
    let enrollment = FixtureEnrollment {
        knowledge: Some(&gateway),
        ..fixture.enrollment()?
    };
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
    assert!(
        sealed.knowledge().is_some(),
        "the sealed root must hold the enrolled gateway"
    );
    // An empty queue drains to an empty report rather than to an error, which
    // is what proves the drain is wired to the seal-3 store at all.
    let report = sealed.drain_publications(8, "2026-09-03T00:00:00Z", |_| {
        orchestrator_app::DeliveryVerdict::Accepted
    })?;
    assert!(report.delivered.is_empty() && report.dead_lettered.is_empty());
    Ok(())
}

/// A one-type registry and a grant over it.
///
/// The composition root cannot invent this — the namespace, the manifests and
/// the operation grant are application policy — which is why seal 9's gateway
/// is an enrollment parameter.
fn knowledge_gateway() -> TestResult<AppKnowledgeGateway> {
    let namespace = Namespace::new("b5")?;
    let type_name = TypeName::new("composition-receipt")?;
    let manifest = TypeManifest {
        namespace: namespace.clone(),
        type_name: type_name.clone(),
        kind: PrimitiveKind::Event,
        schema_versions: [SchemaVersion::new(1)?].into_iter().collect(),
        required_fields: std::collections::BTreeSet::new(),
        sensitivity_ceiling: Sensitivity::Internal,
        allowed_operations: [Operation::writing(PrimitiveKind::Event)]
            .into_iter()
            .collect(),
    };
    let registry = TypeRegistry::activate(vec![manifest])?;
    let capability = KnowledgeCapability {
        namespace,
        types: [type_name].into_iter().collect(),
        operations: [Operation::Append].into_iter().collect(),
        field_mask: FieldMask::All,
        sensitivity_ceiling: Sensitivity::Internal,
        validity: Validity::at(registry.generation()),
        delegation: Delegation::Once,
    };
    Ok(AppKnowledgeGateway::new(registry, capability))
}

// ---------------------------------------------------------------------------
// The reaping witness
// ---------------------------------------------------------------------------

/// Spawns this binary in its own process group, reaps it, and returns proof the
/// group is gone. `PhaseMetricIntent::new` cannot be called without one.
fn reaped_witness() -> TestResult<ExactProcessGroupAbsence> {
    use orchestrator_app::{
        KernelProcessIdentity, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    };
    use std::io::BufRead;
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "reaping_witness_entrypoint", "--nocapture"])
        .env("NANIKA_B5_COMPOSITION_WITNESS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    {
        let stdout = child.stdout.take().ok_or("witness stdout")?;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err("the witness exited before announcing readiness".into());
            }
            // libtest with `--test-threads=1` prints `test <name> ... ` and no
            // newline *before* running the test, so the child's readiness token lands
            // at the end of that progress line rather than on a line of its own.
            // Comparing the whole line therefore never matches under the verification
            // lease, which sets RUST_TEST_THREADS=1, and the handshake deadlocks: the
            // parent waits for a line it will never see while the child waits for the
            // stdin EOF the parent sends only after it. The last whitespace-separated
            // token is the token in both of libtest's printing modes.
            if line.split_whitespace().next_back() == Some("ready") {
                break;
            }
        }
    }
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    drop(child.stdin.take());
    child.wait()?;
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Ok(absence),
        other => Err(format!("expected a reaped group, observed {other:?}").into()),
    }
}

/// Re-exec entry point for [`reaped_witness`]. It is inert unless re-executed.
#[test]
fn reaping_witness_entrypoint() {
    if std::env::var("NANIKA_B5_COMPOSITION_WITNESS").is_err() {
        return;
    }
    use std::io::{BufRead, Write};
    println!("ready");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::process::exit(0);
}

/// Seal 7 against the **real** durable journal — and the integration defect
/// that finding exposes.
///
/// `orchestrator-app`'s own `adaptive_routing_e2e` states in its module doc
/// that it cannot reach a real store ("its boundary constructors are
/// crate-internal"), so it routes through a recording double. The sealed root
/// *can*: seal 3 opened the store through the gated fixture door, and
/// `RuntimeStore` is itself the `RoutingDecisionJournal` (`routing.rs:124`).
///
/// Driving that combination for the first time shows it does not work.
/// `RuntimeStore::open` — and therefore `open_fixture_runtime_store` — yields a
/// **compatibility**-kind store, and `PreparedIntent::new`
/// (`runtime_store.rs:9085`) refuses any compatibility transition that
/// declares no `CompatibilityProjection`. `decide_dispatch_route` declares
/// none, so every routing decision is rejected before it is appended.
///
/// **This is not B5's to fix.** B5 wires already-merged services and takes
/// semantic ownership of none of them; inventing a projection here would be
/// fabricating compatibility evidence for a decision that produced none. The
/// case therefore pins the exact refusal, so that the day the projection is
/// supplied (or the rule relaxed) this test goes red and the gap is revisited
/// rather than silently forgotten.
///
/// Nothing on the ordinary run path depends on it: `run_one_phase` resolves
/// through `ExecutorRegistry`, not through `decide_dispatch_route`.
#[test]
fn seal_seven_hands_the_store_in_and_the_compatibility_store_refuses_the_transition() -> TestResult
{
    let fixture = Fixture::new("routing")?;
    let enrollment = fixture.enrollment()?;
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;

    let request = dispatch_request(&fixture.mission)?;
    match sealed.route_dispatch(&request) {
        Err(SealedRunError::Routing(RoutingDispatchError::Journal(
            RuntimeStoreError::InvalidIntent(reason),
        ))) => {
            assert_eq!(
                reason, "compatibility transitions require at least one real projection",
                "the refusal must stay the projection rule; a different reason means the \
                 defect moved and this pin is stale"
            );
        }
        Err(other) => {
            return Err(format!(
                "seal 7 must reach the store's own projection rule, observed {other:?}"
            )
            .into());
        }
        Ok(outcome) => {
            return Err(format!(
                "the compatibility store accepted a projection-less routing transition — \
                 the defect this case pins is fixed, so replace the pin with the positive \
                 assertions: {outcome:?}"
            )
            .into());
        }
    }

    // The store was borrowed for the call and released, so every other seal is
    // still usable — the point of the scoped `&mut` rule.
    assert!(sealed.metrics().is_some());
    assert!(sealed.events().is_some());
    Ok(())
}

/// One dispatch decision with no usage observations.
///
/// The candidate catalogue and the policy are the caller's — B5 seals the
/// journal, not the routing policy — and an empty observation set is the
/// honest shape for an offline gate: it exercises the boundary's own
/// preconditions and its durable append without inventing provider telemetry.
fn dispatch_request(mission: &MissionId) -> TestResult<DispatchDecisionRequest> {
    let provider = ProviderIdV1::new("claude")?;
    let account = UsageAccountIdV1::new("acct-primary")?;
    Ok(DispatchDecisionRequest {
        authority_inputs: FixedAuthorityInputs {
            identity: RouteIdentity {
                candidate_id: CandidateIdV1::new("claude-primary")?,
                provider: provider.clone(),
                usage_account_id: account.clone(),
            },
            runtime: RuntimeResolutionInput::default(),
            forced_model: None,
            fixed_provider_mode: false,
            legacy_mode: false,
            tier: ModelTier::Work,
            persona: "senior-backend-engineer".to_owned(),
            routing_map: RoutingMap::default(),
        },
        mission_id: mission.clone(),
        phase_id: phase_one()?,
        attempt: 1,
        evaluated_at_utc_ms: UtcMillisV1::new(1_738_425_540_000),
        committed_at_utc: "2026-09-03T00:00:00Z".to_owned(),
        requirements: RouteRequirementsV1 {
            priority: TaskPriorityV1::P1,
            required_capabilities: BTreeSet::new(),
            minimum_quality: QualityTierV1::Economy,
        },
        candidates: vec![AdaptiveCandidateV1 {
            route: RouteTargetV1 {
                candidate_id: CandidateIdV1::new("claude-primary")?,
                provider: provider.clone(),
                usage_account_id: account.clone(),
                runtime: RuntimeIdV1::new("claude")?,
                model: "sonnet".to_owned(),
                effort: Some("medium".to_owned()),
            },
            capabilities: BTreeSet::new(),
            automatic_eligibility: AutomaticEligibilityV1::Automatic,
            quality: QualityTierV1::Standard,
            task_fit_bps: BasisPointsV1::new(10_000)?,
            latency_bps: BasisPointsV1::new(10_000)?,
            configured_preference_bps: BasisPointsV1::new(10_000)?,
        }],
        incumbent_candidate_id: None,
        existing_session_runtime: None,
        policy: AdaptivePolicyV1 {
            policy_version: 1,
            maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
            provider_reserves: vec![ProviderReserveV1 {
                provider,
                usage_account_id: account,
                reserve_bps: BasisPointsV1::new(0)?,
            }],
            score_weights: ScoreWeightsV1 {
                task_fit_bps: BasisPointsV1::new(4_000)?,
                usable_headroom_bps: BasisPointsV1::new(2_000)?,
                reset_proximity_bps: BasisPointsV1::new(1_000)?,
                recent_capacity_bps: BasisPointsV1::new(1_000)?,
                latency_bps: BasisPointsV1::new(1_000)?,
                health_bps: BasisPointsV1::new(500)?,
                configured_preference_bps: BasisPointsV1::new(500)?,
            },
            switch_margin_bps: BasisPointsV1::new(0)?,
            unknown_usage_policy: UnknownUsagePolicyV1::Defer,
            evaluation_mode: AdaptiveEvaluationModeV1::Enforce,
        },
        multiplexer_policy: UsageMultiplexerPolicy {
            maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
            cooldown_ms: DurationMillisV1::new(60_000)?,
        },
        observations: Vec::new(),
        last_accepted_observed_at: BTreeMap::new(),
    })
}

/// The source-level half of the same rule: the store is handed in for the call
/// and never retained as a field borrow.
#[test]
fn seal_seven_borrows_the_store_for_the_call_only() {
    assert!(
        COMPOSITION_SOURCE.contains("decide_dispatch_route(request, store)"),
        "seal 7 must hand the sealed store in as the journal"
    );
    assert!(
        COMPOSITION_SOURCE
            .contains("let store = self.store.as_mut().ok_or(SealedRunError::Unenrolled)?;"),
        "the store must be borrowed for the call, not retained as a field borrow"
    );
}
