#![cfg(unix)]
//! B5-DESIGN §4, Gate 6 — Rust writes the durable mission state, dies under
//! `SIGKILL`, and the frozen in-tree Go binary reads and resumes what it left.
//!
//! The crash mechanism is transplanted from `metrics_audit_security_e2e.rs`:
//! this binary re-execs itself in a `crash-after-commit` mode, commits the
//! durable state, and sends itself `SIGKILL` through `rustix` — the workspace
//! forbids `unsafe`, and `SIGKILL` rather than `abort` because only `SIGKILL`
//! is uncatchable, so no destructor, no buffered flush and no orderly
//! shutdown can have run. The parent asserts `status.signal() == Some(9)`, so a
//! clean exit cannot pass for a crash.
//!
//! Five artifacts are written by Rust and handed to Go: the **workspace**
//! (`FreshFixtureAuthority::create_workspace`'s atomic base), the **checkpoint**
//! (`orchestrator_core::encode_current_checkpoint`, envelope-v1/payload-v2), the
//! **plan** (`plan.json`, the same serializer), the **signal**
//! (`orchestrator.signal.json` in `core.CompletionSignal` shape), and a
//! **template** (`<home>/templates/<name>.json` in `core.Template` shape).
//!
//! ## What "unknown fields preserved" means here, measured rather than assumed
//!
//! Go's `core.Checkpoint` is a plain struct and `encoding/json` silently drops
//! keys it has no field for. So the claim cannot be "Go preserves them", and
//! this gate does not assert that. It asserts the three things that are true
//! and that matter:
//!
//! 1. Go **reads and resumes** a Rust-written checkpoint that carries unknown
//!    keys at all three levels (envelope, payload, plan, phase) without
//!    failing — forward-compatible data does not break the older reader.
//! 2. Rust's own codec round-trips every one of those keys byte-identically,
//!    which is what makes Rust the side that can carry them forward.
//! 3. Exactly which keys Go's rewrite drops is **pinned**, so a change in Go's
//!    struct — or in what Rust chooses to emit — is red rather than silent.
//!
//! ## What this gate does not prove
//!
//! The Go engine consults `orchestrator.signal.json` only while dispatching a
//! phase, which needs a live provider and is therefore out of reach of a
//! hermetic gate. The signal leg here is a **preservation** claim: the file
//! Rust wrote is byte-identical after Go's resume and still parses as a
//! `CompletionSignal` whose `kind` is one of the seven `internal/core/signal.go`
//! declares. That Go *acts* on it belongs to a gate with a provider.
//!
//! **The oracle is mandatory.** A missing or unusable Go binary is a hard
//! failure naming exactly what was checked and where — never an `eprintln!` +
//! `Ok(())` green (TRK-1280).

use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, decode_checkpoint,
    encode_current_checkpoint, encode_current_plan,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const HELPER_MODE: &str = "NANIKA_B5_RESUME_HELPER_MODE";
const HELPER_PARENT: &str = "NANIKA_B5_RESUME_HELPER_PARENT";

const MISSION: &str = "rust-write-go-resume";
const TEMPLATE: &str = "rust-written-template";

/// The unknown keys Rust writes at each level of the checkpoint document.
const ENVELOPE_UNKNOWN: &str = "nanika_unknown_envelope_field";
const PAYLOAD_UNKNOWN: &str = "nanika_unknown_payload_field";
const PLAN_UNKNOWN: &str = "nanika_unknown_plan_field";
const PHASE_UNKNOWN: &str = "nanika_unknown_phase_field";

/// The seven `CompletionSignalKind` values `internal/core/signal.go` declares.
const SIGNAL_KINDS: [&str; 7] = [
    "ok",
    "partial",
    "dependency_missing",
    "scope_expansion",
    "replan_required",
    "human_decision_needed",
    "blocked_by_upstream",
];

// ===========================================================================
// The oracle
// ===========================================================================

/// The frozen in-tree Go orchestrator that reads and resumes what Rust wrote.
///
/// B5-DESIGN §3.1: in-tree and mandatory. `ORCHESTRATOR_ACCEPTED_GO_BIN` is a
/// cache of that build, not an alternative source of truth, which is why the
/// fallback is the workspace's own `target/go-oracle/orchestrator` rather than
/// an installed binary — an installed binary was once built from uncommitted
/// `run.go`, which is TRK-1280, and it cannot witness a claim about a
/// committed tree.
///
/// B5-DESIGN §8.6 precedence, first match wins:
///
/// 1. **The lease.** When `NANIKA_GO_ORACLE_OUTPUT_DIR` is set the oracle is
///    `$NANIKA_GO_ORACLE_OUTPUT_DIR/orchestrator` and nothing else is
///    consulted — `ORCHESTRATOR_ACCEPTED_GO_BIN` is not read even if somehow
///    present. Inside the lease there is one oracle and the lease named it:
///    built from the tested commit's `git archive`, against a `go.sum`-verified
///    module snapshot, by a GOROOT-bound toolchain whose digest is recorded.
/// 2. **`ORCHESTRATOR_ACCEPTED_GO_BIN`, bare runs only.** Unchanged in
///    meaning, and unreachable inside the lease by construction: the
///    gatekeeper's three-variable passthrough excludes it.
/// 3. **The in-tree build.**
fn frozen_go_oracle() -> Result<PathBuf, String> {
    let manifest = frozen_tree_manifest();
    if !manifest.is_file() {
        return Err(format!(
            "the frozen Go oracle manifest {} is missing; the in-tree oracle cannot be identified",
            manifest.display()
        ));
    }
    if let Some(directory) = std::env::var_os("NANIKA_GO_ORACLE_OUTPUT_DIR") {
        let leased = PathBuf::from(directory).join("orchestrator");
        return if leased.is_file() {
            Ok(leased)
        } else {
            Err(format!(
                "NANIKA_GO_ORACLE_OUTPUT_DIR names no built Go oracle at {}; the verification \
                 lease did not produce one (manifest: {})",
                leased.display(),
                manifest.display()
            ))
        };
    }
    if let Some(path) = std::env::var_os("ORCHESTRATOR_ACCEPTED_GO_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "ORCHESTRATOR_ACCEPTED_GO_BIN={} does not name a file; the accepted Go oracle binary is missing (manifest: {})",
                path.display(),
                manifest.display()
            ))
        };
    }
    let built = workspace_root().join("target/go-oracle/orchestrator");
    if built.is_file() {
        Ok(built)
    } else {
        Err(format!(
            "the frozen Go oracle binary is missing: neither ORCHESTRATOR_ACCEPTED_GO_BIN nor the in-tree build {} names an existing file (manifest: {})",
            built.display(),
            manifest.display()
        ))
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

fn frozen_tree_manifest() -> PathBuf {
    workspace_root().join("tests/go-oracle/frozen-tree-manifest.json")
}

// ===========================================================================
// The durable state Rust writes
// ===========================================================================

/// The plan the writer commits: two phases, both terminal, with a `DEPENDS`
/// edge and unknown keys on the plan and on the second phase.
///
/// Both phases are terminal on purpose. Go's `resetPhasesForResume` revives
/// failed and running phases, so a pending phase would make the resume dispatch
/// a worker — which needs a live provider and is out of a hermetic gate's
/// reach. An all-terminal plan is exactly §3.4's R4 rung: Go reads what Rust
/// left and continues it to a second terminal outcome without inventing work.
fn terminal_plan() -> CheckpointPlan {
    let mut plan_extra = BTreeMap::new();
    plan_extra.insert(
        PLAN_UNKNOWN.to_owned(),
        serde_json::json!({"written_by": "rust", "shape": [1, 2, 3]}),
    );
    let mut phase_extra = BTreeMap::new();
    phase_extra.insert(PHASE_UNKNOWN.to_owned(), serde_json::json!({"kept": true}));
    CheckpointPlan {
        id: "plan-rust-write-go-resume".to_owned(),
        task: "fixture task".to_owned(),
        phases: vec![
            CheckpointPhase {
                id: "phase-1".to_owned(),
                name: "build".to_owned(),
                objective: "compile the crate".to_owned(),
                persona: "senior-backend-engineer".to_owned(),
                model_tier: "work".to_owned(),
                runtime: "codex".to_owned(),
                status: "completed".to_owned(),
                ..CheckpointPhase::default()
            },
            CheckpointPhase {
                id: "phase-2".to_owned(),
                name: "verify".to_owned(),
                objective: "run the gates".to_owned(),
                persona: "qa-engineer".to_owned(),
                model_tier: "work".to_owned(),
                runtime: "codex".to_owned(),
                dependencies: vec!["phase-1".to_owned()],
                status: "completed".to_owned(),
                extra: phase_extra,
                ..CheckpointPhase::default()
            },
        ],
        execution_mode: "sequential".to_owned(),
        decomp_source: "predecomposed".to_owned(),
        created_at: "2026-07-13T00:00:00Z".to_owned(),
        extra: plan_extra,
    }
}

fn terminal_checkpoint() -> CheckpointProjection {
    let mut payload_extra = BTreeMap::new();
    payload_extra.insert(
        PAYLOAD_UNKNOWN.to_owned(),
        serde_json::Value::String("kept".to_owned()),
    );
    let mut envelope_extra = BTreeMap::new();
    envelope_extra.insert(ENVELOPE_UNKNOWN.to_owned(), serde_json::Value::from(42_i64));
    CheckpointProjection {
        workspace_id: MISSION.to_owned(),
        domain: "dev".to_owned(),
        plan: Some(terminal_plan()),
        status: "completed".to_owned(),
        started_at: "2026-07-13T00:00:00Z".to_owned(),
        extra: payload_extra,
        envelope_extra,
        ..CheckpointProjection::default()
    }
}

/// The `core.CompletionSignal` shape, field name for field name.
fn signal_document() -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "kind": "partial",
        "summary": "the writer committed its checkpoint and then died",
        "missing_input": [],
        "changed_files": ["checkpoint.json", "plan.json"],
        "remainder": "phase-2 evidence was never collected",
    }))
    .unwrap_or_default()
}

/// The `core.Template` shape, with one unknown key.
fn template_document() -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "name": TEMPLATE,
        "task": "fixture task",
        "phases": [{
            "id": "phase-1",
            "name": "build",
            "objective": "compile {{target}}",
            "persona": "senior-backend-engineer",
            "model_tier": "work",
            "skills": ["golang-cli"],
            "status": "",
        }],
        "execution_mode": "sequential",
        "created_at": "2026-07-13T00:00:00Z",
        "source_plan_id": "plan-rust-write-go-resume",
        "nanika_unknown_template_field": {"kept": true},
    }))
    .unwrap_or_default()
}

// ===========================================================================
// The crashing writer
// ===========================================================================

/// Re-exec entry point. Inert unless [`HELPER_MODE`] is set, which only
/// [`writer_command`] does.
#[test]
fn helper_entrypoint() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    let code = match mode.as_str() {
        // Commit every durable artifact, then die without unwinding. Reaching
        // the exit below at all means a write failed — the parent sees a
        // non-signal exit and reports it rather than mistaking it for a crash.
        "crash-after-commit" => match write_everything(true) {
            Ok(()) => 3,
            Err(error) => {
                eprintln!("writer failed: {error}");
                4
            }
        },
        // Create the workspace but die *before* the checkpoint is committed,
        // so the parent can hand Go a workspace with no resumable offset.
        "crash-before-commit" => match write_everything(false) {
            Ok(()) => 3,
            Err(error) => {
                eprintln!("writer failed: {error}");
                4
            }
        },
        _ => 2,
    };
    std::process::exit(code);
}

fn write_everything(commit_checkpoint: bool) -> TestResult {
    let parent = PathBuf::from(std::env::var(HELPER_PARENT)?);
    let helper = fs::read(std::env::current_exe()?)?;
    let checkout = fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let temporary = fs::canonicalize(std::env::temp_dir())?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
        .with_expected_fixture_helper(&helper);

    let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = fresh.path().to_path_buf();
    let authority = FreshFixtureAuthority::admit(fresh, &policy)?;

    let checkpoint = terminal_checkpoint();
    let plan = checkpoint
        .plan
        .clone()
        .ok_or("the fixture plan is absent")?;
    let seed = FixtureWorkspaceSeed::new(
        b"PHASE: build | OBJECTIVE: compile the crate\nPHASE: verify | OBJECTIVE: run the gates | DEPENDS: build\n".to_vec(),
        &checkpoint,
        encode_current_plan(&plan)?,
    )?;
    let _workspace = authority.create_workspace(MissionId::new(MISSION)?, seed)?;

    // The base workspace already carries `checkpoint.json`; the
    // `crash-before-commit` mode removes it so the crash lands on a workspace
    // with no resumable offset at all.
    let workspace_path = root.join("workspaces").join(MISSION);
    if commit_checkpoint {
        fs::write(
            workspace_path.join("orchestrator.signal.json"),
            signal_document(),
        )?;
        let templates = root.join("templates");
        fs::create_dir_all(&templates)?;
        fs::write(
            templates.join(format!("{TEMPLATE}.json")),
            template_document(),
        )?;
    } else {
        fs::remove_file(workspace_path.join("checkpoint.json"))?;
    }

    // Deliberately no orderly shutdown: the point is that nothing runs after
    // the commit.
    kill_self();
    Err("SIGKILL did not take effect".into())
}

/// Sends `SIGKILL` to this process and does not return while it is pending.
///
/// `rustix` rather than a raw `libc` call because the workspace forbids
/// `unsafe`, and `SIGKILL` rather than `abort` because only `SIGKILL` is
/// uncatchable — a catchable signal would leave open the objection that some
/// handler still had a chance to flush.
///
/// B5-DESIGN §9.L. One `kill()` return was treated as meaning the process is
/// gone, and under load it is not: the caller fell straight through to
/// `"SIGKILL did not take effect"` and the parent saw a clean exit where it
/// requires a signal. Re-sending under a deadline changes nothing this test
/// witnesses — the loop writes nothing, flushes nothing and runs no destructor,
/// which is the whole property `SIGKILL` rather than `abort` protects, and the
/// parent still requires `status.signal() == Some(9)`. A syscall error is
/// reported rather than discarded, so `EPERM`/`ESRCH` is distinguishable from a
/// delivery that has not landed yet.
fn kill_self() {
    let raw = i32::try_from(std::process::id()).unwrap_or(0);
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        eprintln!("this process has no representable pid to signal");
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Err(error) = rustix::process::kill_process(pid, rustix::process::Signal::KILL) {
            eprintln!("sending SIGKILL to this process failed: {error}");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ===========================================================================
// The fixture the writer crashed inside
// ===========================================================================

struct Crashed {
    parent: PathBuf,
    root: PathBuf,
}

impl Drop for Crashed {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl Crashed {
    /// Runs the writer to its `SIGKILL` and returns the home it left behind.
    fn write(label: &str, mode: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-b5-resume-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
        }

        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "helper_entrypoint", "--nocapture"])
            .env(HELPER_MODE, mode)
            .env(HELPER_PARENT, &parent)
            .status()?;
        assert_eq!(
            status.signal(),
            Some(9),
            "the writer exited cleanly ({status:?}) instead of dying under SIGKILL, \
             so nothing here witnesses a crash"
        );

        let root = fs::read_dir(&parent)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("fixture-"))
            })
            .ok_or("the writer left no isolated fixture root behind")?;
        Ok(Self { parent, root })
    }

    fn workspace(&self) -> PathBuf {
        self.root.join("workspaces").join(MISSION)
    }

    fn observe(&self, binary: &Path, argv: &[&str]) -> io::Result<(Option<i32>, String, String)> {
        let output = Command::new(binary)
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.parent.join("go-user"))
            .env("ORCHESTRATOR_CONFIG_DIR", &self.root)
            .env("ORCHESTRATOR_PERSONAS_DIR", self.parent.join("personas"))
            .env("PATH", self.parent.join("empty-path"))
            .env("TMPDIR", &self.parent)
            .output()?;
        Ok((
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }

    fn resume(&self, binary: &Path) -> io::Result<(Option<i32>, String, String)> {
        let workspace = self.workspace().to_string_lossy().into_owned();
        self.observe(binary, &["run", "--resume", &workspace])
    }
}

/// The phase `(id, status)` rows a checkpoint document carries, in order.
fn phase_rows(bytes: &[u8]) -> TestResult<Vec<(String, String)>> {
    let decoded = decode_checkpoint(bytes)?;
    Ok(decoded
        .projection
        .plan
        .as_ref()
        .ok_or("the checkpoint carries no plan")?
        .phases
        .iter()
        .map(|phase| (phase.id.clone(), phase.status.clone()))
        .collect())
}

/// Every key present in the raw checkpoint document, as `level.key` pairs.
fn unknown_keys_present(bytes: &[u8]) -> TestResult<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let mut present = Vec::new();
    if value.get(ENVELOPE_UNKNOWN).is_some() {
        present.push(format!("envelope.{ENVELOPE_UNKNOWN}"));
    }
    let payload = value.get("payload").unwrap_or(&serde_json::Value::Null);
    if payload.get(PAYLOAD_UNKNOWN).is_some() {
        present.push(format!("payload.{PAYLOAD_UNKNOWN}"));
    }
    let plan = payload.get("plan").unwrap_or(&serde_json::Value::Null);
    if plan.get(PLAN_UNKNOWN).is_some() {
        present.push(format!("plan.{PLAN_UNKNOWN}"));
    }
    if plan
        .get("phases")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|phases| {
            phases
                .iter()
                .any(|phase| phase.get(PHASE_UNKNOWN).is_some())
        })
    {
        present.push(format!("phase.{PHASE_UNKNOWN}"));
    }
    Ok(present)
}

// ===========================================================================
// The gate
// ===========================================================================

#[test]
fn the_writer_dies_under_sigkill_with_its_state_committed() -> TestResult {
    let crashed = Crashed::write("commit", "crash-after-commit")?;
    for leaf in [
        "mission.md",
        "checkpoint.json",
        "plan.json",
        "orchestrator.signal.json",
    ] {
        assert!(
            crashed.workspace().join(leaf).is_file(),
            "the crash left no {leaf}"
        );
    }
    assert!(
        crashed
            .root
            .join("templates")
            .join(format!("{TEMPLATE}.json"))
            .is_file(),
        "the crash left no template"
    );
    assert_eq!(
        phase_rows(&fs::read(crashed.workspace().join("checkpoint.json"))?)?,
        [
            ("phase-1".to_owned(), "completed".to_owned()),
            ("phase-2".to_owned(), "completed".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn the_oracle_reads_the_rust_written_workspace() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("read", "crash-after-commit")?;
    let (exit, stdout, stderr) = crashed.observe(&go_binary, &["status"])?;
    assert_eq!(exit, Some(0), "status: {stdout} {stderr}");
    assert!(
        stdout.contains(MISSION),
        "the oracle did not list the Rust-written workspace: {stdout}"
    );
    assert!(
        stdout.contains("2/2 phases"),
        "the oracle read a different phase shape: {stdout}"
    );
    assert!(
        stdout.contains("[completed]"),
        "the oracle read a different status: {stdout}"
    );
    Ok(())
}

#[test]
fn the_oracle_resumes_the_rust_written_workspace_to_terminal() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("resume", "crash-after-commit")?;
    let (exit, stdout, stderr) = crashed.resume(&go_binary)?;
    assert_eq!(exit, Some(0), "resume: {stdout} {stderr}");
    assert!(
        stdout.contains("resuming mission from"),
        "the oracle did not take the resume path: {stdout}"
    );
    assert!(
        stdout.contains("mission completed"),
        "the resume did not reach a terminal outcome: {stdout}"
    );
    assert!(
        stdout.contains("2 completed, 0 failed"),
        "the resume changed the phase outcomes: {stdout}"
    );
    Ok(())
}

#[test]
fn modes_survive_the_oracle_resume() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("modes", "crash-after-commit")?;
    let before = fs::read(crashed.workspace().join("checkpoint.json"))?;
    let (exit, stdout, _) = crashed.resume(&go_binary)?;
    assert_eq!(exit, Some(0), "{stdout}");

    let after = fs::read(crashed.workspace().join("checkpoint.json"))?;
    let before_decoded = decode_checkpoint(&before)?.projection;
    let after_decoded = decode_checkpoint(&after)?.projection;
    let before_plan = before_decoded.plan.as_ref().ok_or("no plan before")?;
    let after_plan = after_decoded.plan.as_ref().ok_or("no plan after")?;

    assert_eq!(
        after_decoded.status, before_decoded.status,
        "mission status"
    );
    assert_eq!(after_decoded.domain, before_decoded.domain, "domain");
    assert_eq!(
        after_decoded.started_at, before_decoded.started_at,
        "started_at"
    );
    assert_eq!(
        after_plan.execution_mode, before_plan.execution_mode,
        "execution mode"
    );
    assert_eq!(after_plan.id, before_plan.id, "plan id");
    assert_eq!(after_plan.task, before_plan.task, "plan task");
    assert_eq!(
        phase_rows(&after)?,
        phase_rows(&before)?,
        "the resume changed the phase modes"
    );
    for (before_phase, after_phase) in before_plan.phases.iter().zip(&after_plan.phases) {
        assert_eq!(after_phase.persona, before_phase.persona, "persona");
        assert_eq!(
            after_phase.model_tier, before_phase.model_tier,
            "model tier"
        );
        assert_eq!(after_phase.runtime, before_phase.runtime, "runtime");
        assert_eq!(
            after_phase.dependencies, before_phase.dependencies,
            "dependencies"
        );
    }
    Ok(())
}

#[test]
fn a_repeated_go_resume_creates_no_duplicate_phase_row() -> TestResult {
    // B5-DESIGN §4 Gate 6's negative assertion, across the language boundary:
    // the idempotence of the phase upsert is exercised in-process today, and
    // here the *second* writer is Go over a Rust-written base. Rows are
    // compared as an ordered sequence, never as a count.
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("idempotent", "crash-after-commit")?;
    let expected = phase_rows(&fs::read(crashed.workspace().join("checkpoint.json"))?)?;

    let mut renders = Vec::new();
    for attempt in 1..=3 {
        let (exit, stdout, stderr) = crashed.resume(&go_binary)?;
        assert_eq!(exit, Some(0), "resume {attempt}: {stdout} {stderr}");
        assert_eq!(
            phase_rows(&fs::read(crashed.workspace().join("checkpoint.json"))?)?,
            expected,
            "resume {attempt} changed the phase rows"
        );
        renders.push(
            stdout
                .lines()
                .filter(|line| line.contains("phases:"))
                .map(str::trim)
                .collect::<Vec<&str>>()
                .join("|"),
        );
    }
    let first = renders.first().ok_or("no render")?;
    for render in &renders {
        assert_eq!(render, first, "a repeated resume reported different phases");
    }
    Ok(())
}

#[test]
fn a_resume_from_a_non_checkpoint_offset_fails_closed() -> TestResult {
    let go_binary = frozen_go_oracle()?;

    // (a) A crash *before* the commit: the workspace exists with no checkpoint.
    let missing = Crashed::write("no-checkpoint", "crash-before-commit")?;
    assert!(
        !missing.workspace().join("checkpoint.json").exists(),
        "the crash-before-commit mode still left a checkpoint"
    );
    let (exit, stdout, _) = missing.resume(&go_binary)?;
    assert_ne!(exit, Some(0), "the oracle guessed a missing checkpoint");
    assert!(
        !stdout.contains("mission completed"),
        "the oracle claimed completion from no checkpoint: {stdout}"
    );

    // (b) A checkpoint truncated mid-object, which is what a torn write looks
    // like. It must refuse rather than resume from a partial plan.
    let torn = Crashed::write("torn-checkpoint", "crash-after-commit")?;
    let path = torn.workspace().join("checkpoint.json");
    let bytes = fs::read(&path)?;
    fs::write(&path, &bytes[..bytes.len() / 2])?;
    let (exit, stdout, _) = torn.resume(&go_binary)?;
    assert_ne!(exit, Some(0), "the oracle resumed from a torn checkpoint");
    assert!(
        !stdout.contains("mission completed"),
        "the oracle claimed completion from a torn checkpoint: {stdout}"
    );

    // (c) A path outside the runtime home is refused before anything is read.
    let outside = torn.parent.join("outside").to_string_lossy().into_owned();
    fs::create_dir_all(&outside)?;
    let (exit, _, _) = torn.observe(&go_binary, &["run", "--resume", &outside])?;
    assert_ne!(exit, Some(0), "the oracle resumed a path outside its home");
    Ok(())
}

#[test]
fn rust_round_trips_every_unknown_field_the_oracle_drops() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("unknown", "crash-after-commit")?;
    let path = crashed.workspace().join("checkpoint.json");
    let before = fs::read(&path)?;

    // 1. Rust wrote all four levels.
    assert_eq!(
        unknown_keys_present(&before)?,
        [
            format!("envelope.{ENVELOPE_UNKNOWN}"),
            format!("payload.{PAYLOAD_UNKNOWN}"),
            format!("plan.{PLAN_UNKNOWN}"),
            format!("phase.{PHASE_UNKNOWN}"),
        ],
        "the writer did not emit every unknown level, so the drop set below \
         would be measured against nothing"
    );

    // 2. Rust's own codec carries them forward byte-identically. This is the
    //    property that makes Rust the side that can preserve them.
    let round_tripped = encode_current_checkpoint(&decode_checkpoint(&before)?.projection)?;
    assert_eq!(
        round_tripped, before,
        "the Rust codec did not round-trip its own checkpoint byte-for-byte"
    );

    // 3. Go reads it without failing, and exactly which keys its rewrite drops
    //    is pinned. Go's `core.Checkpoint` is a plain struct, so `encoding/json`
    //    discards every key it has no field for; a change on either side moves
    //    this list and turns the gate red.
    let (exit, stdout, stderr) = crashed.resume(&go_binary)?;
    assert_eq!(
        exit,
        Some(0),
        "the oracle refused a checkpoint carrying unknown keys: {stdout} {stderr}"
    );
    let after = fs::read(&path)?;
    assert_eq!(
        unknown_keys_present(&after)?,
        Vec::<String>::new(),
        "the measured set of unknown keys Go's rewrite drops has changed"
    );

    // 4. And Rust still reads what Go rewrote, with every modelled field intact
    //    — the drop is of unknown keys only, not of the shared vocabulary.
    let after_decoded = decode_checkpoint(&after)?.projection;
    let before_decoded = decode_checkpoint(&before)?.projection;
    assert_eq!(after_decoded.workspace_id, before_decoded.workspace_id);
    assert_eq!(after_decoded.status, before_decoded.status);
    assert_eq!(phase_rows(&after)?, phase_rows(&before)?);
    Ok(())
}

#[test]
fn the_rust_written_signal_survives_the_oracle_resume() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("signal", "crash-after-commit")?;
    let path = crashed.workspace().join("orchestrator.signal.json");
    let before = fs::read(&path)?;

    let value: serde_json::Value = serde_json::from_slice(&before)?;
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or("the signal carries no kind")?;
    assert!(
        SIGNAL_KINDS.contains(&kind),
        "{kind:?} is not one of the seven CompletionSignalKind values"
    );

    let (exit, stdout, stderr) = crashed.resume(&go_binary)?;
    assert_eq!(exit, Some(0), "resume: {stdout} {stderr}");
    assert_eq!(
        fs::read(&path)?,
        before,
        "the oracle rewrote or deleted a signal file it did not own"
    );
    Ok(())
}

#[test]
fn the_rust_written_template_is_read_by_the_oracle() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let crashed = Crashed::write("template", "crash-after-commit")?;
    let path = crashed
        .root
        .join("templates")
        .join(format!("{TEMPLATE}.json"));
    let before = fs::read(&path)?;

    let (exit, stdout, stderr) = crashed.observe(&go_binary, &["templates", "list"])?;
    assert_eq!(exit, Some(0), "templates list: {stdout} {stderr}");
    assert!(
        stdout.contains(TEMPLATE),
        "the oracle did not list the Rust-written template: {stdout}"
    );
    assert!(
        stdout.contains("1 phases (sequential)"),
        "the oracle read a different template shape: {stdout}"
    );
    assert!(
        stdout.contains("fixture task"),
        "the oracle read a different template task: {stdout}"
    );
    assert_eq!(
        fs::read(&path)?,
        before,
        "listing rewrote a template the reader does not own"
    );
    Ok(())
}

#[test]
fn a_missing_oracle_is_a_hard_failure_rather_than_a_skip() -> TestResult {
    let manifest = frozen_tree_manifest();
    assert!(
        manifest.is_file(),
        "the frozen-tree manifest {} must exist for the oracle to be identifiable",
        manifest.display()
    );
    let resolved = frozen_go_oracle()?;
    assert!(
        resolved.is_file(),
        "the resolved oracle {} is not a file",
        resolved.display()
    );
    Ok(())
}
