#![cfg(unix)]
//! B5-DESIGN §4, Gate 8 — every durable artifact, written and read from both
//! sides of the Go↔Rust boundary.
//!
//! `cargo test -p orchestrator-app --release --locked --test
//! go_bidirectional_differential -- --exact core_go_bidirectional_matrix`
//!
//! One `#[test]`, as the `--exact` invocation requires. The matrix is
//! (write-side ∈ {Go, Rust}) × (read-side ∈ {Go, Rust}) × (each durable
//! artifact: the runtime-store checkpoint projection, the canonical event log,
//! `metrics.db`, and `learnings.db`) — sixteen cells, every one of which
//! actually runs.
//!
//! ## The census, and why a cell cannot quietly not happen
//!
//! `hermetic_go_rust_differential_corpus` compares one operation at a time and
//! a case it cannot run is simply not in the corpus. That is the failure mode
//! this gate is built against: an absent direction is indistinguishable from a
//! direction nobody wrote. So every cell is enumerated up front from the
//! artifact list crossed with both sides, each is *run*, and each lands in the
//! census as exactly one of:
//!
//! * `go-observed` — the frozen in-tree Go binary participated in this cell,
//!   as writer or as reader, and its bytes, rows or exit code were compared.
//! * `rust-only` — the Go binary occupies neither role in this cell. Both
//!   roles are Rust by construction, so the cell instead pins Rust's own
//!   round-trip and carries the **normative reason** that says what the
//!   expectation is pinned against and which cells carry Go for this artifact.
//! * `unavailable` — the cell's mechanism failed to run at all.
//!
//! The classification is **derived** from which sides participate, never
//! asserted by hand ([`Cell::observed`]), so a cell cannot be mislabelled into
//! looking Go-witnessed. `unavailable` exists precisely so a broken cell shows
//! up as a named row rather than as an opaque early return, and the matrix
//! assertion requires the count to be zero. A gate that could not express
//! "this did not run" could not prove that everything did.
//!
//! ## The oracle is in-tree and mandatory
//!
//! B5-DESIGN §3.1. [`frozen_go_oracle`] *builds* the in-tree Go orchestrator
//! through `tests/build-go-oracle.sh` and accepts
//! `ORCHESTRATOR_ACCEPTED_GO_BIN` only when its sha256 equals that build's —
//! the environment variable is a cache, not an alternative source of truth. A
//! missing, unbuildable or mismatched oracle is a hard failure that names what
//! was checked and where. Never an `eprintln!` + `Ok(())` green: an installed
//! binary was once built from uncommitted `run.go` (TRK-1280), and an
//! installed binary cannot witness a claim about a committed tree.
//!
//! ## Two Go home profiles, and why
//!
//! Go resolves its home by precedence. The checkpoint, event-log and
//! `metrics.db` cells drive it with `ORCHESTRATOR_CONFIG_DIR=<fixture root>`,
//! which is where those artifacts live. `learnings.db` is different: the Rust
//! port reads it at the Go *home-relative* path `<root>/.alluka/learnings.db`
//! (`knowledge_gateway.rs`'s `LEARNINGS_DB_RELATIVE`), so those cells drive Go
//! with `HOME=<fixture root>` and a pre-existing `<root>/.alluka`, which
//! selects the same file. Both are ordinary Go precedence branches, not a
//! special mode.
//!
//! Nothing here touches `~/.alluka`: every leg runs under `env_clear()` with an
//! explicit allowlist pointing into a private per-cell fixture root.

use std::{
    collections::BTreeSet,
    fmt, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use orchestrator_app::{
    AdditiveFixtureGrant, ExactProcessGroupAbsence, GoLearningAdapter, GoLearningAppender,
    GoLearningReader, IsolatedFixtureRoot, KernelProcessIdentity, LearningListQuery, MetricsOwner,
    MetricsOwnerCapability, NewLearningRow, PhaseMetricIntent, PhaseStatus,
    RecordedProcessIdentityStatus, TerminalMetricIntent, inspect_recorded_process_identity,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, EventJsonMap, EventRecord, MissionId,
    decode_checkpoint, encode_current_checkpoint, encode_current_event, scan_event_log,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const HELPER_MODE: &str = "NANIKA_B5_BIDI_HELPER_MODE";

const AT: &str = "2026-07-13T00:00:00Z";

// ===========================================================================
// The oracle (B5-DESIGN §3.1)
// ===========================================================================

/// Builds the frozen in-tree Go orchestrator and returns the accepted binary.
///
/// The in-tree build is the source of truth. `ORCHESTRATOR_ACCEPTED_GO_BIN` is
/// honoured only when it names a file whose sha256 equals that build's, and a
/// mismatch is an error naming **both** digests and both paths — that is the
/// whole content of §3.1's rule, and it is what stops a stale or
/// locally-installed binary from standing in for the committed tree.
fn frozen_go_oracle() -> Result<PathBuf, String> {
    let manifest = frozen_tree_manifest();
    if !manifest.is_file() {
        return Err(format!(
            "the frozen Go oracle manifest {} is missing; the in-tree oracle cannot be identified",
            manifest.display()
        ));
    }
    let builder = workspace_root().join("tests/build-go-oracle.sh");
    if !builder.is_file() {
        return Err(format!(
            "the in-tree Go oracle builder {} is missing; the oracle cannot be built (manifest: {})",
            builder.display(),
            manifest.display()
        ));
    }
    let output = Command::new(&builder)
        .current_dir(workspace_root())
        .output()
        .map_err(|error| {
            format!(
                "the in-tree Go oracle build {} could not be started: {error}",
                builder.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "the in-tree Go oracle build {} failed with {:?}: {}",
            builder.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // The builder prints the oracle it resolved. Under the lease that is the
    // lease-owned output directory (§8.6 rung 1) and under a bare run it is
    // `target/go-oracle/orchestrator`; reading it back rather than hardcoding
    // the bare path keeps one definition of where the oracle is.
    let built = resolved_oracle_path(&output.stdout, &builder)?;
    let reference = sha256_file(&built).map_err(|error| {
        format!(
            "the in-tree Go oracle build produced no readable binary at {}: {error}",
            built.display()
        )
    })?;

    // Rung 1 wins outright: inside the lease there is one oracle and the lease
    // named it, so `ORCHESTRATOR_ACCEPTED_GO_BIN` is not consulted at all.
    if std::env::var_os("NANIKA_GO_ORACLE_OUTPUT_DIR").is_some() {
        return Ok(built);
    }
    match std::env::var_os("ORCHESTRATOR_ACCEPTED_GO_BIN") {
        None => Ok(built),
        Some(cached) => accept_cached_oracle(Path::new(&cached), &built, &reference, &manifest),
    }
}

/// The last non-empty line of the builder's stdout, as an absolute path to an
/// existing file. A builder that printed nothing usable is an error naming what
/// it printed, never a fallback to a guessed location.
fn resolved_oracle_path(stdout: &[u8], builder: &Path) -> Result<PathBuf, String> {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .ok_or_else(|| {
            format!(
                "the in-tree Go oracle build {} printed no oracle path",
                builder.display()
            )
        })?;
    let resolved = PathBuf::from(line);
    if !resolved.is_absolute() || !resolved.is_file() {
        return Err(format!(
            "the in-tree Go oracle build {} printed {line}, which is not an existing absolute path",
            builder.display()
        ));
    }
    Ok(resolved)
}

/// The whole content of §3.1's rule about the cache, as a pure function so it
/// can be asserted directly rather than by mutating the environment of a test
/// that runs beside the matrix.
fn accept_cached_oracle(
    cached: &Path,
    built: &Path,
    reference: &str,
    manifest: &Path,
) -> Result<PathBuf, String> {
    if !cached.is_file() {
        return Err(format!(
            "ORCHESTRATOR_ACCEPTED_GO_BIN={} does not name a file; the accepted Go oracle binary \
             is missing (in-tree build: {}, manifest: {})",
            cached.display(),
            built.display(),
            manifest.display()
        ));
    }
    let observed = sha256_file(cached).map_err(|error| {
        format!(
            "ORCHESTRATOR_ACCEPTED_GO_BIN={} could not be read: {error}",
            cached.display()
        )
    })?;
    if observed != reference {
        return Err(format!(
            "ORCHESTRATOR_ACCEPTED_GO_BIN={} has sha256 {observed}, but the in-tree build {} has \
             sha256 {reference}; the cache is not the committed tree's oracle (manifest: {}). \
             Rebuild with tests/build-go-oracle.sh and point the variable at its output.",
            cached.display(),
            built.display(),
            manifest.display()
        ));
    }
    Ok(cached.to_path_buf())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

fn frozen_tree_manifest() -> PathBuf {
    workspace_root().join("tests/go-oracle/frozen-tree-manifest.json")
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path)?);
    Ok(format!("{:x}", hasher.finalize()))
}

// ===========================================================================
// The census
// ===========================================================================

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Side {
    Go,
    Rust,
}

impl fmt::Display for Side {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Go => "go",
            Self::Rust => "rust",
        })
    }
}

/// The four durable artifacts the boundary carries.
const ARTIFACTS: [&str; 4] = ["checkpoint", "event-log", "metrics-db", "learnings-db"];
const SIDES: [Side; 2] = [Side::Go, Side::Rust];

#[derive(Debug, Eq, PartialEq)]
enum Census {
    GoObserved,
    RustOnly { reason: &'static str },
    Unavailable { reason: String },
}

impl Census {
    fn label(&self) -> &'static str {
        match self {
            Self::GoObserved => "go-observed",
            Self::RustOnly { .. } => "rust-only",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

struct Cell {
    artifact: &'static str,
    writer: Side,
    reader: Side,
    observed: Census,
    evidence: String,
}

impl Cell {
    fn name(&self) -> String {
        format!("{}:{}->{}", self.artifact, self.writer, self.reader)
    }

    /// Derives the classification from *participation*, never from a hand
    /// label: a cell in which the Go binary ran is `go-observed`, and one in
    /// which it did not is `rust-only` and must carry a normative reason. A
    /// cell whose mechanism failed is `unavailable` and names the failure.
    fn observed(
        artifact: &'static str,
        writer: Side,
        reader: Side,
        outcome: TestResult<String>,
    ) -> Self {
        let observed = match outcome {
            Err(error) => Census::Unavailable {
                reason: error.to_string(),
            },
            Ok(_) if writer == Side::Go || reader == Side::Go => Census::GoObserved,
            Ok(_) => Census::RustOnly {
                reason: rust_only_reason(artifact),
            },
        };
        let evidence = match &observed {
            Census::Unavailable { reason } => reason.clone(),
            _ => String::new(),
        };
        Self {
            artifact,
            writer,
            reader,
            observed,
            evidence,
        }
    }

    fn with_evidence(mut self, evidence: String) -> Self {
        if !matches!(self.observed, Census::Unavailable { .. }) {
            self.evidence = evidence;
        }
        self
    }
}

/// Why a `rust->rust` cell is still normative rather than a tautology.
///
/// Each reason names the Go-facing cells that carry the same artifact, so the
/// row records *what makes the Rust-only expectation binding* rather than
/// merely that Go was absent. The cell itself asserts a round-trip identity, so
/// a codec change that both Rust halves share still fails here.
fn rust_only_reason(artifact: &'static str) -> &'static str {
    match artifact {
        "checkpoint" => {
            "both roles are Rust: `encode_current_checkpoint` feeds `decode_checkpoint` with no \
             Go process in the cell. The expectation is pinned against Go's own document shape by \
             the three go-observed checkpoint cells, and this cell additionally pins that a \
             decode/re-encode round trip is byte-identical, so a shared codec drift cannot hide."
        }
        "event-log" => {
            "both roles are Rust: `encode_current_event` feeds `scan_event_log` with no Go process \
             in the cell. Go's acceptance of the same bytes is carried by the go-observed \
             event-log cells; this cell pins that every scanned `raw_line` is byte-identical to \
             the line that was encoded."
        }
        "metrics-db" => {
            "both roles are Rust: `MetricsOwner` is the sole writer and the reader. Go's \
             acceptance of the same rows is carried by the go-observed metrics cells; this cell \
             pins that the rows read back equal the intents written."
        }
        "learnings-db" => {
            "both roles are Rust: `GoLearningAppender` writes and `GoLearningAdapter` reads, over \
             the schema Go created. Go's acceptance is carried by the go-observed learnings \
             cells; this cell pins that the appended id set is exactly what was inserted."
        }
        other => unreachable!("no rust-only reason declared for artifact {other}"),
    }
}

// ===========================================================================
// The per-cell fixture
// ===========================================================================

/// One private home for one cell.
///
/// Every cell gets its own, so a cell cannot observe another's writes and a
/// failure cannot cascade. `root` stays the `create_fresh` handle because
/// `AdditiveFixtureGrant::in_fixture` and `fixture_production_boundary` both
/// refuse an adopted root.
struct Bridge {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
    go: PathBuf,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl Bridge {
    fn new(go: &Path, label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-b5-bidi-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent)?;
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
        }
        for leaf in ["go-user", "personas", "empty-path"] {
            fs::create_dir_all(parent.join(leaf))?;
        }
        // The Go orchestrator indexes an empty persona slice in
        // `persona.alphabeticalFallback`, so an empty personas directory is a
        // panic rather than a refusal. One file is enough.
        fs::write(
            parent.join("personas/senior-backend-engineer.md"),
            b"# Senior Backend Engineer\n\nBackend work.\n",
        )?;
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        Ok(Self {
            parent,
            root,
            go: go.to_path_buf(),
        })
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// Runs the oracle with `ORCHESTRATOR_CONFIG_DIR` pointed at the fixture
    /// root — the profile the checkpoint, event-log and metrics cells use.
    fn go_config(&self, argv: &[&str]) -> TestResult<(Option<i32>, String, String)> {
        let output = Command::new(&self.go)
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.parent.join("go-user"))
            .env("ORCHESTRATOR_CONFIG_DIR", self.path())
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

    /// Runs the oracle with `HOME` pointed at the fixture root — the profile
    /// the learnings cells use, because `GoLearningAdapter` reads the Go
    /// home-relative `<root>/.alluka/learnings.db`.
    ///
    /// `<root>/.alluka` must already exist or Go's precedence falls through to
    /// `<root>/.via`, which is a different file the Rust adapter never reads.
    fn go_home(&self, argv: &[&str]) -> TestResult<(Option<i32>, String, String)> {
        let alluka = self.path().join(".alluka");
        if !alluka.is_dir() {
            return Err(format!(
                "{} must exist before the oracle runs or Go selects the .via fallback",
                alluka.display()
            )
            .into());
        }
        let output = Command::new(&self.go)
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.path())
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

    fn workspace(&self, mission: &str) -> PathBuf {
        self.path().join("workspaces").join(mission)
    }

    fn event_log(&self, mission: &str) -> PathBuf {
        self.path().join("events").join(format!("{mission}.jsonl"))
    }

    /// Seeds a workspace whose plan is already terminal, in Go's own document
    /// shape rather than through the Rust encoder, so a later Go rewrite is
    /// unambiguously Go's own output.
    fn seed_terminal_workspace(&self, mission: &str) -> TestResult {
        let workspace = self.workspace(mission);
        fs::create_dir_all(&workspace)?;
        fs::write(
            workspace.join("mission.md"),
            b"PHASE: build | OBJECTIVE: compile the crate\n\
              PHASE: verify | OBJECTIVE: run the gates | DEPENDS: build\n",
        )?;
        fs::write(
            workspace.join("checkpoint.json"),
            go_shaped_checkpoint(mission),
        )?;
        Ok(())
    }

    /// Drives the oracle's resume of an already-terminal plan.
    ///
    /// This is the one hermetic path on which Go *writes* every durable
    /// artifact: it rewrites `checkpoint.json`, appends the canonical event
    /// log, and commits the `missions` and `phases` rows of `metrics.db`. It
    /// needs no provider because every phase is already terminal, so
    /// `resetPhasesForResume` revives nothing and no worker is dispatched.
    fn go_resume(&self, mission: &str) -> TestResult<String> {
        let workspace = self.workspace(mission);
        let workspace = workspace.to_string_lossy().into_owned();
        let (exit, stdout, stderr) = self.go_config(&["run", "--resume", &workspace])?;
        if exit != Some(0) {
            return Err(format!("go resume exited {exit:?}: {stdout}{stderr}").into());
        }
        if !stdout.contains("mission completed") {
            return Err(format!("go resume did not reach a terminal outcome: {stdout}").into());
        }
        Ok(stdout)
    }

    fn metrics_owner(&self) -> TestResult<MetricsOwner> {
        Ok(MetricsOwner::assume(
            MetricsOwnerCapability::in_fixture_boundary(
                orchestrator_app::fixture_production_boundary(&self.root)?,
            )?,
        )?)
    }

    /// Creates `<root>/.alluka` and lets the oracle build the `learnings.db`
    /// schema by ingesting the seeded docs tree.
    ///
    /// The schema is Go's — `GoLearningAppender` is additive-only and never
    /// creates a store — so both the Go-write and the Rust-write learnings
    /// cells start from a Go-created file. The difference between them is which
    /// side put the *rows* in, which is what the cells compare.
    fn go_ingest(&self, documents: &[(&str, &str)]) -> TestResult<String> {
        fs::create_dir_all(self.path().join(".alluka"))?;
        let docs = self.path().join("nanika/docs");
        fs::create_dir_all(&docs)?;
        for (name, body) in documents {
            fs::write(docs.join(name), body.as_bytes())?;
        }
        let (exit, stdout, stderr) = self.go_home(&["ingest", "docs", "docs"])?;
        if exit != Some(0) {
            return Err(format!("go ingest exited {exit:?}: {stdout}{stderr}").into());
        }
        Ok(stdout)
    }
}

/// A checkpoint document in Go's own shape, written by hand rather than by
/// `encode_current_checkpoint`, so the Go-writer cells owe nothing to the Rust
/// codec for their input.
fn go_shaped_checkpoint(mission: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "version": 1,
        "payload": {
            "version": 2,
            "workspace_id": mission,
            "domain": "dev",
            "status": "completed",
            "started_at": AT,
            "plan": {
                "id": format!("plan-{mission}"),
                "task": "fixture task",
                "execution_mode": "sequential",
                "decomp_source": "predecomposed",
                "created_at": AT,
                "phases": [
                    {
                        "id": "phase-1",
                        "name": "build",
                        "objective": "compile the crate",
                        "persona": "senior-backend-engineer",
                        "model_tier": "work",
                        "runtime": "codex",
                        "status": "completed",
                    },
                    {
                        "id": "phase-2",
                        "name": "verify",
                        "objective": "run the gates",
                        "persona": "senior-backend-engineer",
                        "model_tier": "work",
                        "runtime": "codex",
                        "dependencies": ["phase-1"],
                        "status": "completed",
                    },
                ],
            },
        },
    }))
    .unwrap_or_default()
}

/// The same plan as a Rust `CheckpointProjection`.
fn rust_projection(mission: &str) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: mission.to_owned(),
        domain: "dev".to_owned(),
        status: "completed".to_owned(),
        started_at: AT.to_owned(),
        plan: Some(CheckpointPlan {
            id: format!("plan-{mission}"),
            task: "fixture task".to_owned(),
            execution_mode: "sequential".to_owned(),
            decomp_source: "predecomposed".to_owned(),
            created_at: AT.to_owned(),
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
                    persona: "senior-backend-engineer".to_owned(),
                    model_tier: "work".to_owned(),
                    runtime: "codex".to_owned(),
                    dependencies: vec!["phase-1".to_owned()],
                    status: "completed".to_owned(),
                    ..CheckpointPhase::default()
                },
            ],
            ..CheckpointPlan::default()
        }),
        ..CheckpointProjection::default()
    }
}

/// The ordered `(id, status)` phase rows a checkpoint document carries.
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

/// Two canonical events in Go's envelope shape.
fn rust_events(mission: &str) -> TestResult<Vec<EventRecord>> {
    let mut started = EventJsonMap::default();
    started.insert("phases".to_owned(), serde_json::Value::from(2_i64));
    started.insert(
        "task".to_owned(),
        serde_json::Value::String("fixture task".to_owned()),
    );
    let mut completed = EventJsonMap::default();
    completed.insert("phase_count".to_owned(), serde_json::Value::from(2_i64));
    Ok(vec![
        EventRecord {
            id: "evt_b5bidi0000000001".to_owned(),
            event_type: "mission.started".to_owned(),
            timestamp: AT.to_owned(),
            sequence: 1,
            mission_id: mission.to_owned(),
            phase_id: None,
            worker_id: None,
            data: Some(started),
            extra: EventJsonMap::default(),
        },
        EventRecord {
            id: "evt_b5bidi0000000002".to_owned(),
            event_type: "mission.completed".to_owned(),
            timestamp: AT.to_owned(),
            sequence: 2,
            mission_id: mission.to_owned(),
            phase_id: None,
            worker_id: None,
            data: Some(completed),
            extra: EventJsonMap::default(),
        },
    ])
}

/// Writes the Rust-encoded events as a Go-shaped JSONL log and returns the
/// exact bytes on disk.
fn write_rust_event_log(bridge: &Bridge, mission: &str) -> TestResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in rust_events(mission)? {
        bytes.extend_from_slice(&encode_current_event(&record)?);
        bytes.push(b'\n');
    }
    let path = bridge.event_log(mission);
    fs::create_dir_all(path.parent().ok_or("event log has no parent")?)?;
    fs::write(&path, &bytes)?;
    Ok(bytes)
}

/// Records two phase rows and one terminal row through the Rust port's sole
/// `metrics.db` writer.
fn write_rust_metrics(bridge: &Bridge, mission: &str) -> TestResult {
    let owner = bridge.metrics_owner()?;
    let identifier = MissionId::new(mission)?;
    for (phase, status) in [
        ("phase-1", PhaseStatus::Completed),
        ("phase-2", PhaseStatus::Completed),
    ] {
        let mut intent = PhaseMetricIntent::new(identifier.clone(), phase, 1, reaped_witness()?);
        intent.status = status;
        intent.gate_passed = true;
        owner.record_phase(&intent)?;
    }
    owner.record_terminal(&TerminalMetricIntent {
        mission: mission.to_owned(),
        domain: "dev".to_owned(),
        task: "fixture task".to_owned(),
        started_at: AT.to_owned(),
        finished_at: AT.to_owned(),
        duration_s: 0,
        status: "success".to_owned(),
        decomp_source: "predecomposed".to_owned(),
    })?;
    Ok(())
}

/// An ordered dump of the Go-shaped `missions` and `phases` rows this gate
/// compares. Row *counts* are never accepted on their own.
fn metrics_rows(root: &Path) -> TestResult<Vec<String>> {
    let database = root.join("metrics.db");
    // Read-only over an `immutable=1` URI, the same door
    // `GoLearningAdapter::read_only` uses. Go leaves `metrics.db` in WAL mode
    // and removes `-wal`/`-shm` on exit, so a plain `SQLITE_OPEN_READ_ONLY`
    // connection cannot create the shared-memory index and fails on the first
    // `prepare`. This gate used to open read-write to get past that, which
    // changed no row but did rewrite the file the cell is about to hash; the
    // immutable open creates nothing and leaves every byte alone.
    let connection = open_immutable(&database)?;
    let mut rows = Vec::new();
    let mut missions = connection.prepare(
        "SELECT id, domain, task, phases_total, phases_completed, phases_failed, status, \
         decomp_source FROM missions ORDER BY id",
    )?;
    let mut cursor = missions.query([])?;
    while let Some(row) = cursor.next()? {
        rows.push(format!(
            "mission|{}|{}|{}|{}|{}|{}|{}|{}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ));
    }
    drop(cursor);
    drop(missions);
    let mut phases = connection
        .prepare("SELECT id, mission_id, name, persona, status FROM phases ORDER BY id")?;
    let mut cursor = phases.query([])?;
    while let Some(row) = cursor.next()? {
        rows.push(format!(
            "phase|{}|{}|{}|{}|{}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ));
    }
    Ok(rows)
}

// ===========================================================================
// The reaped-group witness `PhaseMetricIntent::new` requires
// ===========================================================================

/// Re-exec entry point. Inert unless [`HELPER_MODE`] is set, which only
/// [`reaped_witness`] does.
#[test]
fn helper_entrypoint() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    if mode == "hold" {
        println!("ready");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    std::process::exit(0);
}

fn reaped_witness() -> TestResult<ExactProcessGroupAbsence> {
    let (child, identity) = live_group()?;
    reap(child)?;
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Ok(absence),
        other => Err(format!("expected a reaped group, observed {other:?}").into()),
    }
}

fn live_group() -> TestResult<(Child, KernelProcessIdentity)> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "helper_entrypoint", "--nocapture"])
        .env(HELPER_MODE, "hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or("helper stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err("helper exited before announcing readiness".into());
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
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    Ok((child, identity))
}

fn reap(mut child: Child) -> TestResult {
    drop(child.stdin.take());
    child.wait()?;
    Ok(())
}

// ===========================================================================
// checkpoint
// ===========================================================================

fn checkpoint_go_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "checkpoint-gg")?;
    let mission = "bidi-ckpt-gg";
    bridge.seed_terminal_workspace(mission)?;

    // Before the resume there is no event log, so `status` counts completed
    // phases from the checkpoint's own plan (`internal/cmd/status.go`'s
    // "no event log — fall back to checkpoint-derived phase counts" branch).
    let (exit, before, stderr) = bridge.go_config(&["status"])?;
    if exit != Some(0) {
        return Err(format!("go status exited {exit:?}: {before}{stderr}").into());
    }
    for expected in [mission, "2/2 phases", "[completed]"] {
        if !before.contains(expected) {
            return Err(format!("go status did not report {expected:?}: {before}").into());
        }
    }

    bridge.go_resume(mission)?;
    let written = fs::read(bridge.workspace(mission).join("checkpoint.json"))?;

    // The resume rewrote the checkpoint with both phases still terminal.
    let rows = phase_rows(&written)?;
    let expected_rows = vec![
        ("phase-1".to_owned(), "completed".to_owned()),
        ("phase-2".to_owned(), "completed".to_owned()),
    ];
    if rows != expected_rows {
        return Err(format!("go's rewrite produced phase rows {rows:?}").into());
    }

    // And now `status` reads the *event projection* instead, because an event
    // log exists. The resume of an already-terminal plan dispatches no phase,
    // so it emits only `mission.started`/`mission.completed` and the
    // projection reports zero completed phases. Pinned rather than smoothed
    // over: this is Go reading Go, and the two counters genuinely disagree.
    let (exit, after, stderr) = bridge.go_config(&["status"])?;
    if exit != Some(0) {
        return Err(format!("go status exited {exit:?}: {after}{stderr}").into());
    }
    for expected in [mission, "0/2 phases", "[completed]"] {
        if !after.contains(expected) {
            return Err(format!(
                "go status after its own resume did not report {expected:?}: {after}"
            )
            .into());
        }
    }
    Ok(format!(
        "go rewrote {} checkpoint bytes; status reads 2/2 from the checkpoint before the resume \
         and 0/2 from the event projection after it",
        written.len()
    ))
}

fn checkpoint_go_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "checkpoint-gr")?;
    let mission = "bidi-ckpt-gr";
    bridge.seed_terminal_workspace(mission)?;
    bridge.go_resume(mission)?;
    let written = fs::read(bridge.workspace(mission).join("checkpoint.json"))?;

    let decoded = decode_checkpoint(&written)?;
    if decoded.projection.workspace_id != mission {
        return Err(format!(
            "rust read workspace_id {:?} from the go-written checkpoint",
            decoded.projection.workspace_id
        )
        .into());
    }
    if decoded.projection.status != "completed" {
        return Err(format!(
            "rust read status {:?} from the go-written checkpoint",
            decoded.projection.status
        )
        .into());
    }
    let rows = phase_rows(&written)?;
    let expected = vec![
        ("phase-1".to_owned(), "completed".to_owned()),
        ("phase-2".to_owned(), "completed".to_owned()),
    ];
    if rows != expected {
        return Err(format!("rust read phase rows {rows:?}, expected {expected:?}").into());
    }
    Ok(format!(
        "rust decoded go's {} byte checkpoint",
        written.len()
    ))
}

fn checkpoint_rust_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "checkpoint-rg")?;
    let mission = "bidi-ckpt-rg";
    let workspace = bridge.workspace(mission);
    fs::create_dir_all(&workspace)?;
    fs::write(
        workspace.join("mission.md"),
        b"PHASE: build | OBJECTIVE: compile the crate\n\
          PHASE: verify | OBJECTIVE: run the gates | DEPENDS: build\n",
    )?;
    let encoded = encode_current_checkpoint(&rust_projection(mission))?;
    fs::write(workspace.join("checkpoint.json"), &encoded)?;

    let (exit, stdout, stderr) = bridge.go_config(&["status"])?;
    if exit != Some(0) {
        return Err(format!("go status exited {exit:?}: {stdout}{stderr}").into());
    }
    for expected in [mission, "2/2 phases", "[completed]"] {
        if !stdout.contains(expected) {
            return Err(format!(
                "go status did not report {expected:?} for a rust checkpoint: {stdout}"
            )
            .into());
        }
    }
    let resumed = bridge.go_resume(mission)?;
    if !resumed.contains("2 completed, 0 failed") {
        return Err(format!("go's continuation changed the phase outcomes: {resumed}").into());
    }
    Ok(format!(
        "go read and continued rust's {} byte checkpoint",
        encoded.len()
    ))
}

fn checkpoint_rust_rust(_go: &Path) -> TestResult<String> {
    let mission = "bidi-ckpt-rr";
    let encoded = encode_current_checkpoint(&rust_projection(mission))?;
    let decoded = decode_checkpoint(&encoded)?;
    let reencoded = encode_current_checkpoint(&decoded.projection)?;
    if reencoded != encoded {
        return Err(format!(
            "the checkpoint round trip is not byte-identical: {} bytes in, {} bytes out",
            encoded.len(),
            reencoded.len()
        )
        .into());
    }
    let rows = phase_rows(&encoded)?;
    let expected = vec![
        ("phase-1".to_owned(), "completed".to_owned()),
        ("phase-2".to_owned(), "completed".to_owned()),
    ];
    if rows != expected {
        return Err(format!("rust round trip produced phase rows {rows:?}").into());
    }
    Ok(format!(
        "encode/decode/re-encode is byte-identical over {} bytes",
        encoded.len()
    ))
}

// ===========================================================================
// event-log
// ===========================================================================

fn event_log_go_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "events-gg")?;
    let mission = "bidi-evt-gg";
    bridge.seed_terminal_workspace(mission)?;
    bridge.go_resume(mission)?;
    let written = fs::read(bridge.event_log(mission))?;

    let (exit, stdout, stderr) = bridge.go_config(&["events", "replay", mission])?;
    if exit != Some(0) {
        return Err(format!("go events replay exited {exit:?}: {stdout}{stderr}").into());
    }
    for expected in ["mission.started", "mission.completed"] {
        if !stdout.contains(expected) {
            return Err(format!("go replay did not print {expected:?}: {stdout}").into());
        }
    }
    let (exit, listed, stderr) = bridge.go_config(&["events", "list"])?;
    if exit != Some(0) || !listed.contains(mission) {
        return Err(format!("go events list exited {exit:?}: {listed}{stderr}").into());
    }
    Ok(format!("go wrote and replayed {} log bytes", written.len()))
}

fn event_log_go_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "events-gr")?;
    let mission = "bidi-evt-gr";
    bridge.seed_terminal_workspace(mission)?;
    bridge.go_resume(mission)?;
    let written = fs::read(bridge.event_log(mission))?;

    let scan = scan_event_log(&written);
    if !scan.diagnostics.is_empty() {
        return Err(format!(
            "rust found {} diagnostics in go's log: {:?}",
            scan.diagnostics.len(),
            scan.diagnostics
        )
        .into());
    }
    let types: Vec<String> = scan
        .events
        .iter()
        .map(|event| event.record.event_type.clone())
        .collect();
    let expected = vec!["mission.started".to_owned(), "mission.completed".to_owned()];
    if types != expected {
        return Err(format!("rust scanned go's log as {types:?}, expected {expected:?}").into());
    }
    for event in &scan.events {
        if event.record.mission_id != mission {
            return Err(format!(
                "rust read mission_id {:?} from go's log",
                event.record.mission_id
            )
            .into());
        }
    }
    Ok(format!(
        "rust scanned {} go-written events with 0 diagnostics",
        scan.events.len()
    ))
}

fn event_log_rust_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "events-rg")?;
    let mission = "bidi-evt-rg";
    let bytes = write_rust_event_log(&bridge, mission)?;

    let (exit, stdout, stderr) = bridge.go_config(&["events", "replay", mission])?;
    if exit != Some(0) {
        return Err(format!("go replay of a rust log exited {exit:?}: {stdout}{stderr}").into());
    }
    let started = stdout
        .find("mission.started")
        .ok_or("go replay did not print mission.started for a rust log")?;
    let completed = stdout
        .find("mission.completed")
        .ok_or("go replay did not print mission.completed for a rust log")?;
    if started >= completed {
        return Err(format!("go replayed the rust log out of order: {stdout}").into());
    }
    if fs::read(bridge.event_log(mission))? != bytes {
        return Err("go's replay modified the rust-written log".into());
    }
    Ok(format!(
        "go replayed {} rust-encoded log bytes in order and left them unchanged",
        bytes.len()
    ))
}

fn event_log_rust_rust(_go: &Path) -> TestResult<String> {
    let mission = "bidi-evt-rr";
    let records = rust_events(mission)?;
    let mut bytes = Vec::new();
    let mut lines = Vec::new();
    for record in &records {
        let encoded = encode_current_event(record)?;
        bytes.extend_from_slice(&encoded);
        bytes.push(b'\n');
        lines.push(encoded);
    }
    let scan = scan_event_log(&bytes);
    if !scan.diagnostics.is_empty() {
        return Err(format!(
            "rust round trip produced diagnostics: {:?}",
            scan.diagnostics
        )
        .into());
    }
    if scan.events.len() != records.len() {
        return Err(format!(
            "rust scanned {} of {} encoded events",
            scan.events.len(),
            records.len()
        )
        .into());
    }
    for (index, event) in scan.events.iter().enumerate() {
        // `scan_event_log` keeps the line terminator in `raw_line`, so the
        // comparison is against the encoded line plus its `\n` — the exact
        // bytes the writer put on disk.
        let mut framed = lines[index].clone();
        framed.push(b'\n');
        if event.raw_line != framed {
            return Err(format!("event {index} did not round trip byte-identically").into());
        }
        if event.record != records[index] {
            return Err(format!("event {index} did not round trip field for field").into());
        }
    }
    Ok(format!(
        "{} events round tripped byte-identically",
        records.len()
    ))
}

// ===========================================================================
// metrics-db
// ===========================================================================

fn metrics_go_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "metrics-gg")?;
    let mission = "bidi-met-gg";
    bridge.seed_terminal_workspace(mission)?;
    bridge.go_resume(mission)?;
    let rows = metrics_rows(bridge.path())?;

    let (exit, stdout, stderr) = bridge.go_config(&["metrics"])?;
    if exit != Some(0) {
        return Err(format!("go metrics exited {exit:?}: {stdout}{stderr}").into());
    }
    if !stdout.contains(mission) {
        return Err(format!("go metrics did not list its own mission: {stdout}").into());
    }
    let (exit, phases, stderr) = bridge.go_config(&["metrics", "phases", mission])?;
    if exit != Some(0) {
        return Err(format!("go metrics phases exited {exit:?}: {phases}{stderr}").into());
    }
    for expected in ["build", "verify"] {
        if !phases.contains(expected) {
            return Err(format!("go metrics phases omitted {expected:?}: {phases}").into());
        }
    }
    Ok(format!(
        "go wrote and read {} ordered metrics rows",
        rows.len()
    ))
}

fn metrics_go_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "metrics-gr")?;
    let mission = "bidi-met-gr";
    bridge.seed_terminal_workspace(mission)?;
    bridge.go_resume(mission)?;
    let before = metrics_rows(bridge.path())?;

    let owner = bridge.metrics_owner()?;
    let totals = owner
        .mission_totals(mission)?
        .ok_or("rust read no mission row from go's metrics.db")?;
    if totals.phases_total != 2 || totals.phases_completed != 2 {
        return Err(format!(
            "rust read totals {}/{} from go's metrics.db",
            totals.phases_completed, totals.phases_total
        )
        .into());
    }
    let mut names = owner.recorded_phase_names(mission)?;
    names.sort();
    if names != vec!["build".to_owned(), "verify".to_owned()] {
        return Err(format!("rust read phase names {names:?} from go's metrics.db").into());
    }
    drop(owner);

    // The Rust reader opens the store read-write to apply Go's own additive
    // migrations, so the claim that matters is that no Go-written row moved.
    let after = metrics_rows(bridge.path())?;
    if after != before {
        return Err(format!(
            "the rust read changed go's rows:\nbefore {before:#?}\nafter  {after:#?}"
        )
        .into());
    }
    Ok(format!(
        "rust read {} go-written rows and left every one unchanged",
        before.len()
    ))
}

fn metrics_rust_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "metrics-rg")?;
    let mission = "bidi-met-rg";
    write_rust_metrics(&bridge, mission)?;
    let rows = metrics_rows(bridge.path())?;

    let (exit, stdout, stderr) = bridge.go_config(&["metrics"])?;
    if exit != Some(0) {
        return Err(
            format!("go metrics over a rust store exited {exit:?}: {stdout}{stderr}").into(),
        );
    }
    if !stdout.contains(mission) {
        return Err(format!("go metrics did not list the rust-written mission: {stdout}").into());
    }
    let (exit, phases, stderr) = bridge.go_config(&["metrics", "phases", mission])?;
    if exit != Some(0) {
        return Err(format!("go metrics phases exited {exit:?}: {phases}{stderr}").into());
    }
    for expected in ["phase-1", "phase-2"] {
        if !phases.contains(expected) {
            return Err(
                format!("go metrics phases omitted the rust phase {expected:?}: {phases}").into(),
            );
        }
    }
    let after = metrics_rows(bridge.path())?;
    if after != rows {
        return Err("go's read changed the rust-written rows".into());
    }
    Ok(format!(
        "go read {} rust-written rows and left every one unchanged",
        rows.len()
    ))
}

fn metrics_rust_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "metrics-rr")?;
    let mission = "bidi-met-rr";
    write_rust_metrics(&bridge, mission)?;
    let owner = bridge.metrics_owner()?;
    let totals = owner
        .mission_totals(mission)?
        .ok_or("rust read back no mission row it had written")?;
    if totals.phases_total != 2 || totals.phases_completed != 2 {
        return Err(format!(
            "rust read back totals {}/{}",
            totals.phases_completed, totals.phases_total
        )
        .into());
    }
    let mut names = owner.recorded_phase_names(mission)?;
    names.sort();
    if names != vec!["phase-1".to_owned(), "phase-2".to_owned()] {
        return Err(format!("rust read back phase names {names:?}").into());
    }
    drop(owner);
    let rows = metrics_rows(bridge.path())?;
    Ok(format!("{} rows written and read back by rust", rows.len()))
}

// ===========================================================================
// learnings-db
// ===========================================================================

const GO_DOCUMENT: (&str, &str) = (
    "go-written.md",
    "# A go-written note\n\nThe oracle ingests this into learnings.db.\n",
);

/// Opens one SQLite file read-only over an `immutable=1` `file:` URI.
///
/// Go opens its stores in WAL mode and removes `-wal`/`-shm` on a clean exit,
/// so a plain read-only connection cannot create the shared-memory index a WAL
/// database needs and fails on the first statement. `immutable=1` reads the
/// main database directly and creates nothing beside it, which is what lets
/// this gate hash a Go artifact before and after the Rust read and require the
/// two to be equal. It mirrors `GoLearningAdapter::read_only`'s fallback; the
/// port is what is under test, so the gate does not borrow its code.
fn open_immutable(database: &Path) -> TestResult<rusqlite::Connection> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let raw = database
        .to_str()
        .ok_or("the fixture database path is not UTF-8")?;
    let mut uri = String::from("file:");
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(byte));
            }
            _ => {
                uri.push('%');
                uri.push(char::from(HEX[usize::from(byte >> 4)]));
                uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
    }
    uri.push_str("?immutable=1");
    Ok(rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?)
}

fn learnings_database(bridge: &Bridge) -> PathBuf {
    bridge.path().join(".alluka/learnings.db")
}

/// The Go store's on-disk journal mode, read out of the file header.
///
/// `PRAGMA journal_mode` cannot answer this: over an `immutable=1` connection
/// it reports the *connection's* mode ("delete"), not the file's, and a
/// read-write connection would rewrite the artifact this cell is about. Bytes
/// 18 and 19 of the SQLite header are the file-format write and read versions,
/// and `2` in both is exactly "this database is in WAL mode"
/// (<https://sqlite.org/fileformat2.html> §1.3).
fn on_disk_journal_mode(database: &Path) -> TestResult<&'static str> {
    let header = fs::read(database)?;
    let (write_version, read_version) = header
        .get(18)
        .zip(header.get(19))
        .ok_or("the go-written store is shorter than one SQLite header")?;
    Ok(match (write_version, read_version) {
        (2, 2) => "wal",
        (1, 1) => "rollback-journal",
        _ => "unknown",
    })
}

/// An image of the Go store and both of its WAL sidecars.
///
/// The sidecars are part of the image on purpose: the repair's whole claim is
/// that reading creates neither, so a `-wal` that exists afterwards is as much
/// a failure as a changed main file.
fn store_image(database: &Path) -> TestResult<Vec<(String, Option<String>)>> {
    let mut image = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut path = database.to_path_buf().into_os_string();
        path.push(suffix);
        let path = PathBuf::from(path);
        image.push((
            suffix.to_owned(),
            match path.symlink_metadata() {
                Err(_) => None,
                Ok(_) => Some(sha256_file(&path)?),
            },
        ));
    }
    Ok(image)
}

/// Proves the Rust adapter reads a Go-written WAL store *and* leaves it alone.
///
/// This replaces the M4a-r2 census row's `refuse_then_normalise`, which pinned
/// the opposite: the adapter refused every Go-written store, and the gate
/// rewrote the journal mode to `delete` so the reader could be exercised at
/// all. That rewrite mutated the very artifact the cell is about. CF-M4a-3
/// repaired the adapter, so the cell now asserts the store is still in WAL
/// mode with no sidecars, reads it, and requires a byte-identical image.
fn read_go_store_without_touching_it(bridge: &Bridge) -> TestResult<String> {
    let database = learnings_database(bridge);
    let mode = on_disk_journal_mode(&database)?;
    if mode != "wal" {
        return Err(format!(
            "the go-written store is in {mode:?}, not wal; this cell no longer crosses the \
             journal-mode boundary it was written for"
        )
        .into());
    }
    let before = store_image(&database)?;
    if before[1].1.is_some() || before[2].1.is_some() {
        return Err(
            "go left a -wal or -shm sidecar behind; the quiescence guard would (correctly) \
                    refuse, and this cell is not exercising the sidecar-less case"
                .into(),
        );
    }

    let adapter = GoLearningAdapter::in_fixture(&bridge.root)?;
    let version = adapter.schema_version()?;

    let after = store_image(&database)?;
    if before != after {
        return Err("the rust adapter changed the go-written store while reading it".into());
    }
    Ok(format!("schema version {version}"))
}

fn learnings_go_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "learnings-gg")?;
    bridge.go_ingest(&[GO_DOCUMENT])?;

    let (exit, stdout, stderr) = bridge.go_home(&["stats"])?;
    if exit != Some(0) {
        return Err(format!("go stats exited {exit:?}: {stdout}{stderr}").into());
    }
    if !stdout.contains("learnings: 1 total") {
        return Err(format!("go stats did not count its own row: {stdout}").into());
    }
    Ok("go ingested one row and counted it".to_owned())
}

fn learnings_go_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "learnings-gr")?;
    bridge.go_ingest(&[GO_DOCUMENT])?;
    let read = read_go_store_without_touching_it(&bridge)?;
    let database = learnings_database(&bridge);
    let before = store_image(&database)?;

    let adapter = GoLearningAdapter::in_fixture(&bridge.root)?;
    let page = adapter.list(&LearningListQuery {
        domain: None,
        learning_type: None,
        include_archived: false,
        limit: 16,
    })?;
    if page.rows.len() != 1 {
        return Err(format!("rust read {} rows from go's store", page.rows.len()).into());
    }
    let row = &page.rows[0];
    if row.learning_type != "source" {
        return Err(format!("rust read learning type {:?}", row.learning_type).into());
    }
    if !row.content.contains("go-written note") {
        return Err(format!("rust read unexpected content {:?}", row.content).into());
    }
    let stats = adapter.stats()?;
    if stats.total != 1 {
        return Err(format!("rust read stats total {}", stats.total).into());
    }
    if before != store_image(&database)? {
        return Err("the rust adapter changed go's WAL store while listing it".into());
    }
    Ok(format!(
        "rust read row {:?} straight out of go's sidecar-less WAL store ({read}) and left every \
         byte of it, and of both absent sidecars, unchanged",
        row.id
    ))
}

fn learnings_rust_go(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "learnings-rg")?;
    // Go creates the store and its schema; `GoLearningAppender` is additive
    // only and never conjures a layout. The row below is Rust's. The appender
    // opens read-write, so it is unaffected by the WAL divergence above.
    bridge.go_ingest(&[GO_DOCUMENT])?;

    let grant = AdditiveFixtureGrant::in_fixture(&bridge.root)?;
    let appender = GoLearningAppender::for_grant(&grant)?;
    let receipt = appender.insert_new(
        &grant,
        &[NewLearningRow {
            id: "b5-bidi-rust-row".to_owned(),
            learning_type: "pattern".to_owned(),
            content: "the rust port appends learnings additively".to_owned(),
            context: String::new(),
            domain: "dev".to_owned(),
            created_at: AT.to_owned(),
        }],
    )?;
    if receipt.inserted_ids != vec!["b5-bidi-rust-row".to_owned()] {
        return Err(format!("rust inserted {:?}", receipt.inserted_ids).into());
    }

    let (exit, stdout, stderr) = bridge.go_home(&["stats"])?;
    if exit != Some(0) {
        return Err(format!("go stats over a rust row exited {exit:?}: {stdout}{stderr}").into());
    }
    if !stdout.contains("learnings: 2 total") {
        return Err(format!("go stats did not count the rust row: {stdout}").into());
    }
    Ok("go counted the rust-appended row".to_owned())
}

fn learnings_rust_rust(go: &Path) -> TestResult<String> {
    let bridge = Bridge::new(go, "learnings-rr")?;
    bridge.go_ingest(&[GO_DOCUMENT])?;
    read_go_store_without_touching_it(&bridge)?;

    let adapter = GoLearningAdapter::in_fixture(&bridge.root)?;
    let before = adapter.row_set_digest()?;
    let grant = AdditiveFixtureGrant::in_fixture(&bridge.root)?;
    let appender = GoLearningAppender::for_grant(&grant)?;
    appender.insert_new(
        &grant,
        &[NewLearningRow {
            id: "b5-bidi-rust-roundtrip".to_owned(),
            learning_type: "insight".to_owned(),
            content: "an additive insert grows the id set by exactly the declared id".to_owned(),
            context: String::new(),
            domain: "dev".to_owned(),
            created_at: AT.to_owned(),
        }],
    )?;
    let after = adapter.row_set_digest()?;
    let grew: Vec<&String> = after
        .ids()
        .iter()
        .filter(|id| !before.ids().contains(id))
        .collect();
    if grew != vec![&"b5-bidi-rust-roundtrip".to_owned()] {
        return Err(format!("the id set grew by {grew:?}").into());
    }
    for id in before.ids() {
        if !after.ids().contains(id) {
            return Err(format!("the additive insert lost pre-existing id {id:?}").into());
        }
    }
    Ok("the appended id set is exactly the declared id".to_owned())
}

// ===========================================================================
// The matrix
// ===========================================================================

type CellRunner = fn(&Path) -> TestResult<String>;

fn runners() -> Vec<(&'static str, Side, Side, CellRunner)> {
    vec![
        (
            "checkpoint",
            Side::Go,
            Side::Go,
            checkpoint_go_go as CellRunner,
        ),
        ("checkpoint", Side::Go, Side::Rust, checkpoint_go_rust),
        ("checkpoint", Side::Rust, Side::Go, checkpoint_rust_go),
        ("checkpoint", Side::Rust, Side::Rust, checkpoint_rust_rust),
        ("event-log", Side::Go, Side::Go, event_log_go_go),
        ("event-log", Side::Go, Side::Rust, event_log_go_rust),
        ("event-log", Side::Rust, Side::Go, event_log_rust_go),
        ("event-log", Side::Rust, Side::Rust, event_log_rust_rust),
        ("metrics-db", Side::Go, Side::Go, metrics_go_go),
        ("metrics-db", Side::Go, Side::Rust, metrics_go_rust),
        ("metrics-db", Side::Rust, Side::Go, metrics_rust_go),
        ("metrics-db", Side::Rust, Side::Rust, metrics_rust_rust),
        ("learnings-db", Side::Go, Side::Go, learnings_go_go),
        ("learnings-db", Side::Go, Side::Rust, learnings_go_rust),
        ("learnings-db", Side::Rust, Side::Go, learnings_rust_go),
        ("learnings-db", Side::Rust, Side::Rust, learnings_rust_rust),
    ]
}

#[test]
fn core_go_bidirectional_matrix() -> TestResult {
    // The oracle is mandatory: this `?` is what makes a missing, unbuildable
    // or mismatched Go binary a red gate rather than a quiet pass.
    let go = frozen_go_oracle()?;

    let runners = runners();
    let mut cells = Vec::new();
    for (artifact, writer, reader, runner) in runners {
        let outcome = runner(&go);
        let evidence = outcome.as_ref().map(String::clone).unwrap_or_default();
        cells.push(Cell::observed(artifact, writer, reader, outcome).with_evidence(evidence));
    }

    // Completeness: the census must contain every artifact crossed with every
    // pair of sides. A cell that was never enumerated is the failure mode this
    // gate exists to make impossible, so it is checked before the results are.
    let present: BTreeSet<String> = cells.iter().map(Cell::name).collect();
    let mut missing = Vec::new();
    for artifact in ARTIFACTS {
        for writer in SIDES {
            for reader in SIDES {
                let name = format!("{artifact}:{writer}->{reader}");
                if !present.contains(&name) {
                    missing.push(name);
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the bidirectional matrix has no case for {missing:?}; an artifact or direction with no \
         case is a failure, not an omission",
    );
    assert_eq!(
        present.len(),
        ARTIFACTS.len() * SIDES.len() * SIDES.len(),
        "the census has duplicate or extra cells: {present:?}",
    );

    // The census, printed under `--nocapture` so a run is readable evidence
    // rather than a bare "ok".
    for cell in &cells {
        println!(
            "{:<28} {:<12} {}",
            cell.name(),
            cell.observed.label(),
            cell.evidence
        );
    }

    let unavailable: Vec<String> = cells
        .iter()
        .filter(|cell| matches!(cell.observed, Census::Unavailable { .. }))
        .map(|cell| format!("{}: {}", cell.name(), cell.evidence))
        .collect();
    assert!(
        unavailable.is_empty(),
        "{} case(s) could not be observed at all:\n{}",
        unavailable.len(),
        unavailable.join("\n"),
    );

    // Every rust-only cell carries a normative reason, and every go-observed
    // cell does not need one — a cell that lost its reason would be an
    // unexplained absence of the oracle.
    for cell in &cells {
        match &cell.observed {
            Census::RustOnly { reason } => assert!(
                reason.len() > 80,
                "{} is rust-only without a normative reason",
                cell.name()
            ),
            Census::GoObserved => assert!(
                cell.writer == Side::Go || cell.reader == Side::Go,
                "{} is labelled go-observed with no Go side",
                cell.name()
            ),
            Census::Unavailable { .. } => unreachable!("checked above"),
        }
    }

    // Both directions per artifact are Go-observed. A one-directional pass is a
    // failure: this is the assertion that would catch a Rust→Go path that
    // works while the Go→Rust path silently does not.
    for artifact in ARTIFACTS {
        for (writer, reader) in [(Side::Go, Side::Rust), (Side::Rust, Side::Go)] {
            let name = format!("{artifact}:{writer}->{reader}");
            let cell = cells
                .iter()
                .find(|cell| cell.name() == name)
                .ok_or_else(|| format!("{name} is missing from the census"))?;
            assert_eq!(
                cell.observed.label(),
                "go-observed",
                "{name} must be witnessed by the Go oracle in both directions",
            );
        }
    }

    let go_observed = cells
        .iter()
        .filter(|cell| matches!(cell.observed, Census::GoObserved))
        .count();
    assert_eq!(
        go_observed, 12,
        "twelve of the sixteen cells have a Go side; observed {go_observed}",
    );
    Ok(())
}

// ===========================================================================
// The oracle contract, asserted directly
// ===========================================================================

/// A cached oracle that is not the in-tree build is refused, and the refusal
/// names both digests, both paths and how to regenerate.
///
/// This is TRK-1280's property: an installed binary was once built from
/// uncommitted `run.go`, so a binary that merely *exists* cannot stand in for
/// the committed tree. Asserted against the pure rule rather than by setting
/// `ORCHESTRATOR_ACCEPTED_GO_BIN`, which would race the matrix test.
#[test]
fn a_mismatched_cached_oracle_is_refused_naming_both_digests() -> TestResult {
    let manifest = frozen_tree_manifest();
    let built = workspace_root().join("target/go-oracle/orchestrator");
    // A real, readable file that is certainly not the in-tree Go build.
    let decoy = std::env::current_exe()?;
    let reference = "0".repeat(64);

    let Err(message) = accept_cached_oracle(&decoy, &built, &reference, &manifest) else {
        return Err("a decoy binary was accepted as the frozen Go oracle".into());
    };
    for expected in ["has sha256", &reference, "tests/build-go-oracle.sh"] {
        assert!(
            message.contains(expected),
            "the refusal did not name {expected:?}: {message}",
        );
    }
    assert!(
        message.contains(&decoy.display().to_string()),
        "the refusal did not name the rejected candidate: {message}",
    );
    assert!(
        message.contains(&built.display().to_string()),
        "the refusal did not name the in-tree build: {message}",
    );
    Ok(())
}

/// A cached oracle that does not exist is a hard failure naming the manifest
/// and the in-tree build — never a skip.
#[test]
fn an_absent_cached_oracle_is_a_hard_failure_naming_what_was_checked() -> TestResult {
    let manifest = frozen_tree_manifest();
    let built = workspace_root().join("target/go-oracle/orchestrator");
    let absent = Path::new("/nonexistent/orchestrator-go-oracle");

    let Err(message) = accept_cached_oracle(absent, &built, "unused", &manifest) else {
        return Err("an absent cached oracle was accepted".into());
    };
    for expected in [
        "does not name a file",
        &built.display().to_string(),
        &manifest.display().to_string(),
        &absent.display().to_string(),
    ] {
        assert!(
            message.contains(expected),
            "the refusal did not name {expected:?}: {message}",
        );
    }
    Ok(())
}
